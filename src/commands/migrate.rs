/*! `embr migrate`: moves the settings of pesde.toml, wally.toml, and
forest.json into ember.toml.

each setting that ember has a place for moves, and leaves its own file. the
rest stays where it was, so a field that a later version of the other tool
adds is never lost. a file with nothing left in it is deleted. ember.toml
wins where it already sets the same key: its value stays, and the source
entry goes all the same.

lockfiles stay. ember.lock records what embr resolved itself, and the next
`embr install` writes it. */

use crate::error::Error;
use crate::project::manifest::{
    Environment, Manifest, is_github_username, manifest_in, override_paths, parse_version_req,
    split_package_name,
};
use crate::registry::index::forest;
use crate::ui;
use clap::Args;
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::Path;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value, value};

#[derive(Args, Debug, Default)]
pub struct MigrateArgs {
    /// Migrate every manifest found, and ask nothing
    #[arg(short, long)]
    pub yes: bool,
}

const PESDE_INDEX: &str = "https://github.com/pesde-pkg/index";
const WALLY_INDEX: &str = "https://github.com/UpliftGames/wally-index";

/** the [scripts] names that pesde runs by itself at install. they are hooks
into pesde, not commands a user runs, so they stay in pesde.toml. */
const PESDE_HOOKS: [&str; 2] = ["sourcemap_generator", "roblox_sync_config_generator"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Pesde,
    Wally,
    Forest,
}

impl Source {
    const ALL: [Source; 3] = [Source::Pesde, Source::Wally, Source::Forest];

    fn file(self) -> &'static str {
        match self {
            Source::Pesde => "pesde.toml",
            Source::Wally => "wally.toml",
            Source::Forest => "forest.json",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Source::Pesde => "pesde",
            Source::Wally => "Wally",
            Source::Forest => "Forest",
        }
    }

    fn lockfile(self) -> &'static str {
        match self {
            Source::Pesde => "pesde.lock",
            Source::Wally => "wally.lock",
            Source::Forest => "forest-lock.json",
        }
    }
}

/// one setting for ember.toml: the table, the key under it, and the value.
struct Write {
    table: &'static str,
    key: String,
    value: Item,
}

impl Write {
    fn new(table: &'static str, key: impl Into<String>, value: impl Into<Item>) -> Self {
        Write {
            table,
            key: key.into(),
            value: value.into(),
        }
    }
}

/// what one source gives ember.toml, and what stays of it.
struct Plan {
    source: Source,
    writes: Vec<Write>,
    /// the source file as it stays. None when nothing is left of it.
    rest: Option<String>,
    /// the keys that stay in the source, for the report.
    kept: Vec<String>,
    /// at least one [scripts] entry now runs Lune.
    lune: bool,
}

pub fn run(args: MigrateArgs) -> Result<(), Error> {
    /* the same rule as init: a terminal gets the question, and anything
    else (CI, a script, --yes) gets the default, which is every file */
    let ask = !args.yes && std::io::stdin().is_terminal();

    migrate_in(Path::new("."), ask)
}

fn migrate_in(dir: &Path, ask: bool) -> Result<(), Error> {
    let found: Vec<Source> = Source::ALL
        .into_iter()
        .filter(|source| dir.join(source.file()).exists())
        .collect();

    if found.is_empty() {
        println!("No pesde.toml, wally.toml, or forest.json here, so there is nothing to migrate");
        return Ok(());
    }

    let chosen = match ask {
        true => choose(&found)?,
        false => found,
    };

    if chosen.is_empty() {
        println!("Nothing selected, so nothing changed");
        return Ok(());
    }

    let plans = chosen
        .iter()
        .map(|&source| {
            let text = std::fs::read_to_string(dir.join(source.file()))?;
            plan(source, &text).map_err(|reason| Error::Migrate {
                file: source.file().to_string(),
                reason,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    let path = manifest_in(dir);
    let existed = path.exists();
    let mut document: DocumentMut = match existed {
        true => std::fs::read_to_string(&path)?.parse()?,
        false => DocumentMut::new(),
    };

    let mut kept_ember = Vec::new();
    for write in plans.iter().flat_map(|plan| &plan.writes) {
        if !insert(&mut document, write) {
            kept_ember.push(format!("{}.{}", write.table, write.key));
        }
    }

    /* a migration that leaves ember.toml unreadable writes nothing at all,
    so no source loses a setting that ember.toml did not take */
    let text = document.to_string();
    toml::from_str::<Manifest>(&text).map_err(|error| Error::Migrate {
        file: "ember.toml".to_string(),
        reason: error.to_string(),
    })?;

    let wrote = plans.iter().any(|plan| !plan.writes.is_empty());
    if wrote || existed {
        std::fs::write(&path, text)?;
    }

    for plan in &plans {
        let file = dir.join(plan.source.file());
        let moved = plan.writes.len();
        match &plan.rest {
            None => {
                std::fs::remove_file(&file)?;
                ui::print_success(&format!(
                    "Moved {moved} setting{} from {} into ember.toml, and deleted {}",
                    ui::plural(moved),
                    plan.source.file(),
                    plan.source.file()
                ));
            }
            Some(rest) => {
                std::fs::write(&file, rest)?;
                ui::print_success(&format!(
                    "Moved {moved} setting{} from {} into ember.toml; it keeps what ember has no place for: {}",
                    ui::plural(moved),
                    plan.source.file(),
                    plan.kept.join(", ")
                ));
            }
        }
        if dir.join(plan.source.lockfile()).exists() {
            println!(
                "  {} stays; `embr install` writes ember.lock",
                plan.source.lockfile()
            );
        }
    }

    if !kept_ember.is_empty() {
        println!(
            "  ember.toml already set {}, so its own values stay",
            kept_ember.join(", ")
        );
    }

    if plans.iter().any(|plan| plan.lune) {
        println!("  the migrated scripts run `lune`; add it with `embr tool add lune-org/lune`");
    }

    Ok(())
}

/// the checklist, every file checked.
fn choose(found: &[Source]) -> Result<Vec<Source>, Error> {
    let labels: Vec<String> = found
        .iter()
        .map(|source| format!("{} ({})", source.title(), source.file()))
        .collect();

    inquire::set_global_render_config(ui::render_config());
    let picked = inquire::MultiSelect::new("Migrate which manifests into ember.toml?", labels)
        .with_all_selected_by_default()
        .raw_prompt()?;

    Ok(picked
        .into_iter()
        .map(|option| found[option.index])
        .collect())
}

fn plan(source: Source, text: &str) -> Result<Plan, String> {
    match source {
        Source::Pesde => plan_pesde(text),
        Source::Wally => plan_wally(text),
        Source::Forest => plan_forest(text),
    }
}

/** puts one setting into ember.toml. false when ember.toml already sets the
key, or holds something other than a table where the setting goes. */
fn insert(document: &mut DocumentMut, write: &Write) -> bool {
    let table: Option<&mut dyn TableLike> = match write.table {
        // `[[target]]` holds several, and the first is the project's own
        "target" => match document
            .entry("target")
            .or_insert(Item::Table(Table::new()))
        {
            Item::ArrayOfTables(targets) => targets
                .get_mut(0)
                .map(|target| target as &mut dyn TableLike),
            other => other.as_table_like_mut(),
        },
        name => document
            .entry(name)
            .or_insert(Item::Table(Table::new()))
            .as_table_like_mut(),
    };

    let Some(table) = table else {
        return false;
    };
    if table.contains_key(&write.key) {
        return false;
    }
    table.insert(&write.key, write.value.clone());
    true
}

/// what a TOML source keeps, as names for the report: `key`, or `table.key`.
fn kept_keys(document: &DocumentMut) -> Vec<String> {
    document.iter().map(|(key, _)| key.to_string()).collect()
}

/// drops each table that the migration emptied.
fn drop_empty(document: &mut DocumentMut, tables: &[&str]) {
    for name in tables {
        if document
            .get(name)
            .and_then(Item::as_table_like)
            .is_some_and(TableLike::is_empty)
        {
            document.remove(name);
        }
    }
}

/// the TOML source as it stays: None when no key is left.
fn rest_of(document: &DocumentMut) -> Option<String> {
    // the keys that moved leave their blank lines behind at the top
    (!document.is_empty()).then(|| document.to_string().trim_start().to_string())
}

fn str_at<'a>(table: &'a dyn TableLike, key: &str) -> Option<&'a str> {
    table.get(key).and_then(Item::as_str)
}

/// a package name that ember takes as it is: `scope/name` in lowercase.
fn valid_name(name: &str) -> bool {
    split_package_name(name).is_ok()
}

fn valid_version(version: &str) -> bool {
    semver::Version::parse(version).is_ok()
}

fn valid_req(req: &str) -> bool {
    parse_version_req(req).is_ok()
}

/// an index dependency for ember.toml.
fn registry_dependency(name: &str, version: &str, index: &str, target: Option<&str>) -> Item {
    let mut table = InlineTable::new();
    table.insert("name", name.into());
    table.insert("version", version.into());
    table.insert("index", index.into());
    if let Some(target) = target {
        table.insert("target", target.into());
    }
    value(table)
}

/** [package] authors, when every one is a GitHub username. the registry adds
each author to the owners of the scope, so ember refuses any other shape. */
fn github_authors(authors: &Array) -> Option<Array> {
    let names: Vec<&str> = authors.iter().map(Value::as_str).collect::<Option<_>>()?;
    names
        .iter()
        .all(|name| is_github_username(name))
        .then(|| names.into_iter().collect())
}

/// moves the plain string keys of [package] that ember spells the same.
fn package_strings(table: &mut dyn TableLike, keys: &[&str], writes: &mut Vec<Write>) {
    for key in keys {
        if let Some(text) = str_at(table, key).map(str::to_string) {
            table.remove(key);
            writes.push(Write::new("package", *key, text));
        }
    }
}

// --- pesde ------------------------------------------------------------------

/** the ember.toml index keys for the keys of pesde's [indices] and
[wally_indices]. pesde's `default` becomes `pesde` and `wally`, so a project
that already uses ember's own default index keeps it. */
struct PesdeIndices {
    pesde: BTreeMap<String, String>,
    wally: BTreeMap<String, String>,
}

impl PesdeIndices {
    fn key(map: &BTreeMap<String, String>, key: Option<&str>) -> Option<String> {
        map.get(key.unwrap_or("default")).cloned()
    }
}

fn plan_pesde(text: &str) -> Result<Plan, String> {
    let mut document: DocumentMut = text.parse().map_err(|error| format!("{error}"))?;
    let mut writes = Vec::new();

    // [package] is the top level in pesde.toml
    let named = str_at(document.as_table(), "name").is_some_and(valid_name)
        && str_at(document.as_table(), "version").is_some_and(valid_version);
    if named {
        let top = document.as_table_mut();
        package_strings(
            top,
            &["name", "version", "description", "license", "repository"],
            &mut writes,
        );
        if let Some(private) = top.get("private").and_then(Item::as_bool) {
            top.remove("private");
            writes.push(Write::new("package", "private", private));
        }
        if let Some(authors) = top
            .get("authors")
            .and_then(Item::as_array)
            .and_then(github_authors)
        {
            top.remove("authors");
            writes.push(Write::new("package", "authors", authors));
        }
    }

    // [target], with the two lists pesde keeps at the top level
    let environment = document
        .get("target")
        .and_then(Item::as_table_like)
        .and_then(|target| str_at(target, "environment"))
        .and_then(|name| Environment::from_pesde(name).ok());
    if let Some(environment) = environment {
        let target = document["target"]
            .as_table_like_mut()
            .expect("the environment came from this table");
        target.remove("environment");
        writes.push(Write::new("target", "environment", environment.as_str()));
        if let Some(lib) = str_at(target, "lib").map(str::to_string) {
            target.remove("lib");
            writes.push(Write::new("target", "main", lib));
        }
        for (from, to) in [("includes", "includes"), ("workspace_members", "workspace")] {
            if let Some(list) = document.get(from).and_then(Item::as_array).cloned() {
                document.remove(from);
                writes.push(Write::new("target", to, list));
            }
        }
    }

    let indices = pesde_indices(&mut document, &mut writes);
    let mut used_default = (false, false);

    // [dependencies]
    if let Some(table) = document
        .get_mut("dependencies")
        .and_then(Item::as_table_like_mut)
    {
        let aliases: Vec<String> = table.iter().map(|(alias, _)| alias.to_string()).collect();
        for alias in aliases {
            let Some(entry) = table.get(&alias).and_then(Item::as_table_like) else {
                continue;
            };
            if let Some(item) = pesde_dependency(entry, &indices, &mut used_default) {
                table.remove(&alias);
                writes.push(Write::new("dependencies", alias, item));
            }
        }
    }

    // [overrides]: `foo>bar,baz>bar` is ember's `foo.bar, baz.bar`
    if let Some(table) = document
        .get_mut("overrides")
        .and_then(Item::as_table_like_mut)
    {
        let keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
        for key in keys {
            let path = key
                .split(',')
                .map(|path| path.trim().replace('>', "."))
                .collect::<Vec<_>>()
                .join(", ");
            if override_paths(&path).is_err() {
                continue;
            }
            let item = match table.get(&key) {
                Some(Item::Value(Value::String(alias))) => Some(value(alias.value().as_str())),
                Some(entry) => entry
                    .as_table_like()
                    .and_then(|entry| pesde_dependency(entry, &indices, &mut used_default)),
                None => None,
            };
            if let Some(item) = item {
                table.remove(&key);
                writes.push(Write::new("overrides", path, item));
            }
        }
    }

    // a dependency that named no index reads pesde's own, which ember spells out
    if used_default.0
        && !writes
            .iter()
            .any(|w| w.table == "indices" && w.key == "pesde")
    {
        writes.push(Write::new("indices", "pesde", PESDE_INDEX));
    }
    if used_default.1
        && !writes
            .iter()
            .any(|w| w.table == "indices" && w.key == "wally")
    {
        writes.push(Write::new("indices", "wally", WALLY_INDEX));
    }

    // [scripts]: pesde runs each one with Lune
    let mut lune = false;
    if let Some(table) = document
        .get_mut("scripts")
        .and_then(Item::as_table_like_mut)
    {
        let names: Vec<String> = table.iter().map(|(name, _)| name.to_string()).collect();
        for name in names {
            if PESDE_HOOKS.contains(&name.as_str()) {
                continue;
            }
            if let Some(path) = str_at(table, &name).map(str::to_string) {
                table.remove(&name);
                writes.push(Write::new("scripts", name, lune_command(&path)));
                lune = true;
            }
        }
    }

    drop_empty(
        &mut document,
        &[
            "target",
            "indices",
            "wally_indices",
            "dependencies",
            "overrides",
            "scripts",
        ],
    );

    Ok(Plan {
        source: Source::Pesde,
        writes,
        kept: kept_keys(&document),
        rest: rest_of(&document),
        lune,
    })
}

/// moves [indices] and [wally_indices], and says what each key became.
fn pesde_indices(document: &mut DocumentMut, writes: &mut Vec<Write>) -> PesdeIndices {
    let mut indices = PesdeIndices {
        pesde: BTreeMap::from([("default".to_string(), "pesde".to_string())]),
        wally: BTreeMap::from([("default".to_string(), "wally".to_string())]),
    };

    for (table, map, default) in [
        ("indices", &mut indices.pesde, "pesde"),
        ("wally_indices", &mut indices.wally, "wally"),
    ] {
        let Some(entries) = document.get_mut(table).and_then(Item::as_table_like_mut) else {
            continue;
        };
        let keys: Vec<String> = entries.iter().map(|(key, _)| key.to_string()).collect();
        for key in keys {
            let Some(url) = str_at(entries, &key).map(str::to_string) else {
                continue;
            };
            let ember = match (key.as_str(), table) {
                ("default", _) => default.to_string(),
                (_, "wally_indices") => format!("wally-{key}"),
                _ => key.clone(),
            };
            entries.remove(&key);
            map.insert(key, ember.clone());
            writes.push(Write::new("indices", ember, url));
        }
    }

    indices
}

/** one pesde specifier as an ember one. None for a shape ember has no
place for: git, path, or a key ember does not know. */
fn pesde_dependency(
    entry: &dyn TableLike,
    indices: &PesdeIndices,
    used_default: &mut (bool, bool),
) -> Option<Item> {
    let has_only = |allowed: &[&str]| entry.iter().all(|(key, _)| allowed.contains(&key));

    if let Some(name) = str_at(entry, "name") {
        let version = str_at(entry, "version").filter(|req| valid_req(req))?;
        if !has_only(&["name", "version", "index", "target"]) || !valid_name(name) {
            return None;
        }
        let index_key = str_at(entry, "index");
        let index = PesdeIndices::key(&indices.pesde, index_key)?;
        let target = match str_at(entry, "target") {
            Some(target) => Some(Environment::from_pesde(target).ok()?.as_str()),
            None => None,
        };
        used_default.0 |= index_key.is_none() && index == "pesde";
        return Some(registry_dependency(name, version, &index, target));
    }

    if let Some(name) = str_at(entry, "wally") {
        // a published pesde.toml spells the name `wally#scope/name`
        let name = name.strip_prefix("wally#").unwrap_or(name).to_lowercase();
        let version = str_at(entry, "version").filter(|req| valid_req(req))?;
        if !has_only(&["wally", "version", "index"]) || !valid_name(&name) {
            return None;
        }
        let index_key = str_at(entry, "index");
        let index = PesdeIndices::key(&indices.wally, index_key)?;
        used_default.1 |= index_key.is_none() && index == "wally";
        return Some(registry_dependency(&name, version, &index, None));
    }

    if let Some(workspace) = str_at(entry, "workspace") {
        if !has_only(&["workspace", "version"]) {
            return None;
        }
        let mut table = InlineTable::new();
        table.insert("workspace", workspace.into());
        table.insert("version", str_at(entry, "version").unwrap_or("^").into());
        return Some(value(table));
    }

    None
}

/** the ember script for a pesde script. pesde runs the file with Lune, and
`embr run` appends its arguments, so Lune's `process.args` sees the same. */
fn lune_command(path: &str) -> String {
    match path.contains(char::is_whitespace) {
        true => format!("lune run \"{path}\""),
        false => format!("lune run {path}"),
    }
}

// --- Wally ------------------------------------------------------------------

fn plan_wally(text: &str) -> Result<Plan, String> {
    let mut document: DocumentMut = text.parse().map_err(|error| format!("{error}"))?;
    let mut writes = Vec::new();

    if let Some(package) = document
        .get_mut("package")
        .and_then(Item::as_table_like_mut)
    {
        let named = str_at(package, "name").is_some_and(valid_name)
            && str_at(package, "version").is_some_and(valid_version);
        if named {
            package_strings(
                package,
                &["name", "version", "description", "license", "repository"],
                &mut writes,
            );
            if let Some(private) = package.get("private").and_then(Item::as_bool) {
                package.remove("private");
                writes.push(Write::new("package", "private", private));
            }
            if let Some(authors) = package
                .get("authors")
                .and_then(Item::as_array)
                .and_then(github_authors)
            {
                package.remove("authors");
                writes.push(Write::new("package", "authors", authors));
            }
        }

        // wally's realm is the target environment; `dev` has no ember equal
        let environment =
            str_at(package, "realm").and_then(|realm| Environment::from_wally_realm(realm).ok());
        if let Some(environment) = environment {
            package.remove("realm");
            writes.push(Write::new("target", "environment", environment.as_str()));
            for (from, to) in [("include", "includes"), ("exclude", "excludes")] {
                if let Some(list) = package.get(from).and_then(Item::as_array).cloned() {
                    package.remove(from);
                    writes.push(Write::new("target", to, list));
                }
            }
        }

        // the registry the dependencies resolve against
        let registry = str_at(package, "registry")
            .unwrap_or(WALLY_INDEX)
            .to_string();
        package.remove("registry");
        writes.push(Write::new("indices", "wally", registry));
    }

    // a server dependency installs into the Roblox server tree
    for (table, target) in [
        ("dependencies", None),
        ("server-dependencies", Some(Environment::Server.as_str())),
    ] {
        let Some(entries) = document.get_mut(table).and_then(Item::as_table_like_mut) else {
            continue;
        };
        let aliases: Vec<String> = entries.iter().map(|(alias, _)| alias.to_string()).collect();
        for alias in aliases {
            let Some((name, req)) = str_at(entries, &alias).and_then(|spec| spec.split_once('@'))
            else {
                continue;
            };
            let name = name.to_lowercase();
            if !valid_name(&name) || !valid_req(req) {
                continue;
            }
            let item = registry_dependency(&name, req, "wally", target);
            entries.remove(&alias);
            writes.push(Write::new("dependencies", alias, item));
        }
    }

    // a manifest with dependencies and no [package] still reads the main index
    if !writes.iter().any(|w| w.table == "indices")
        && writes.iter().any(|w| w.table == "dependencies")
    {
        writes.push(Write::new("indices", "wally", WALLY_INDEX));
    }

    drop_empty(
        &mut document,
        &["package", "dependencies", "server-dependencies"],
    );

    Ok(Plan {
        source: Source::Wally,
        writes,
        kept: kept_keys(&document),
        rest: rest_of(&document),
        lune: false,
    })
}

// --- Forest -----------------------------------------------------------------

fn plan_forest(text: &str) -> Result<Plan, String> {
    let mut manifest: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(text).map_err(|error| error.to_string())?;
    let mut writes = Vec::new();

    let string = |manifest: &serde_json::Map<String, serde_json::Value>, key: &str| {
        manifest
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };

    // Forest splits the name into `author` (the scope) and `name`
    let name = match (string(&manifest, "author"), string(&manifest, "name")) {
        (Some(author), Some(name)) => Some(format!("{author}/{name}").to_lowercase()),
        _ => None,
    };
    let version = string(&manifest, "version").filter(|version| valid_version(version));
    if let (Some(name), Some(version)) = (name.filter(|name| valid_name(name)), version) {
        for key in ["author", "name", "version"] {
            manifest.remove(key);
        }
        writes.push(Write::new("package", "name", name));
        writes.push(Write::new("package", "version", version));
        for key in ["description", "license"] {
            if let Some(text) = string(&manifest, key) {
                manifest.remove(key);
                writes.push(Write::new("package", key, text));
            }
        }
    }

    // `uefn` is Forest's other platform, which ember does not install
    let roblox = string(&manifest, "platform").as_deref() == Some("roblox");
    if roblox {
        manifest.remove("platform");
        writes.push(Write::new(
            "target",
            "environment",
            Environment::Roblox.as_str(),
        ));
        if let Some(root) = string(&manifest, "root") {
            manifest.remove("root");
            writes.push(Write::new("target", "main", root));
        }
    }

    // a UEFN dependency has no place in a Roblox tree, so it stays
    if let Some(serde_json::Value::Object(dependencies)) =
        manifest.get_mut("dependencies").filter(|_| roblox)
    {
        let names: Vec<String> = dependencies.keys().cloned().collect();
        for name in names {
            // a range, or a range with the alias that names the install folder
            let (req, alias) = match &dependencies[&name] {
                serde_json::Value::String(req) => (req.clone(), None),
                serde_json::Value::Object(entry)
                    if entry.keys().all(|key| key == "version" || key == "alias") =>
                {
                    let Some(req) = entry.get("version").and_then(serde_json::Value::as_str) else {
                        continue;
                    };
                    let alias = entry
                        .get("alias")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                    (req.to_string(), alias)
                }
                _ => continue,
            };
            let package = name.to_lowercase();
            let Ok((_, short)) = split_package_name(&package) else {
                continue;
            };
            if !valid_req(&req) {
                continue;
            }
            let alias = alias.unwrap_or_else(|| short.to_string());
            writes.push(Write::new(
                "dependencies",
                alias,
                registry_dependency(&package, &req, "forest", None),
            ));
            dependencies.remove(&name);
        }
        if dependencies.is_empty() {
            manifest.remove("dependencies");
        }
    }

    if writes.iter().any(|w| w.table == "dependencies") {
        writes.push(Write::new("indices", "forest", forest::API_URL));
    }

    let kept = manifest.keys().cloned().collect();
    let rest = match manifest.is_empty() {
        true => None,
        false => {
            Some(serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())? + "\n")
        }
    };

    Ok(Plan {
        source: Source::Forest,
        writes,
        rest,
        kept,
        lune: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the value a plan writes under `table.key`, as TOML text.
    fn written(plan: &Plan, table: &str, key: &str) -> Option<String> {
        plan.writes
            .iter()
            .find(|w| w.table == table && w.key == key)
            .map(|w| w.value.to_string().trim().to_string())
    }

    /// an empty dir holding the given files.
    fn scratch(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("embr-test-migrate-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            std::fs::write(dir.join(file), text).unwrap();
        }
        dir
    }

    const PESDE: &str = r#"name = "acme/kit"
version = "1.2.3"
description = "A kit"
authors = ["savruun"]
includes = ["src", "pesde.toml"]

[target]
environment = "roblox"
lib = "src/init.luau"
build_files = ["src"]

[indices]
default = "https://github.com/pesde-pkg/index"

[wally_indices]
default = "https://github.com/UpliftGames/wally-index"

[dependencies]
signal = { name = "acme/signal", version = "^2.0.0" }
promise = { wally = "wally#evaera/Promise", version = "^4.0.0" }
local = { path = "../local" }

[dev_dependencies]
testez = { wally = "roblox/testez", version = "^0.4.0" }

[scripts]
build = "scripts/build.luau"
sourcemap_generator = "scripts/sourcemap.luau"

[engines]
lune = "^0.8.9"
"#;

    #[test]
    fn pesde_moves_what_ember_has_a_place_for() {
        let plan = plan_pesde(PESDE).unwrap();

        assert_eq!(written(&plan, "package", "name").unwrap(), "\"acme/kit\"");
        assert_eq!(
            written(&plan, "package", "authors").unwrap(),
            "[\"savruun\"]"
        );
        assert_eq!(
            written(&plan, "target", "environment").unwrap(),
            "\"roblox\""
        );
        assert_eq!(
            written(&plan, "target", "main").unwrap(),
            "\"src/init.luau\""
        );
        assert!(written(&plan, "target", "includes").is_some());

        // pesde's `default` indices keep ember's own default untouched
        assert_eq!(
            written(&plan, "indices", "pesde").unwrap(),
            "\"https://github.com/pesde-pkg/index\""
        );
        assert!(written(&plan, "indices", "wally").is_some());
        assert!(written(&plan, "indices", "default").is_none());

        let signal = written(&plan, "dependencies", "signal").unwrap();
        assert!(signal.contains("index = \"pesde\""), "{signal}");
        let promise = written(&plan, "dependencies", "promise").unwrap();
        assert!(promise.contains("name = \"evaera/promise\""), "{promise}");
        assert!(promise.contains("index = \"wally\""), "{promise}");

        // what ember has no place for stays in pesde.toml
        assert!(written(&plan, "dependencies", "local").is_none());
        let rest = plan.rest.unwrap();
        for kept in [
            "build_files",
            "local",
            "dev_dependencies",
            "sourcemap_generator",
            "engines",
        ] {
            assert!(rest.contains(kept), "{kept} left pesde.toml:\n{rest}");
        }
        assert!(!rest.contains("acme/signal"), "{rest}");
        assert!(!rest.contains("[indices]"), "{rest}");
    }

    /// pesde runs a script file with Lune, and so does the ember script.
    #[test]
    fn pesde_scripts_run_with_lune() {
        let plan = plan_pesde(PESDE).unwrap();

        assert_eq!(
            written(&plan, "scripts", "build").unwrap(),
            "\"lune run scripts/build.luau\""
        );
        // a hook pesde runs by itself is no script to run
        assert!(written(&plan, "scripts", "sourcemap_generator").is_none());
        assert!(plan.lune);
        assert_eq!(
            lune_command("my scripts/a.luau"),
            "lune run \"my scripts/a.luau\""
        );
    }

    #[test]
    fn pesde_overrides_take_the_ember_path_spelling() {
        let plan = plan_pesde(
            "[overrides]\n\"foo>bar,baz>bar\" = { name = \"acme/bar\", version = \"2.0.0\" }\n\"a>b\" = \"b\"\n",
        )
        .unwrap();

        assert!(written(&plan, "overrides", "foo.bar, baz.bar").is_some());
        assert_eq!(written(&plan, "overrides", "a.b").unwrap(), "\"b\"");
        assert!(plan.rest.is_none());
    }

    #[test]
    fn wally_dependencies_point_at_the_wally_index() {
        let plan = plan_wally(
            r#"[package]
name = "acme/game"
version = "0.1.0"
realm = "shared"
registry = "https://github.com/UpliftGames/wally-index"
homepage = "https://acme.dev"

[place]
shared-packages = "game.ReplicatedStorage.Packages"

[dependencies]
Promise = "evaera/promise@4.0.0"

[server-dependencies]
ProfileService = "madstudioroblox/profileservice@1.4.2"

[dev-dependencies]
TestEZ = "roblox/testez@0.4.1"
"#,
        )
        .unwrap();

        assert_eq!(
            written(&plan, "target", "environment").unwrap(),
            "\"roblox\""
        );
        assert!(written(&plan, "indices", "wally").is_some());
        let promise = written(&plan, "dependencies", "Promise").unwrap();
        assert!(promise.contains("version = \"4.0.0\""), "{promise}");
        // a server dependency lands in the server tree
        let profile = written(&plan, "dependencies", "ProfileService").unwrap();
        assert!(profile.contains("target = \"roblox_server\""), "{profile}");

        assert_eq!(plan.kept, ["package", "place", "dev-dependencies"]);
        let rest = plan.rest.unwrap();
        assert!(rest.contains("homepage"), "{rest}");
        assert!(!rest.contains("registry"), "{rest}");
    }

    #[test]
    fn forest_joins_the_scope_and_keeps_the_alias() {
        let plan = plan_forest(
            r#"{
  "name": "datastream",
  "author": "stratiz",
  "version": "1.4.2",
  "platform": "roblox",
  "license": "MIT",
  "root": "src/init.luau",
  "dependencies": {
    "forest/signal": "^2.0.1",
    "user1/promise": { "version": "^3.1.0", "alias": "promise3" }
  },
  "mounts": { "server": { "dependencies": {} } }
}"#,
        )
        .unwrap();

        assert_eq!(
            written(&plan, "package", "name").unwrap(),
            "\"stratiz/datastream\""
        );
        assert_eq!(
            written(&plan, "target", "main").unwrap(),
            "\"src/init.luau\""
        );
        assert!(
            written(&plan, "dependencies", "signal")
                .unwrap()
                .contains("index = \"forest\"")
        );
        assert!(written(&plan, "dependencies", "promise3").is_some());
        assert_eq!(
            written(&plan, "indices", "forest").unwrap(),
            "\"https://api.forest.dev\""
        );
        assert_eq!(plan.kept, ["mounts"]);
    }

    #[test]
    fn a_uefn_project_keeps_its_dependencies() {
        let plan =
            plan_forest(r#"{ "platform": "uefn", "dependencies": { "forest/signal": "^2.0.1" } }"#)
                .unwrap();

        assert!(plan.writes.is_empty());
        // serde_json sorts the keys of what stays
        assert_eq!(plan.kept, ["dependencies", "platform"]);
    }

    #[test]
    fn a_file_with_nothing_left_is_deleted_and_the_rest_stays() {
        let dir = scratch(
            "end-to-end",
            &[
                (
                    "wally.toml",
                    "[package]\nname = \"acme/game\"\nversion = \"0.1.0\"\nrealm = \"shared\"\nregistry = \"https://github.com/UpliftGames/wally-index\"\n\n[dependencies]\nPromise = \"evaera/promise@4.0.0\"\n",
                ),
                (
                    "forest.json",
                    "{ \"platform\": \"roblox\", \"mounts\": {}, \"dependencies\": { \"forest/signal\": \"^2.0.1\" } }",
                ),
                ("wally.lock", ""),
            ],
        );

        migrate_in(&dir, false).unwrap();

        assert!(!dir.join("wally.toml").exists(), "every wally key moved");
        assert!(dir.join("wally.lock").exists(), "lockfiles stay");
        let forest = std::fs::read_to_string(dir.join("forest.json")).unwrap();
        assert!(forest.contains("mounts"), "{forest}");
        assert!(!forest.contains("forest/signal"), "{forest}");

        let manifest: Manifest =
            toml::from_str(&std::fs::read_to_string(dir.join("ember.toml")).unwrap()).unwrap();
        assert_eq!(manifest.name(), Some("acme/game"));
        assert_eq!(manifest.target().unwrap().environment, Environment::Roblox);
        assert!(manifest.dependencies.contains_key("Promise"));
        assert!(manifest.dependencies.contains_key("signal"));
    }

    /// ember.toml wins a key it already sets, and the source entry goes anyway.
    #[test]
    fn ember_toml_keeps_its_own_value() {
        let dir = scratch(
            "conflict",
            &[
                (
                    "ember.toml",
                    "# mine\n[dependencies]\nPromise = { name = \"evaera/promise\", version = \"^3.0.0\", index = \"wally\" }\n\n[indices]\nwally = \"https://github.com/UpliftGames/wally-index\"\n",
                ),
                (
                    "wally.toml",
                    "[dependencies]\nPromise = \"evaera/promise@4.0.0\"\n",
                ),
            ],
        );

        migrate_in(&dir, false).unwrap();

        let ember = std::fs::read_to_string(dir.join("ember.toml")).unwrap();
        assert!(ember.starts_with("# mine"), "{ember}");
        assert!(ember.contains("^3.0.0"), "{ember}");
        assert!(!dir.join("wally.toml").exists());
    }

    #[test]
    fn nothing_to_migrate_writes_nothing() {
        let dir = scratch("empty", &[]);
        migrate_in(&dir, false).unwrap();
        assert!(!dir.join("ember.toml").exists());
    }
}
