/*! `embr preset`: manifest presets. a preset is a part of an ember.toml, one
TOML text with no other file. `save` keeps the manifest of the current
project under ~/.ember/presets, without the keys that name one project.
`apply` merges a preset into the ember.toml of a project, and `embr init
--preset` into a new one. `publish`, `search` and `info` talk to Ember.

```text
~/.ember/presets/<name>.toml
``` */

use crate::commands::template;
use crate::error::Error;
use crate::net::listing::{self, Kind};
use crate::net::registry;
use crate::project::manifest::{EMBER_PREFIX, Manifest, manifest_in};
use crate::sys::paths;
use crate::ui;
use clap::Subcommand;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike};

#[derive(Subcommand, Debug)]
pub enum PresetCommand {
    /// Save the manifest of the current project as a preset
    Save {
        /// Name of the preset: letters, digits, `-` and `_`
        name: String,

        /// Replace a saved preset of the same name
        #[arg(long)]
        force: bool,
    },

    /// List the saved presets
    #[command(visible_alias = "ls")]
    List {
        /// Print the list as one JSON document on stdout
        #[arg(long)]
        json: bool,
    },

    /// Print a saved preset
    Show {
        /// Name of the preset
        name: String,
    },

    /// Delete a saved preset
    #[command(visible_alias = "rm")]
    Remove {
        /// Name of the preset
        name: String,
    },

    /// Merge a preset into the ember.toml of the current project
    Apply {
        /// The preset: a saved name, or scope/name[@version] on Ember, also written ember:scope/name
        spec: String,

        /// Take the value of the preset for a key that ember.toml has
        #[arg(long)]
        force: bool,

        /// Write ember.toml without asking first
        #[arg(short, long)]
        yes: bool,

        /// Print the changes and write nothing
        #[arg(long)]
        dry_run: bool,
    },

    /// Publish a saved preset to Ember
    Publish(listing::PublishArgs),

    /// Search the presets on Ember
    Search(listing::SearchArgs),

    /// Show a preset on Ember: its versions and details
    Info(listing::InfoArgs),
}

impl PresetCommand {
    /// whether the command prints one JSON document, so no other line may follow it.
    pub fn is_json(&self) -> bool {
        match self {
            PresetCommand::List { json } => *json,
            PresetCommand::Search(args) => args.json,
            PresetCommand::Info(args) => args.json,
            _ => false,
        }
    }
}

pub fn run(command: PresetCommand) -> Result<(), Error> {
    match command {
        PresetCommand::Save { name, force } => save(&name, force),
        PresetCommand::List { json } => list(json),
        PresetCommand::Show { name } => show(&name),
        PresetCommand::Remove { name } => remove(&name),
        PresetCommand::Apply {
            spec,
            force,
            yes,
            dry_run,
        } => apply(&spec, force, yes, dry_run),
        PresetCommand::Publish(args) => {
            let text = saved_text(&args.name)?;
            check(&text)?;
            listing::run_publish(
                Kind::Preset,
                &args,
                listing::Meta::default(),
                text.as_bytes(),
            )
        }
        PresetCommand::Search(args) => listing::run_search(Kind::Preset, args),
        PresetCommand::Info(args) => listing::run_info(Kind::Preset, args),
    }
}

/** the top-level keys of ember.toml that a preset holds: each key of
[`Manifest`]. under `package`, only the keys of [`PACKAGE_ALLOWED`].
`[workspace]` is no key of the manifest type, so it is not here, and Ember
refuses a preset that has it. a test names each key of the type, so a new
key gets a decision. */
pub const ALLOWED: [&str; 15] = [
    "package",
    "target",
    "config",
    "indices",
    "dependencies",
    "dev-dependencies",
    "peer-dependencies",
    "features",
    "feature-files",
    "highlight",
    "overrides",
    "patches",
    "tools",
    "scripts",
    "studio",
];

/** the keys of `[package]` that a preset holds. each other key names one
project: its name, version, description, authors, repository, homepage,
license and logo. Ember refuses a preset with `name` or `version`. */
pub const PACKAGE_ALLOWED: [&str; 2] = ["private", "visibility"];

/// the largest preset that Ember takes, in bytes.
const MAX_BYTES: usize = 64 * 1024;

/// the file of the saved preset `name`. the name must be one that `save` takes.
fn file(name: &str) -> Result<PathBuf, Error> {
    if !template::is_name(name) {
        return Err(Error::Preset(format!(
            "'{name}' is not a preset name; use letters, digits, `-` and `_`"
        )));
    }
    Ok(paths::presets_dir()?.join(format!("{name}.toml")))
}

/// the text of the saved preset `name`, which must exist.
fn saved_text(name: &str) -> Result<String, Error> {
    file(name)
        .ok()
        .and_then(|file| fs::read_to_string(file).ok())
        .ok_or_else(|| {
            Error::Preset(format!(
                "No preset is named '{name}'; `embr preset list` shows the saved ones"
            ))
        })
}

/// the names of the saved presets, sorted.
fn saved() -> Result<Vec<String>, Error> {
    let Ok(entries) = fs::read_dir(paths::presets_dir()?) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|file| file.strip_suffix(".toml").map(str::to_string))
        .filter(|name| template::is_name(name))
        .collect();
    names.sort();
    Ok(names)
}

/** parses a preset and checks what Ember checks: UTF-8 TOML of at most 64
KB, with top-level keys from [`ALLOWED`] only, and under `[package]` keys
from [`PACKAGE_ALLOWED`] only. `apply` and `publish` both go through it, so
no `[workspace]`, `package.name` or `package.version` reaches Ember. */
fn check(text: &str) -> Result<DocumentMut, Error> {
    if text.len() > MAX_BYTES {
        return Err(Error::Preset("The preset is larger than 64 KB".to_string()));
    }
    let document: DocumentMut = text
        .parse()
        .map_err(|error| Error::Preset(format!("The preset is not valid TOML: {error}")))?;
    if let Some((key, _)) = document.iter().find(|(key, _)| !ALLOWED.contains(key)) {
        return Err(Error::Preset(format!(
            "The preset has the key `{}`, which a preset cannot hold; the keys are {}",
            ui::printable(key),
            ALLOWED.join(", ")
        )));
    }
    if let Some(package) = document.get("package") {
        let bad = match package.as_table_like() {
            Some(package) => package
                .iter()
                .map(|(key, _)| key)
                .find(|key| !PACKAGE_ALLOWED.contains(key)),
            None => Some("package"),
        };
        if let Some(key) = bad {
            return Err(Error::Preset(format!(
                "The preset has the key `{}` under [package], which a preset cannot hold; the keys there are {}",
                ui::printable(key),
                PACKAGE_ALLOWED.join(", ")
            )));
        }
    }
    Ok(document)
}

/// `manifest` without the keys that a preset cannot hold, and those keys.
fn strip(manifest: &str) -> Result<(String, Vec<String>), Error> {
    let mut document: DocumentMut = manifest.parse()?;
    let mut left_out: Vec<String> = document
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !ALLOWED.contains(&key.as_str()))
        .collect();
    for key in &left_out {
        document.remove(key);
    }
    let mut empty = false;
    if let Some(package) = document
        .get_mut("package")
        .and_then(Item::as_table_like_mut)
    {
        let named: Vec<String> = package
            .iter()
            .map(|(key, _)| key.to_string())
            .filter(|key| !PACKAGE_ALLOWED.contains(&key.as_str()))
            .collect();
        for key in named {
            package.remove(&key);
            left_out.push(format!("package.{key}"));
        }
        empty = package.is_empty();
    }
    if empty {
        document.remove("package");
    }
    Ok((document.to_string(), left_out))
}

fn save(name: &str, force: bool) -> Result<(), Error> {
    let file = file(name)?;
    let manifest = manifest_in(Path::new(""));
    if !manifest.exists() {
        return Err(Error::ManifestMissing);
    }
    let (text, left_out) = strip(&fs::read_to_string(manifest)?)?;
    if check(&text)?.is_empty() {
        return Err(Error::Preset(
            "ember.toml has no table that a preset holds".to_string(),
        ));
    }
    if file.exists() && !force {
        return Err(Error::Preset(format!(
            "A preset named '{name}' exists; pass --force to replace it"
        )));
    }
    fs::create_dir_all(paths::presets_dir()?)?;
    fs::write(&file, text)?;
    ui::print_success(&format!("Saved preset {name}"));
    if !left_out.is_empty() {
        println!("Left out: {}", left_out.join(", "));
    }
    Ok(())
}

/// the top-level keys of the saved preset `name`. empty when the file is broken.
fn keys(name: &str) -> Vec<String> {
    saved_text(name)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .map(|document| document.iter().map(|(key, _)| key.to_string()).collect())
        .unwrap_or_default()
}

fn list(json: bool) -> Result<(), Error> {
    let names = saved()?;
    let dir = paths::presets_dir()?;
    if json {
        let presets: Vec<serde_json::Value> = names
            .iter()
            .map(|name| serde_json::json!({ "name": name, "keys": keys(name) }))
            .collect();
        return ui::print_json(&serde_json::json!({
            "version": ui::JSON_VERSION,
            "folder": dir,
            "presets": presets,
        }));
    }
    if names.is_empty() {
        println!("No presets saved. Save one with `embr preset save <name>`");
        return Ok(());
    }
    println!("Presets in {}", dir.display());
    for name in names {
        ui::print_script_entry(&name, &[keys(&name).join(", ")]);
    }
    Ok(())
}

fn show(name: &str) -> Result<(), Error> {
    let text = saved_text(name)?;
    println!("Preset {name}");
    println!("File: {}\n", file(name)?.display());
    print!("{}", ui::printable(&text));
    if !text.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn remove(name: &str) -> Result<(), Error> {
    saved_text(name)?;
    fs::remove_file(file(name)?)?;
    ui::print_success(&format!("Removed preset {name}"));
    Ok(())
}

/** the text of the preset `spec`, checked. a name with no `/` is a saved
preset. `ember:scope/name[@version]` and a bare `scope/name[@version]` are
on Ember. a preset is never on GitHub. */
pub fn resolve(spec: &str) -> Result<String, Error> {
    resolve_at(&registry::api_url(), spec)
}

fn resolve_at(api: &str, spec: &str) -> Result<String, Error> {
    let text = match spec.strip_prefix(EMBER_PREFIX) {
        None if spec.starts_with(template::GITHUB_PREFIX) => {
            return Err(Error::Preset(
                "A preset is a saved one or is on Ember; GitHub has templates only".to_string(),
            ));
        }
        None if !spec.contains('/') => saved_text(spec)?,
        on_ember => {
            let (name, version) = listing::split_version(on_ember.unwrap_or(spec));
            let (_, bytes) = listing::download(api, Kind::Preset, name, version)?;
            String::from_utf8(bytes)
                .map_err(|_| Error::Preset(format!("The preset {name} is not UTF-8 text")))?
        }
    };
    check(&text)?;
    Ok(text)
}

/// what `apply` does with one key of a preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// the project has no such key, and gets it.
    Added,
    /// the project has another value, and `--force` takes the one of the preset.
    Replaced,
    /// the project has another value, and keeps it.
    Skipped,
}

/** merges the preset `preset` into the manifest `project`. gives the new
text and each change, by the dotted path of its key.

a key that the project lacks is added. a key that the project has keeps
its value, or takes the value of the preset with `force`. a table merges
key by key, and an array is one value. a key with the same value on each
side is no change. the comments and the key order of `project` stay. */
pub fn merge(
    project: &str,
    preset: &str,
    force: bool,
) -> Result<(String, Vec<(Action, String)>), Error> {
    let mut document: DocumentMut = project.parse()?;
    let incoming = check(preset)?;
    // plain values decide, because they compare. the documents hold the comments
    let ours: toml::Table = project.parse()?;
    let theirs: toml::Table = preset
        .parse()
        .map_err(|error: toml::de::Error| Error::Preset(error.message().to_string()))?;
    let mut changes = Vec::new();
    let empty = document.is_empty();
    merge_table(
        document.as_table_mut(),
        &ours,
        incoming.as_table(),
        &theirs,
        force,
        "",
        &mut changes,
    );
    // a table that came first in the preset has no blank line above it
    if !empty {
        for (action, key) in &changes {
            if let (Action::Added, Some(Item::Table(table))) = (action, document.get_mut(key)) {
                let prefix = table.decor().prefix().and_then(|prefix| prefix.as_str());
                if let Some(prefix) = prefix.filter(|prefix| !prefix.starts_with('\n')) {
                    let prefix = format!("\n{prefix}");
                    table.decor_mut().set_prefix(prefix);
                }
            }
        }
    }
    Ok((document.to_string(), changes))
}

fn merge_table(
    project: &mut dyn TableLike,
    ours: &toml::Table,
    preset: &dyn TableLike,
    theirs: &toml::Table,
    force: bool,
    path: &str,
    changes: &mut Vec<(Action, String)>,
) {
    for (key, item) in preset.iter() {
        let at = match path {
            "" => key.to_string(),
            path => format!("{path}.{key}"),
        };
        match (ours.get(key), theirs.get(key)) {
            (None, _) => {
                project.insert(key, unplaced(item));
                // the comment above the key comes with it
                if let (Some((from, _)), Some(mut to)) =
                    (preset.get_key_value(key), project.key_mut(key))
                {
                    *to.leaf_decor_mut() = from.leaf_decor().clone();
                }
                changes.push((Action::Added, at));
            }
            (Some(toml::Value::Table(ours)), Some(toml::Value::Table(theirs))) => {
                let (Some(table), Some(preset)) = (
                    project.get_mut(key).and_then(Item::as_table_like_mut),
                    item.as_table_like(),
                ) else {
                    continue;
                };
                let before = changes.len();
                merge_table(table, ours, preset, theirs, force, &at, changes);
                // a new key of an inline table gets the spaces of its neighbours
                if changes[before..]
                    .iter()
                    .any(|(action, _)| *action != Action::Skipped)
                    && let Some(inline) = project.get_mut(key).and_then(Item::as_inline_table_mut)
                {
                    inline.fmt();
                }
            }
            (ours, theirs) if ours == theirs => {}
            _ if force => {
                match (project.get_mut(key), item) {
                    // a value keeps the comment beside it
                    (Some(Item::Value(old)), Item::Value(new)) => {
                        let decor = old.decor().clone();
                        *old = new.clone();
                        *old.decor_mut() = decor;
                    }
                    _ => {
                        project.insert(key, unplaced(item));
                    }
                }
                changes.push((Action::Replaced, at));
            }
            _ => changes.push((Action::Skipped, at)),
        }
    }
}

/** a copy of `item` whose tables have no place in the file of the preset.
toml_edit prints tables by their place, so a table with the place of
another file lands between the tables of the project. with no place, it
prints after them. */
fn unplaced(item: &Item) -> Item {
    fn copy(from: &Table) -> Table {
        let mut to = Table::new();
        to.set_implicit(from.is_implicit());
        to.set_dotted(from.is_dotted());
        *to.decor_mut() = from.decor().clone();
        for (key, child) in from.iter() {
            let (key, _) = from.get_key_value(key).expect("the key is in the table");
            to.insert_formatted(key, unplaced(child));
        }
        to
    }
    match item {
        Item::Table(table) => Item::Table(copy(table)),
        Item::ArrayOfTables(tables) => {
            let mut array = ArrayOfTables::new();
            for table in tables.iter() {
                array.push(copy(table));
            }
            Item::ArrayOfTables(array)
        }
        other => other.clone(),
    }
}

/** refuses a merge that breaks a manifest that was valid before it. a
preset from Ember is text that anyone can write. */
fn check_merged(before: &str, after: &str) -> Result<(), Error> {
    if toml::from_str::<Manifest>(before).is_ok()
        && let Err(error) = toml::from_str::<Manifest>(after)
    {
        return Err(Error::Preset(format!(
            "The preset makes ember.toml invalid, so embr did not write it: {}",
            ui::printable(error.message())
        )));
    }
    Ok(())
}

fn print_changes(changes: &[(Action, String)]) {
    for (action, key) in changes {
        let key = ui::printable(key);
        match action {
            Action::Added => println!("  add      {key}"),
            Action::Replaced => println!("  replace  {key}"),
            Action::Skipped => println!("  skip     {key} (ember.toml has a value)"),
        }
    }
}

/// how many of `changes` are `action`.
fn count(changes: &[(Action, String)], action: Action) -> usize {
    changes.iter().filter(|(found, _)| *found == action).count()
}

fn apply(spec: &str, force: bool, yes: bool, dry_run: bool) -> Result<(), Error> {
    let path = manifest_in(Path::new(""));
    if !path.exists() {
        return Err(Error::ManifestMissing);
    }
    let preset = resolve(spec)?;
    let project = fs::read_to_string(&path)?;
    let (merged, changes) = merge(&project, &preset, force)?;

    let skipped = count(&changes, Action::Skipped);
    if skipped == changes.len() {
        print_changes(&changes);
        println!("Nothing to change: ember.toml has each key of the preset");
        if skipped > 0 {
            println!("Pass --force to take the values of the preset");
        }
        return Ok(());
    }
    check_merged(&project, &merged)?;
    println!("Preset {spec} changes ember.toml:");
    print_changes(&changes);
    if dry_run {
        println!("Dry run: ember.toml stays as it is");
        return Ok(());
    }
    if !yes {
        if !ui::can_prompt() {
            return Err(Error::Preset(
                "apply asks before it writes ember.toml; pass --yes to skip the question"
                    .to_string(),
            ));
        }
        inquire::set_global_render_config(ui::render_config());
        let write = inquire::Confirm::new("Write ember.toml?")
            .with_default(false)
            .prompt()?;
        if !write {
            println!("Nothing was written");
            return Ok(());
        }
    }
    fs::write(&path, merged)?;
    ui::print_success(&format!(
        "Applied preset {spec}: {} added, {} replaced, {skipped} skipped",
        count(&changes, Action::Added),
        count(&changes, Action::Replaced),
    ));
    Ok(())
}

/** merges the preset `preset` into the new manifest at `path`, for `embr
init --preset`. the values of the preset win: the manifest is new, and the
person named the preset. */
pub fn apply_to_new(path: &Path, spec: &str, preset: &str) -> Result<(), Error> {
    let project = fs::read_to_string(path)?;
    let (merged, changes) = merge(&project, preset, true)?;
    check_merged(&project, &merged)?;
    fs::write(path, merged)?;
    ui::print_success(&format!("Applied preset {spec}:"));
    print_changes(&changes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::http::mock;
    use crate::project::manifest::Package;

    /** a new key of the manifest type does not compile here until this
    test names it. then the key goes in [`ALLOWED`], or stays out with a
    reason. */
    #[allow(dead_code)]
    fn every_key_has_a_decision(manifest: Manifest) {
        let Manifest {
            // in, with the keys of `PACKAGE_ALLOWED` only
            package,
            // in, each one
            targets: _,
            config: _,
            indices: _,
            dependencies: _,
            dev_dependencies: _,
            peer_dependencies: _,
            features: _,
            feature_files: _,
            highlight: _,
            overrides: _,
            patches: _,
            tools: _,
            scripts: _,
            studio: _,
        } = manifest;
        if let Some(package) = package {
            let Package {
                // out: each one names one project
                name: _,
                version: _,
                description: _,
                authors: _,
                repository: _,
                homepage: _,
                license: _,
                logo: _,
                // in: how the project publishes, which projects can share
                private: _,
                visibility: _,
            } = package;
        }
    }

    /// a manifest with each table that the manifest type knows.
    const FULL: &str = r#"
[package]
name = "acme/rocket"
version = "1.0.0"
private = true
description = "A rocket"
authors = ["acme"]
repository = "https://github.com/acme/rocket"
homepage = "https://example.com"
license = "MIT"
logo = "logo.png"
visibility = "unlisted"

[target]
environment = "roblox"

[config]
aliases = false

[indices]
default = "https://github.com/ember-luau/index"

[dependencies]
maid = { name = "acme/maid", version = "^1" }

[dev-dependencies]
lest = { name = "acme/lest", version = "^1" }

[peer-dependencies]
react = { name = "acme/react", version = "^17" }

[features]
default = []

[feature-files.extra]
entry = "src/extra.luau"
files = ["src/extra.luau"]

[highlight]
name = "rocket"

[overrides]
"maid.signal" = { name = "acme/signal", version = "^2" }

[patches]
"acme/maid@1.0.0" = "patches/maid.patch"

[tools]
rojo = "rojo-rbx/rojo@7.4.4"

[scripts]
build = "rojo build"

[studio]
place = 1
"#;

    #[test]
    fn the_allowed_keys_are_the_keys_of_the_manifest_type() {
        // through serde, so each name is the one that ember.toml spells
        let manifest: Manifest = toml::from_str(FULL).unwrap();
        let written: toml::Table = toml::to_string(&manifest).unwrap().parse().unwrap();
        let mut of_the_type: Vec<&str> = written.keys().map(String::as_str).collect();
        of_the_type.sort();
        let mut expected = ALLOWED.to_vec();
        expected.sort();
        assert_eq!(of_the_type, expected);
        // each key of [package] is in the allowed list, or in this list of the keys left out
        let mut of_package: Vec<&str> = written["package"]
            .as_table()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        of_package.sort();
        let mut expected: Vec<&str> = PACKAGE_ALLOWED
            .into_iter()
            .chain([
                "name",
                "version",
                "description",
                "authors",
                "repository",
                "homepage",
                "license",
                "logo",
            ])
            .collect();
        expected.sort();
        assert_eq!(of_package, expected);
        assert_eq!(PACKAGE_ALLOWED, ["private", "visibility"]);
        assert_eq!(
            ALLOWED,
            [
                "package",
                "target",
                "config",
                "indices",
                "dependencies",
                "dev-dependencies",
                "peer-dependencies",
                "features",
                "feature-files",
                "highlight",
                "overrides",
                "patches",
                "tools",
                "scripts",
                "studio",
            ]
        );
    }

    #[test]
    fn a_save_drops_workspace_unknown_keys_and_the_package_keys_of_one_project() {
        let manifest =
            format!("{FULL}\n[workspace.package]\nversion = \"2.0.0\"\n\n[mystery]\nx = 1\n");
        let (text, left_out) = strip(&manifest).unwrap();
        assert_eq!(
            left_out,
            [
                "workspace",
                "mystery",
                "package.name",
                "package.version",
                "package.description",
                "package.authors",
                "package.repository",
                "package.homepage",
                "package.license",
                "package.logo",
            ]
        );
        let document = check(&text).unwrap();
        let kept: Vec<&str> = document.iter().map(|(key, _)| key).collect();
        assert_eq!(kept, ALLOWED);
        assert!(
            text.contains("[package]\nprivate = true\nvisibility = \"unlisted\"\n"),
            "{text}"
        );
        assert!(text.contains("build = \"rojo build\""), "{text}");
        // a [package] with nothing left goes away
        let (text, _) = strip("[package]\nname = \"a/b\"\n\n[scripts]\nx = \"y\"\n").unwrap();
        assert_eq!(text.trim_start(), "[scripts]\nx = \"y\"\n");

        /* and a preset with one of them is refused before an apply and
        before a publish, as Ember refuses it */
        for bad in [
            "[package]\nname = \"a/b\"\n",
            "[package]\nprivate = true\nversion = \"1.0.0\"\n",
            "[package]\nlicense = \"MIT\"\n",
            "package = 1\n",
            "[workspace.package]\nx = 1\n",
        ] {
            assert!(matches!(
                check(bad),
                Err(Error::Preset(message)) if message.contains("cannot hold")
            ));
        }
        assert!(matches!(
            check(&format!("[scripts]\nx = \"{}\"\n", "y".repeat(MAX_BYTES))),
            Err(Error::Preset(message)) if message.contains("64 KB")
        ));
        assert!(matches!(
            check("[scripts\n"),
            Err(Error::Preset(message)) if message.contains("not valid TOML")
        ));
    }

    const PROJECT: &str = "\
# my game
[package]
name = \"acme/game\" # the name

# where it runs
[target]
environment = \"roblox\" # not lune

[scripts]
# the build
build = \"rojo build\"
test = \"lune run test\"

[dependencies]
maid = { name = \"acme/maid\", version = \"^1\" }
";

    const PRESET: &str = "\
[target]
environment = \"lune\"
language = \"alloy\"

[scripts]
build = \"alloy build\"
test = \"lune run test\"
lint = [\"selene src\", \"stylua --check src\"]

[dependencies]
maid = { name = \"acme/maid\", version = \"^2\", features = [\"extra\"] }

[tools]
rojo = \"rojo-rbx/rojo@7.4.4\"

[tools.detail]
x = 1
";

    fn changed(changes: &[(Action, String)], action: Action) -> Vec<&str> {
        changes
            .iter()
            .filter(|(found, _)| *found == action)
            .map(|(_, key)| key.as_str())
            .collect()
    }

    #[test]
    fn a_merge_adds_what_is_missing_and_skips_what_is_there() {
        let (merged, changes) = merge(PROJECT, PRESET, false).unwrap();
        assert_eq!(
            changed(&changes, Action::Added),
            [
                "target.language",
                "scripts.lint",
                "dependencies.maid.features",
                "tools"
            ]
        );
        assert_eq!(
            changed(&changes, Action::Skipped),
            [
                "target.environment",
                "scripts.build",
                "dependencies.maid.version"
            ]
        );
        // the same value on each side, `scripts.test`, is no change
        assert_eq!(changes.len(), 7);
        // each comment and the key order of the project stay, and new keys go last
        assert_eq!(
            merged,
            "\
# my game
[package]
name = \"acme/game\" # the name

# where it runs
[target]
environment = \"roblox\" # not lune
language = \"alloy\"

[scripts]
# the build
build = \"rojo build\"
test = \"lune run test\"
lint = [\"selene src\", \"stylua --check src\"]

[dependencies]
maid = { name = \"acme/maid\", version = \"^1\", features = [\"extra\"] }

[tools]
rojo = \"rojo-rbx/rojo@7.4.4\"

[tools.detail]
x = 1
"
        );
    }

    #[test]
    fn force_takes_the_value_of_the_preset_and_keeps_the_comments() {
        let (merged, changes) = merge(PROJECT, PRESET, true).unwrap();
        assert_eq!(
            changed(&changes, Action::Replaced),
            [
                "target.environment",
                "scripts.build",
                "dependencies.maid.version"
            ]
        );
        assert!(changed(&changes, Action::Skipped).is_empty());
        assert!(
            merged.contains("environment = \"lune\" # not lune"),
            "{merged}"
        );
        assert!(
            merged.contains("# the build\nbuild = \"alloy build\"\n"),
            "{merged}"
        );
        assert!(merged.contains("version = \"^2\""), "{merged}");
        assert!(merged.starts_with("# my game\n[package]\nname = \"acme/game\" # the name\n"));
    }

    #[test]
    fn an_array_is_one_value_and_a_table_never_merges_with_a_value() {
        let project =
            "[scripts]\nlint = [\"selene src\"]\n\n[[target]]\nenvironment = \"roblox\"\n";
        let preset = "[scripts]\nlint = [\"stylua src\"]\n\n[target]\nenvironment = \"lune\"\n";
        let (merged, changes) = merge(project, preset, false).unwrap();
        assert_eq!(
            changed(&changes, Action::Skipped),
            ["scripts.lint", "target"]
        );
        assert_eq!(merged, project);

        let (merged, changes) = merge(project, preset, true).unwrap();
        assert_eq!(
            changed(&changes, Action::Replaced),
            ["scripts.lint", "target"]
        );
        let manifest: Manifest = toml::from_str(&merged).unwrap();
        assert_eq!(manifest.targets().len(), 1, "{merged}");
        assert!(merged.contains("lint = [\"stylua src\"]"), "{merged}");

        // a second apply of the same preset changes nothing
        let (again, changes) = merge(&merged, preset, false).unwrap();
        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(again, merged);
    }

    #[test]
    fn a_merge_that_breaks_the_manifest_is_refused() {
        let project = "[target]\nenvironment = \"roblox\"\n";
        let (merged, _) = merge(project, "[scripts]\nbuild = 3\n", false).unwrap();
        assert!(matches!(
            check_merged(project, &merged),
            Err(Error::Preset(message)) if message.contains("invalid")
        ));
        let (merged, _) = merge(project, "[scripts]\nbuild = \"x\"\n", false).unwrap();
        assert!(check_merged(project, &merged).is_ok());
    }

    #[test]
    fn a_preset_on_ember_is_checked_before_it_is_used() {
        let detail = |sha256: &str| {
            format!(
                r#"{{"scope":"ada","name":"strict","description":"d","tags":[],"latest":"1.0.0",
                "versions":[{{"version":"1.0.0","sha256":"{sha256}"}}]}}"#
            )
        };
        let good = "[scripts]\nlint = \"selene src\"\n";
        let serve = |body: &'static str| {
            let detail = detail(&crate::sys::hash::sha256_hex(body.as_bytes()));
            mock::serve(move |request| match request.contains("/download") {
                true => mock::reply("200 OK", body),
                false => mock::reply("200 OK", &detail),
            })
        };
        let (api, log) = serve(good);
        // the two spellings of a preset on Ember
        assert_eq!(resolve_at(&api, "ember:ada/strict").unwrap(), good);
        assert_eq!(resolve_at(&api, "ada/strict@1.0.0").unwrap(), good);
        let log = log.lock().unwrap();
        assert!(log[0].starts_with("get /v1/presets/ada/strict "));
        assert!(log[1].starts_with("get /v1/presets/ada/strict/versions/1.0.0/download "));
        drop(log);

        // Ember refuses a package name, and embr checks again
        let (api, _) = serve("[package]\nname = \"evil/name\"\n");
        assert!(matches!(
            resolve_at(&api, "ember:ada/strict"),
            Err(Error::Preset(message)) if message.contains("cannot hold")
        ));
        // a preset is never on GitHub, and a missing one has no fallback
        assert!(matches!(
            resolve_at(&api, "github:ada/strict"),
            Err(Error::Preset(message)) if message.contains("GitHub")
        ));
        let (api, log) = mock::serve(|_| mock::reply("404 Not Found", "{}"));
        assert!(matches!(
            resolve_at(&api, "ada/none"),
            Err(Error::EmberListingMissing { kind: "preset", .. })
        ));
        assert_eq!(log.lock().unwrap().len(), 1);
    }
}
