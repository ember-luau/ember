/*! the editor files that install keeps in step with the packages folders.

each folder that holds links gets a require alias in `.luaurc` or
`.config.luau`, so `require("@packages/core")` works in luau-lsp and in the
runtimes. the `.d.luau` files of the installed packages go into
`luau-lsp.types.definitionFiles` in `.vscode/settings.json`.

embr keeps no record of what it wrote. an alias is embr's when its name is
one that embr gives a folder, and a definition entry is embr's when its path
goes into a package store. so each install reads the files again and changes
only those entries. everything else in the files stays as the user wrote it. */

use super::STORE_DIR;
use crate::project::lockfile::LockedPackage;
use crate::project::manifest::{self, Environment, Manifest, RequireStyle};
use crate::registry::index::DownloadSource;
use crate::registry::pack;
use full_moon::ast::punctuated::Pair;
use full_moon::ast::{Expression, Field, LastStmt, TableConstructor};
use full_moon::node::Node;
use full_moon::tokenizer::TokenType;
use jsonc_parser::cst::{CstInputValue, CstRootNode};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const LUAURC: &str = ".luaurc";
const CONFIG_LUAU: &str = ".config.luau";
const SETTINGS: &str = ".vscode/settings.json";
const DEFINITIONS_SETTING: &str = "luau-lsp.types.definitionFiles";
/// how many `.d.luau` files a package gives without a `definitions` list, Crater's cap.
const DEFINITIONS_CAP: usize = 10;

/// each alias name that embr owns, and the path it must hold. None means its folder holds no links.
type Aliases = BTreeMap<String, Option<String>>;

/** brings the aliases and the definition files in `dir` in step with
`packages`, what the install put in place. a file that embr cannot edit
safely stays as it is, with a warning, and the install goes on. */
pub(super) fn sync(
    manifest: &Manifest,
    dir: &Path,
    packages: &[LockedPackage],
    warn: &mut impl FnMut(String),
) {
    if manifest.config.aliases != Some(false)
        && let Err(message) = sync_aliases(manifest, dir, packages)
    {
        warn(format!("warning: {message}"));
    }
    if manifest.config.definition_files != Some(false)
        && let Err(reason) = sync_definitions(manifest, dir, packages)
    {
        warn(format!(
            "warning: embr does not change {SETTINGS}: {reason}"
        ));
    }
}

/** the alias that embr gives the folder of `environment`. the project's own
environment gets `packages`, the name the docs give. each other one gets
`<folder>-packages`, after its `[config]` key, because Luau allows no `/`
in an alias name. */
fn alias_name(manifest: &Manifest, environment: Environment) -> String {
    match manifest.target() {
        Some(target) if target.environment == environment => "packages".to_string(),
        _ => format!("{}-packages", environment.dir_name()),
    }
}

/** the aliases for the folders that hold links. an instance-require tree
gets none: its code gets to packages through `script`, never by string. */
fn wanted_aliases(manifest: &Manifest, packages: &[LockedPackage]) -> Aliases {
    Environment::ALL
        .into_iter()
        .map(|environment| {
            let linked = manifest.require_style(environment) == RequireStyle::String
                && packages
                    .iter()
                    .any(|package| package.link.is_some() && package.context() == environment);
            let path = Path::new(".").join(manifest.packages_out(environment));
            (
                alias_name(manifest, environment),
                linked.then(|| slashed(&path)),
            )
        })
        .collect()
}

fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

enum Edit {
    Set(String),
    Remove,
}

/** what one alias that embr owns needs. a folder with links sets its path.
without links the alias goes, but only while its path gets to no folder: an
alias that gets to a real folder can be the user's own. */
fn alias_edit(wanted: Option<&str>, current: Option<&str>, dir: &Path) -> Option<Edit> {
    match (wanted, current) {
        (Some(wanted), Some(current)) if wanted == current => None,
        (Some(wanted), _) => Some(Edit::Set(wanted.to_string())),
        (None, Some(current)) if !dir.join(current).is_dir() => Some(Edit::Remove),
        (None, _) => None,
    }
}

/** writes the aliases to the config file of `dir`: `.config.luau` when the
project has one, else `.luaurc`, which is made only when an alias needs it.
the error is the whole warning. */
fn sync_aliases(manifest: &Manifest, dir: &Path, packages: &[LockedPackage]) -> Result<(), String> {
    let luaurc = dir.join(LUAURC);
    let config = dir.join(CONFIG_LUAU);
    let (name, path, edit): (&str, &Path, fn(&str, &Aliases, &Path) -> _) = match (
        luaurc.exists(),
        config.exists(),
    ) {
        /* Luau refuses a folder with both files as ambiguous, so no
        alias in either one can work */
        (true, true) => {
            return Err(format!(
                "this folder has both {LUAURC} and {CONFIG_LUAU}, and Luau refuses that, so embr writes no aliases"
            ));
        }
        (false, true) => (CONFIG_LUAU, &config, edit_config_luau),
        _ => (LUAURC, &luaurc, edit_luaurc),
    };
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("embr cannot read {name}: {error}")),
    };
    match edit(&text, &wanted_aliases(manifest, packages), dir) {
        Ok(Some(new)) => {
            fs::write(path, new).map_err(|error| format!("embr cannot write {name}: {error}"))
        }
        Ok(None) => Ok(()),
        Err(reason) => Err(format!("embr does not change {name}: {reason}")),
    }
}

/** the `.luaurc` text with embr's aliases in step, None when nothing
changes. the CST keeps comments, trailing commas, and every other key. */
fn edit_luaurc(text: &str, wanted: &Aliases, dir: &Path) -> Result<Option<String>, String> {
    let root = CstRootNode::parse(text, &Default::default()).map_err(|error| error.to_string())?;
    let config = root
        .object_value_or_create()
        .ok_or("its top-level value is not an object")?;
    let mut changed = false;
    for (name, wanted) in wanted {
        let aliases = match config.get("aliases") {
            Some(property) => Some(
                property
                    .object_value()
                    .ok_or("`aliases` is not an object")?,
            ),
            None => None,
        };
        // Luau reads alias names without case
        let property = aliases
            .iter()
            .flat_map(|aliases| aliases.properties())
            .find(|property| {
                property
                    .name()
                    .and_then(|key| key.decoded_value().ok())
                    .is_some_and(|key| key.eq_ignore_ascii_case(name))
            });
        let current = property
            .as_ref()
            .and_then(|property| property.value()?.as_string_lit()?.decoded_value().ok());
        match (
            alias_edit(wanted.as_deref(), current.as_deref(), dir),
            property,
        ) {
            (None, _) | (Some(Edit::Remove), None) => continue,
            (Some(Edit::Set(path)), Some(property)) => {
                property.set_value(CstInputValue::String(path))
            }
            (Some(Edit::Set(path)), None) => {
                let aliases = config
                    .object_value_or_create("aliases")
                    .ok_or("`aliases` is not an object")?;
                aliases.append(name, CstInputValue::String(path));
            }
            (Some(Edit::Remove), Some(property)) => property.remove(),
        }
        changed = true;
    }
    Ok(changed.then(|| with_newline(text, root.to_string())))
}

/// a file that embr makes from nothing ends in a newline, as editors write them.
fn with_newline(old: &str, mut new: String) -> String {
    if old.trim().is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new
}

/// a byte range of the source and the text that takes its place.
struct Splice {
    start: usize,
    end: usize,
    text: String,
}

/** the `.config.luau` text with embr's aliases in step, None when nothing
changes. it edits the `luau.aliases` table in place and keeps every other
byte. a shape that it cannot read, such as a computed table, is an error,
and the file stays as it is.

each round makes one kind of edit, so no two splices of a round overlap:
new values and removals first, then new fields. the third round must find
nothing to do. that also proves the new text parses and holds the aliases,
so a bad edit never replaces the file. */
fn edit_config_luau(source: &str, wanted: &Aliases, dir: &Path) -> Result<Option<String>, String> {
    let mut text = source.to_string();
    for round in 0..3 {
        let mut splices = config_luau_splices(&text, wanted, dir).map_err(|reason| {
            if round == 0 {
                reason
            } else {
                "embr cannot write its aliases there safely".to_string()
            }
        })?;
        if splices.is_empty() {
            return Ok((text != source).then_some(text));
        }
        splices.sort_by_key(|splice| std::cmp::Reverse(splice.start));
        for splice in splices {
            text.replace_range(splice.start..splice.end, &splice.text);
        }
    }
    Err("embr cannot write its aliases there safely".to_string())
}

/// the edits one round makes, see [`edit_config_luau`].
fn config_luau_splices(source: &str, wanted: &Aliases, dir: &Path) -> Result<Vec<Splice>, String> {
    const SHAPE: &str = "it does not end with `return { ... }`";
    let ast = full_moon::parse(source).map_err(|_| "it does not parse".to_string())?;
    let root = match ast.nodes().last_stmt() {
        Some(LastStmt::Return(statement)) => {
            match statement.returns().iter().collect::<Vec<_>>()[..] {
                [Expression::TableConstructor(table)] => table,
                _ => return Err(SHAPE.to_string()),
            }
        }
        _ => return Err(SHAPE.to_string()),
    };
    let luau = table_field(root, "luau")?;
    let aliases = match luau {
        Some(luau) => table_field(luau, "aliases")?,
        None => None,
    };

    let mut splices = Vec::new();
    let mut added = Vec::new();
    for (name, wanted) in wanted {
        // Luau reads alias names without case
        let pair = aliases.and_then(|aliases| {
            aliases.fields().pairs().find(|pair| {
                field_key(pair.value()).is_some_and(|key| key.eq_ignore_ascii_case(name))
            })
        });
        let value = pair.and_then(|pair| field_value(pair.value()));
        match (
            alias_edit(wanted.as_deref(), value.and_then(literal), dir),
            pair,
            value,
        ) {
            (Some(Edit::Set(path)), _, Some(value)) => splices.push(Splice {
                start: start(value),
                end: end(value),
                text: format!("\"{path}\""),
            }),
            (Some(Edit::Set(path)), _, None) => {
                added.push(format!("{} = \"{path}\"", key_text(name)))
            }
            (Some(Edit::Remove), Some(pair), _) => splices.push(removal(source, pair)),
            _ => {}
        }
    }
    if !splices.is_empty() || added.is_empty() {
        return Ok(splices);
    }
    let fields = added.join(", ");
    Ok(vec![match (luau, aliases) {
        (_, Some(aliases)) => append(source, aliases, &added),
        (Some(luau), None) => append(source, luau, &[format!("aliases = {{ {fields} }}")]),
        (None, _) => append(
            source,
            root,
            &[format!("luau = {{ aliases = {{ {fields} }} }}")],
        ),
    }])
}

/** the table literal under the key `name` of `table`. None without the key,
an error when the key holds anything that is not a table literal. */
fn table_field<'a>(
    table: &'a TableConstructor,
    name: &str,
) -> Result<Option<&'a TableConstructor>, String> {
    match table
        .fields()
        .iter()
        .find(|field| field_key(field) == Some(name))
    {
        None => Ok(None),
        Some(field) => match field_value(field) {
            Some(Expression::TableConstructor(inner)) => Ok(Some(inner)),
            _ => Err(format!("`{name}` is not a table literal")),
        },
    }
}

/// the key of `name = value` or `["name"] = value`.
fn field_key(field: &Field) -> Option<&str> {
    match field {
        Field::NameKey { key, .. } => match key.token_type() {
            TokenType::Identifier { identifier } => Some(identifier.as_str()),
            _ => None,
        },
        Field::ExpressionKey { key, .. } => literal(key),
        _ => None,
    }
}

fn field_value(field: &Field) -> Option<&Expression> {
    match field {
        Field::NameKey { value, .. } | Field::ExpressionKey { value, .. } => Some(value),
        _ => None,
    }
}

/// the text of a string literal with no escapes in it.
fn literal(expression: &Expression) -> Option<&str> {
    let Expression::String(token) = expression else {
        return None;
    };
    match token.token_type() {
        TokenType::StringLiteral { literal, .. } if !literal.contains('\\') => {
            Some(literal.as_str())
        }
        _ => None,
    }
}

/// a key as Luau source. a name stays bare, anything else gets brackets: `["server-packages"]`.
fn key_text(name: &str) -> String {
    if name
        .chars()
        .all(|char| char.is_ascii_alphanumeric() || char == '_')
    {
        name.to_string()
    } else {
        format!("[\"{name}\"]")
    }
}

fn start(node: &impl Node) -> usize {
    node.start_position().map_or(0, |position| position.bytes())
}

fn end(node: &impl Node) -> usize {
    node.end_position().map_or(0, |position| position.bytes())
}

/** the splice that adds `fields` at the end of `table`: one on each line in
a table that spans lines, else on the table's own line. */
fn append(source: &str, table: &TableConstructor, fields: &[String]) -> Splice {
    let (open, close) = table.braces().tokens();
    let Some(last) = table.fields().last() else {
        let at = end(open);
        return Splice {
            start: at,
            end: at,
            text: format!(" {} ", fields.join(", ")),
        };
    };
    let at = end(last);
    let comma = if last.punctuation().is_some() {
        ""
    } else {
        ","
    };
    let close = start(close);
    if !source[end(open)..close].contains('\n') {
        return Splice {
            start: at,
            end: at,
            text: format!("{comma} {}", fields.join(", ")),
        };
    }
    /* the new lines go after the rest of the last field's line, so a
    comment there stays with the field that it is about */
    let line_end = source[at..close]
        .find('\n')
        .map_or(close, |offset| at + offset);
    let line = &source[source[..at].rfind('\n').map_or(0, |offset| offset + 1)..at];
    let indent = &line[..line.len() - line.trim_start().len()];
    let new: String = fields
        .iter()
        .map(|field| format!("\n{indent}{field},"))
        .collect();
    Splice {
        start: at,
        end: line_end,
        text: format!("{comma}{}{new}", &source[at..line_end]),
    }
}

/// the splice that drops one field with its comma, and its whole line when nothing else is on it.
fn removal(source: &str, pair: &Pair<Field>) -> Splice {
    let (start, end) = (start(pair.value()), end(pair));
    let line_start = source[..start].rfind('\n').map_or(0, |offset| offset + 1);
    let (start, end) = match source[end..].find('\n') {
        Some(offset)
            if source[line_start..start].trim().is_empty()
                && source[end..end + offset].trim().is_empty() =>
        {
            (line_start, end + offset + 1)
        }
        _ => (start, end),
    };
    Splice {
        start,
        end,
        text: String::new(),
    }
}

/** writes the definition files of the registry packages to the settings
file of `dir`. a project with no settings file gets one only when a
package has a definition file. */
fn sync_definitions(
    manifest: &Manifest,
    dir: &Path,
    packages: &[LockedPackage],
) -> Result<(), String> {
    let mut wanted = BTreeMap::new();
    for package in packages {
        // a workspace member is source the user edits, not a store
        if matches!(package.source, DownloadSource::Workspace { .. }) {
            continue;
        }
        let store = manifest
            .packages_out(package.context())
            .join(STORE_DIR)
            .join(package.name.replace('/', "_"));
        let files = definition_files(&dir.join(&store));
        let several = files.len() > 1;
        for file in files {
            let key = if several {
                format!("@{}/{}", package.name, slashed(&file))
            } else {
                format!("@{}", package.name)
            };
            /* a package in two trees would declare its globals twice, so
            the first copy wins */
            wanted
                .entry(key)
                .or_insert_with(|| slashed(&store.join(file)));
        }
    }
    let stores: Vec<String> = Environment::ALL
        .into_iter()
        .map(|environment| {
            format!(
                "{}/",
                plain(&slashed(
                    &manifest.packages_out(environment).join(STORE_DIR)
                ))
            )
        })
        .collect();

    let path = dir.join(SETTINGS);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound && wanted.is_empty() => return Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.to_string()),
    };
    let Some(new) = edit_settings(&text, &wanted, &stores)? else {
        return Ok(());
    };
    fs::create_dir_all(dir.join(".vscode"))
        .and_then(|()| fs::write(&path, new))
        .map_err(|error| error.to_string())
}

/// a path without its leading `./`, so two spellings of one path compare equal.
fn plain(path: &str) -> &str {
    path.trim_start_matches("./")
}

/** the definition files of the package at `storage`, relative to it. the
package names them in `[target] definitions`. without that list it gives
every `.d.luau` in it, at most ten, as Crater does. */
fn definition_files(storage: &Path) -> Vec<PathBuf> {
    let declared = Manifest::load_from(&manifest::manifest_in(storage))
        .ok()
        .and_then(|manifest| Some(manifest.target()?.definitions.clone()))
        .unwrap_or_default();
    let mut files = Vec::new();
    let Ok(filter) = pack::PathFilter::new(&declared) else {
        return files;
    };
    if pack::walk(storage, storage, &[], &mut files).is_err() {
        return Vec::new();
    }
    files.retain(|file| {
        file.to_string_lossy().ends_with(".d.luau") && (declared.is_empty() || filter.matches(file))
    });
    files.sort();
    if declared.is_empty() {
        files.truncate(DEFINITIONS_CAP);
    }
    files
}

/** the settings text with embr's definition files in step, None when
nothing changes. an entry whose path goes into a package store is embr's.
every other entry is the user's, and its name stays theirs. */
fn edit_settings(
    text: &str,
    wanted: &BTreeMap<String, String>,
    stores: &[String],
) -> Result<Option<String>, String> {
    // VS Code reads a UTF-8 BOM, the JSONC parser does not
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(body) => ("\u{feff}", body),
        None => ("", text),
    };
    let root = CstRootNode::parse(body, &Default::default()).map_err(|error| error.to_string())?;
    let settings = root
        .object_value_or_create()
        .ok_or("its top-level value is not an object")?;
    let files = match settings.get(DEFINITIONS_SETTING) {
        Some(property) => property
            .object_value()
            .ok_or_else(|| format!("`{DEFINITIONS_SETTING}` is not an object"))?,
        None if wanted.is_empty() => return Ok(None),
        None => settings
            .object_value_or_create(DEFINITIONS_SETTING)
            .ok_or_else(|| format!("`{DEFINITIONS_SETTING}` is not an object"))?,
    };

    let mut changed = false;
    let mut taken = BTreeSet::new();
    for property in files.properties() {
        let Some(name) = property.name().and_then(|key| key.decoded_value().ok()) else {
            continue;
        };
        let path = property
            .value()
            .and_then(|value| value.as_string_lit()?.decoded_value().ok());
        match path {
            Some(path)
                if stores
                    .iter()
                    .any(|store| plain(&path.replace('\\', "/")).starts_with(store)) =>
            {
                if wanted.get(&name) == Some(&path) {
                    taken.insert(name);
                } else {
                    property.remove();
                    changed = true;
                }
            }
            _ => {
                taken.insert(name);
            }
        }
    }
    for (name, path) in wanted {
        if !taken.contains(name) {
            files.append(name, CstInputValue::String(path.clone()));
            changed = true;
        }
    }
    Ok(changed.then(|| format!("{bom}{}", with_newline(body, root.to_string()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("embr-test-editor-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest(text: &str) -> Manifest {
        toml::from_str(text).unwrap()
    }

    const ROBLOX: &str = "[target]\nenvironment = \"roblox\"\n";

    fn locked(name: &str, link: Option<&str>, context: Environment) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            environment: context,
            context: None,
            link: link.map(str::to_string),
            index: "https://example.com/index".to_string(),
            redirects: BTreeMap::new(),
            patch: None,
            source: DownloadSource::Zip {
                url: format!("https://example.com/{name}"),
            },
        }
    }

    fn wanted(pairs: &[(&str, Option<&str>)]) -> Aliases {
        pairs
            .iter()
            .map(|(name, path)| (name.to_string(), path.map(str::to_string)))
            .collect()
    }

    /// the own tree is `@packages`, another is `@<folder>-packages`, and a folder with no links gets no path.
    #[test]
    fn each_linked_folder_gets_an_alias() {
        let packages = [
            locked("acme/core", Some("core"), Environment::Roblox),
            locked("acme/deep", None, Environment::Lune),
            locked("acme/admin", Some("admin"), Environment::Server),
        ];

        let aliases = wanted_aliases(&manifest(ROBLOX), &packages);
        assert_eq!(aliases["packages"].as_deref(), Some("./packages/roblox"));
        assert_eq!(
            aliases["server-packages"].as_deref(),
            Some("./packages/server")
        );
        // a transitive alone puts no link in a folder
        assert_eq!(aliases["lune-packages"], None);
        assert!(!aliases.contains_key("roblox-packages"));

        // the trees that require through `script` get none
        let instance = manifest(&format!("{ROBLOX}[config]\nrequires = \"instance\"\n"));
        assert!(
            wanted_aliases(&instance, &packages)
                .values()
                .all(Option::is_none)
        );
    }

    /// a hand-written .luaurc keeps its comments and keys. only embr's aliases move.
    #[test]
    fn luaurc_edits_keep_everything_else() {
        let dir = scratch("luaurc");
        fs::create_dir_all(dir.join("mine")).unwrap();
        let text = r#"{
    // strict everywhere
    "languageMode": "strict",
    "aliases": {
        "src": "./src",
        "Packages": "./old/place",
        "lune-packages": "./packages/lune",
        "luau-packages": "./mine",
    },
}
"#;
        let aliases = wanted(&[
            ("packages", Some("./packages/roblox")),
            ("server-packages", Some("./packages/server")),
            ("lune-packages", None),
            ("luau-packages", None),
        ]);

        let new = edit_luaurc(text, &aliases, &dir).unwrap().unwrap();
        assert!(new.contains("// strict everywhere"), "{new}");
        assert!(new.contains(r#""languageMode": "strict""#), "{new}");
        assert!(new.contains(r#""src": "./src""#), "{new}");
        // Luau reads `Packages` and `packages` as one alias, so the old key takes the path
        assert!(new.contains(r#""Packages": "./packages/roblox""#), "{new}");
        assert!(
            new.contains(r#""server-packages": "./packages/server""#),
            "{new}"
        );
        // its folder is gone, so the alias goes. one that gets to a real folder stays
        assert!(!new.contains("lune-packages"), "{new}");
        assert!(new.contains(r#""luau-packages": "./mine""#), "{new}");
        assert_eq!(edit_luaurc(&new, &aliases, &dir).unwrap(), None);
    }

    /// a .luaurc that does not parse, or whose aliases are no object, is left alone.
    #[test]
    fn a_luaurc_embr_cannot_read_is_an_error() {
        let dir = scratch("luaurc-bad");
        let aliases = wanted(&[("packages", Some("./packages/roblox"))]);
        assert!(edit_luaurc("{ \"aliases\": ", &aliases, &dir).is_err());
        assert!(edit_luaurc("{ \"aliases\": [] }", &aliases, &dir).is_err());
        assert!(edit_luaurc("[]", &aliases, &dir).is_err());
    }

    /// comments and other keys around `aliases` survive, and a second run changes nothing.
    #[test]
    fn config_luau_edits_only_the_aliases() {
        let dir = scratch("config-luau");
        let source = r#"-- project config
return {
    luau = {
        languagemode = "strict", -- the mode
        lint = { ["*"] = true },
        aliases = {
            src = "./src", -- my code
            ["lune-packages"] = "./packages/lune",
            packages = "./old",
        },
        globals = { "expect" },
    },
    lute = { lint = {} },
}
"#;
        let aliases = wanted(&[
            ("packages", Some("./packages/roblox")),
            ("server-packages", Some("./packages/server")),
            ("lune-packages", None),
        ]);

        let new = edit_config_luau(source, &aliases, &dir).unwrap().unwrap();
        assert_eq!(
            new,
            r#"-- project config
return {
    luau = {
        languagemode = "strict", -- the mode
        lint = { ["*"] = true },
        aliases = {
            src = "./src", -- my code
            packages = "./packages/roblox",
            ["server-packages"] = "./packages/server",
        },
        globals = { "expect" },
    },
    lute = { lint = {} },
}
"#
        );
        assert_eq!(edit_config_luau(&new, &aliases, &dir).unwrap(), None);
    }

    /// a config with no `luau` or no `aliases` table gets one.
    #[test]
    fn config_luau_gets_the_tables_it_lacks() {
        let dir = scratch("config-luau-new");
        let aliases = wanted(&[("packages", Some("./packages/lune"))]);

        let new = edit_config_luau("return {\n\tlute = {},\n}\n", &aliases, &dir)
            .unwrap()
            .unwrap();
        assert_eq!(
            new,
            "return {\n\tlute = {},\n\tluau = { aliases = { packages = \"./packages/lune\" } },\n}\n"
        );

        let new = edit_config_luau(
            "return { luau = { languagemode = \"strict\" } }",
            &aliases,
            &dir,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            new,
            "return { luau = { languagemode = \"strict\", aliases = { packages = \"./packages/lune\" } } }"
        );
        assert_eq!(
            edit_config_luau("return {}", &aliases, &dir)
                .unwrap()
                .unwrap(),
            "return { luau = { aliases = { packages = \"./packages/lune\" } } }"
        );
    }

    /// a computed config is not embr's to rewrite.
    #[test]
    fn config_luau_shapes_embr_cannot_read_are_errors() {
        let dir = scratch("config-luau-bad");
        let aliases = wanted(&[("packages", Some("./packages/lune"))]);
        for source in [
            "local config = {}\nreturn config\n",
            "return { luau = makeLuau() }\n",
            "return { luau = { aliases = table.clone(base) } }\n",
            "return {\n",
        ] {
            assert!(
                edit_config_luau(source, &aliases, &dir).is_err(),
                "{source}"
            );
        }
    }

    /// `.config.luau` takes the aliases when it is the only config, both files get none, and no alias makes no file.
    #[test]
    fn aliases_go_to_the_config_the_project_has() {
        let packages = [locked("acme/core", Some("core"), Environment::Roblox)];
        let roblox = manifest(ROBLOX);

        let dir = scratch("which-none");
        sync_aliases(&roblox, &dir, &[]).unwrap();
        assert!(!dir.join(LUAURC).exists(), "nothing to write makes no file");
        sync_aliases(&roblox, &dir, &packages).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(LUAURC)).unwrap(),
            "{\n  \"aliases\": {\n    \"packages\": \"./packages/roblox\"\n  }\n}\n"
        );

        let dir = scratch("which-luau");
        fs::write(dir.join(CONFIG_LUAU), "return {}\n").unwrap();
        sync_aliases(&roblox, &dir, &packages).unwrap();
        assert!(!dir.join(LUAURC).exists());
        assert!(
            fs::read_to_string(dir.join(CONFIG_LUAU))
                .unwrap()
                .contains("packages = \"./packages/roblox\"")
        );

        let dir = scratch("which-both");
        fs::write(dir.join(CONFIG_LUAU), "return {}\n").unwrap();
        fs::write(dir.join(LUAURC), "{}").unwrap();
        assert!(sync_aliases(&roblox, &dir, &packages).is_err());
        assert_eq!(fs::read_to_string(dir.join(LUAURC)).unwrap(), "{}");
    }

    /// `aliases = false` and `definition-files = false` write nothing, and a rewrite of the manifest keeps them.
    #[test]
    fn the_config_keys_turn_each_half_off() {
        let dir = scratch("off");
        let store = dir.join("packages/roblox/.ember/acme_types");
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("types.d.luau"), "declare x: number\n").unwrap();
        let packages = [locked("acme/types", Some("types"), Environment::Roblox)];

        let off = manifest(&format!(
            "{ROBLOX}[config]\naliases = false\ndefinition-files = false\n"
        ));
        sync(&off, &dir, &packages, &mut |message| panic!("{message}"));
        assert!(!dir.join(LUAURC).exists());
        assert!(!dir.join(SETTINGS).exists());
        let written = toml::to_string(&off).unwrap();
        assert!(written.contains("aliases = false"), "{written}");
        assert!(written.contains("definition-files = false"), "{written}");

        sync(&manifest(ROBLOX), &dir, &packages, &mut |message| {
            panic!("{message}")
        });
        assert!(dir.join(LUAURC).exists());
        assert!(dir.join(SETTINGS).exists());
    }

    /// the user's entries and comments stay, embr's entries for gone packages go.
    #[test]
    fn settings_edits_keep_the_users_entries() {
        let stores = ["packages/roblox/.ember/".to_string()];
        let text = r#"{
    // my settings
    "luau-lsp.types.definitionFiles": {
        "@roblox": "types/globalTypes.d.luau",
        "@acme/gone": "packages/roblox/.ember/acme_gone/gone.d.luau",
        "@acme/kept": "./packages/roblox/.ember/acme_kept/kept.d.luau",
        "@acme/types": "my/own.d.luau",
    },
    "editor.tabSize": 4,
}
"#;
        let wanted: BTreeMap<String, String> = [
            (
                "@acme/kept",
                "./packages/roblox/.ember/acme_kept/kept.d.luau",
            ),
            ("@acme/new", "packages/roblox/.ember/acme_new/new.d.luau"),
            (
                "@acme/types",
                "packages/roblox/.ember/acme_types/types.d.luau",
            ),
        ]
        .into_iter()
        .map(|(name, path)| (name.to_string(), path.to_string()))
        .collect();

        let new = edit_settings(text, &wanted, &stores).unwrap().unwrap();
        assert!(new.contains("// my settings"), "{new}");
        assert!(new.contains(r#""editor.tabSize": 4"#), "{new}");
        assert!(
            new.contains(r#""@roblox": "types/globalTypes.d.luau""#),
            "{new}"
        );
        assert!(!new.contains("acme_gone"), "{new}");
        assert!(
            new.contains(r#""@acme/new": "packages/roblox/.ember/acme_new/new.d.luau""#),
            "{new}"
        );
        // the user's own entry keeps its name
        assert!(new.contains(r#""@acme/types": "my/own.d.luau""#), "{new}");
        assert!(!new.contains("acme_types"), "{new}");
        assert_eq!(edit_settings(&new, &wanted, &stores).unwrap(), None);

        assert!(
            edit_settings(
                "{ \"luau-lsp.types.definitionFiles\": [] }",
                &wanted,
                &stores
            )
            .is_err()
        );
        assert_eq!(
            edit_settings("{}", &BTreeMap::new(), &stores).unwrap(),
            None
        );
    }

    /// a package names its files, or gives every `.d.luau` up to the cap. no file means no settings.json.
    #[test]
    fn definition_files_come_from_the_packages() {
        let dir = scratch("definitions");
        let packages = [
            locked("acme/many", Some("many"), Environment::Roblox),
            locked("acme/named", None, Environment::Roblox),
        ];
        sync_definitions(&manifest(ROBLOX), &dir, &packages).unwrap();
        assert!(
            !dir.join(".vscode").exists(),
            "nothing to write makes no file"
        );

        let many = dir.join("packages/roblox/.ember/acme_many");
        fs::create_dir_all(many.join("types")).unwrap();
        for index in 0..12 {
            fs::write(many.join(format!("types/t{index:02}.d.luau")), "").unwrap();
        }
        fs::write(many.join("init.luau"), "").unwrap();
        let named = dir.join("packages/roblox/.ember/acme_named");
        fs::create_dir_all(named.join("defs")).unwrap();
        fs::write(named.join("defs/api.d.luau"), "").unwrap();
        fs::write(named.join("other.d.luau"), "").unwrap();
        fs::write(
            named.join("ember.toml"),
            "[target]\nenvironment = \"roblox\"\ndefinitions = [\"defs\"]\n",
        )
        .unwrap();

        sync_definitions(&manifest(ROBLOX), &dir, &packages).unwrap();
        let text = fs::read_to_string(dir.join(SETTINGS)).unwrap();
        let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
        let files = settings[DEFINITIONS_SETTING].as_object().unwrap();
        assert_eq!(
            files["@acme/named"],
            "packages/roblox/.ember/acme_named/defs/api.d.luau"
        );
        assert_eq!(
            files["@acme/many/types/t00.d.luau"],
            "packages/roblox/.ember/acme_many/types/t00.d.luau"
        );
        assert_eq!(files.len(), 1 + DEFINITIONS_CAP, "{text}");
    }
}
