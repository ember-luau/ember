use crate::error::Error;
use crate::project::lockfile::Lockfile;
use crate::project::manifest::manifest_in;
use crate::project::manifest::{
    DEFAULT_INDEX_URL, Language, Manifest, features, parse_version_req, split_package_name, takes,
};
use crate::registry::index::pesde::{self, Candidate};
use crate::registry::index::{DownloadSource, Index, Refresh, git, hidden};
use crate::registry::notices::{Level, Meta};
use crate::ui;
use clap::Args;
use crossterm::style::Stylize;
use serde_json::json;
use std::path::Path;
use toml::Value;

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// Package to show, as scope/name
    pub package: String,
    /// Show the newest version that this requirement matches, such as "^1.2"
    #[arg(long)]
    pub version: Option<String>,
    /// Index key from [indices], ember, or local, in place of the default index
    #[arg(long)]
    pub index: Option<String>,
    /// Print the details as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

/// the versions listed before "and N more".
const RECENT: usize = 10;

/** shows one package from its index entries. it only reads, and works
outside a project too. */
pub fn run(args: InfoArgs) -> Result<(), Error> {
    let name = args.package.trim().to_lowercase();
    // a bad name fails here, before the index clone
    split_package_name(&name)?;
    // a path or git dependency of this project is in no index
    if args.version.is_none()
        && args.index.is_none()
        && let Some((source, manifest)) = local(&name)
    {
        if args.json {
            return ui::print_json(&local_document(&name, &source, &manifest));
        }
        print!("{}", describe_local(&name, &source, &manifest));
        return Ok(());
    }
    /* outside a project an empty manifest stands in, so --index takes
    `ember` and `local`, and the default index is this computer's */
    let manifest: Manifest = match Manifest::load() {
        Err(Error::ManifestMissing) => toml::from_str("")?,
        loaded => loaded?,
    };
    let url = manifest.index_url(args.index.as_deref())?.to_string();
    let req_text = args.version.as_deref().unwrap_or("^");
    let req = parse_version_req(req_text)?;

    let (mut entries, meta) = read(&url, &name)?;
    entries.sort_by(|a, b| {
        b.version
            .cmp(&a.version)
            .then_with(|| a.target.cmp(&b.target))
    });
    // the resolver's rule: the latest takes a prerelease only when no stable version is open
    let no_stable = !entries
        .iter()
        .any(|entry| !entry.yanked && entry.version.pre.is_empty());
    let matches = |entry: &&Candidate| takes(&req, &entry.version, no_stable);
    // a yanked version shows only when the requirement matches nothing else
    let shown = entries
        .iter()
        .filter(matches)
        .find(|entry| !entry.yanked)
        .or_else(|| entries.iter().find(matches))
        .ok_or_else(|| Error::NoMatchingVersion {
            name: name.clone(),
            req: req_text.to_string(),
        })?;

    if args.json {
        return ui::print_json(&document(&name, &entries, shown, &meta));
    }
    let mut add = format!("embr add {name}");
    if let Some(version) = &args.version {
        add.push_str(&format!(" --version \"{version}\""));
    }
    if let Some(index) = &args.index {
        add.push_str(&format!(" --index {index}"));
    }
    print!("{}", describe(&name, &entries, shown, &meta, &add));
    Ok(())
}

/** the source and the own ember.toml of `name` when ember.lock installs
it from a path or a git commit. None when the lock has no such package. */
fn local(name: &str) -> Option<(DownloadSource, Value)> {
    let lock = Lockfile::load().ok()?;
    let package = lock.packages.into_iter().find(|package| {
        package.name.eq_ignore_ascii_case(name) && package.source.local_label().is_some()
    })?;
    let text = match &package.source {
        DownloadSource::Path { path } => {
            std::fs::read_to_string(manifest_in(Path::new(path))).ok()?
        }
        DownloadSource::Git { url, rev, .. } => {
            git::locate(url, &git::Reference::Rev(rev.clone()), None, false)
                .ok()?
                .1
        }
        _ => return None,
    };
    Some((package.source, toml::from_str(&text).ok()?))
}

/// the info text of a path or git package: its version, its source and its dependencies.
fn describe_local(name: &str, source: &DownloadSource, manifest: &Value) -> String {
    let field = |key: &str| manifest.get("package")?.get(key)?.as_str();
    let mut out = name.bold().to_string();
    if let Some(version) = field("version") {
        out.push_str(&format!(" {}", version.with(ui::accent())));
    }
    out.push('\n');
    if let Some(description) = field("description") {
        out.push_str(&format!("{}\n", ui::strip_controls(description).trim()));
    }
    let source = source.local_label().unwrap_or_default();
    out.push_str(&format!("\n{}  {source}\n", "Source".bold()));
    out.push_str(&dependency_sections(manifest));
    out
}

/** the `--json` details of a path or git package, in the shape of
[`document`]. its own ember.toml gives the fields, and the ones that only
an index knows are null or empty. */
fn local_document(name: &str, source: &DownloadSource, manifest: &Value) -> serde_json::Value {
    let package = manifest.get("package");
    let field = |key: &str| {
        let text = package?.get(key)?.as_str()?.trim();
        (!text.is_empty()).then(|| ui::strip_controls(text).trim().to_string())
    };
    let authors: Vec<&str> = package
        .and_then(|package| package.get("authors"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let (dependencies, dev_dependencies) = dependencies_json(manifest);
    json!({
        "version": ui::JSON_VERSION,
        "name": name,
        "source": source.local_json(),
        "deprecated": null,
        "replacement": null,
        "notices": [],
        "shown": {
            "version": field("version"),
            "yanked": false,
            "deprecated": null,
            "description": field("description"),
            "license": field("license"),
            "repository": field("repository"),
            "homepage": field("homepage"),
            "authors": authors,
            "published_at": null,
            "published_by": null,
            "targets": [],
            "requires": {},
            "dependencies": dependencies,
            "features": feature_table(manifest),
            "feature_files": file_feature_table(manifest),
            "dev_dependencies": dev_dependencies,
        },
        "versions": [],
    })
}

/** every entry of `name` in the index at `url`. a TTL-fresh cache that
does not hold it earns one forced refresh, since it can be minutes old. a
private or unlisted package is not in ember's git index, so the API
answers for it, as it does for an install. */
fn read(url: &str, name: &str) -> Result<(Vec<Candidate>, Meta), Error> {
    let index = Index::open(url, Refresh::Ttl)?;
    let found = match index.file(name) {
        Err(Error::PackageNotFound { .. }) if index.ttl_skipped() => {
            Index::open(url, Refresh::Force)?.file(name)
        }
        found => found,
    };
    let file = match found {
        Err(missing @ Error::PackageNotFound { .. }) if url == DEFAULT_INDEX_URL => {
            hidden::file(name)?.ok_or(missing)?
        }
        Ok(Some(file)) => file,
        Ok(None) => {
            return Err(Error::IndexFetch {
                url: url.to_string(),
                reason: "only a pesde-format index lists its versions this way".to_string(),
            });
        }
        Err(error) => return Err(error),
    };
    let (entries, _) = pesde::entries(&file, url, name)?;
    Ok((entries, Meta::of(&file)))
}

/** the info text. `entries` is newest first, and `shown` is the version
that the details come from. the details are those of its first target,
because one manifest gives every target of a version its dependencies. */
fn describe(
    name: &str,
    entries: &[Candidate],
    shown: &Candidate,
    meta: &Meta,
    add: &str,
) -> String {
    let entry = &shown.entry;
    // the registry writes an empty string for "no value", as pesde does
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let same_version: Vec<&Candidate> = entries
        .iter()
        .filter(|entry| entry.version == shown.version)
        .collect();

    let mut out = format!(
        "{} {}",
        name.bold(),
        shown.version.to_string().with(ui::accent())
    );
    if shown.yanked {
        out.push_str(&" (yanked)".dark_grey().to_string());
    }
    out.push('\n');
    if let Some(description) = text("description") {
        out.push_str(&format!("{description}\n"));
    }
    // the whole package's deprecation, else this version's
    if let Some(reason) = meta.deprecated.as_ref().or(shown.deprecated.as_ref()) {
        let mut line = format!("Deprecated: {}", ui::strip_controls(reason).trim());
        if let Some(replacement) = meta
            .replacement
            .as_ref()
            .filter(|_| meta.deprecated.is_some())
        {
            line = format!(
                "{}. Use {} instead",
                line.trim_end_matches('.'),
                ui::strip_controls(replacement)
            );
        }
        out.push_str(&format!("{}\n", line.with(ui::accent())));
    }
    // every notice, with the id that `embr notices remove` takes
    for notice in &meta.notices {
        let level = match notice.level {
            Level::Security => "Security".red(),
            Level::Warning => "Warning".yellow(),
            Level::Notice => "Notice".stylize(),
        };
        let range = notice
            .versions
            .as_deref()
            .map_or(String::new(), |range| format!(" for {range}"));
        let message = ui::strip_controls(&notice.message);
        let message = message.trim().replace('\n', "\n  ");
        out.push_str(&format!(
            "{level} {}{range}: {message}\n",
            notice.id.as_str().dark_grey()
        ));
    }

    let mut facts: Vec<(&str, String)> = Vec::new();
    for (label, key) in [
        ("License", "license"),
        ("Repository", "repository"),
        ("Homepage", "homepage"),
    ] {
        if let Some(value) = text(key) {
            facts.push((label, value.to_string()));
        }
    }
    // a pesde entry names its authors, an ember entry the login that published it
    let authors: Vec<&str> = entry
        .get("authors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !authors.is_empty() {
        facts.push(("Authors", authors.join(", ")));
    }
    let date = text("published_at").map(|at| at.get(..10).unwrap_or(at));
    match (date, text("published_by")) {
        (Some(date), Some(login)) => facts.push(("Published", format!("{date} by {login}"))),
        (Some(date), None) => facts.push(("Published", date.to_string())),
        (None, Some(login)) => facts.push(("Published", format!("by {login}"))),
        (None, None) => {}
    }
    facts.push((
        "Targets",
        same_version
            .iter()
            .map(|entry| target(entry))
            .collect::<Vec<_>>()
            .join(", "),
    ));
    let mut requires: Vec<String> = Vec::new();
    for entry in &same_version {
        for (key, req) in entry
            .entry
            .get("requires")
            .and_then(Value::as_table)
            .into_iter()
            .flatten()
        {
            let line = format!("{key} {}", req.as_str().unwrap_or("?"));
            if !requires.contains(&line) {
                requires.push(line);
            }
        }
    }
    if !requires.is_empty() {
        facts.push(("Requires", requires.join(", ")));
    }
    out.push('\n');
    let width = facts
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0);
    for (label, value) in &facts {
        out.push_str(&format!("{}  {value}\n", format!("{label:<width$}").bold()));
    }

    out.push_str(&dependency_sections(entry));

    out.push_str(&format!("\n{}\n", "Versions".bold()));
    let mut versions: Vec<&semver::Version> = entries.iter().map(|entry| &entry.version).collect();
    versions.dedup();
    let width = versions
        .iter()
        .take(RECENT)
        .map(|version| version.to_string().len())
        .max()
        .unwrap_or(0);
    for version in versions.iter().take(RECENT) {
        let targets: Vec<&Candidate> = entries
            .iter()
            .filter(|entry| entry.version == **version)
            .collect();
        let mut line = format!(
            "  {:<width$}  {}",
            version.to_string(),
            targets
                .iter()
                .map(|entry| target(entry))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if targets.iter().all(|entry| entry.yanked) {
            line.push_str(&" yanked".dark_grey().to_string());
        }
        if targets.iter().any(|entry| entry.deprecated.is_some()) {
            line.push_str(&" deprecated".dark_grey().to_string());
        }
        out.push_str(&format!("{line}\n"));
    }
    if versions.len() > RECENT {
        out.push_str(&format!("  and {} more\n", versions.len() - RECENT));
    }

    out.push('\n');
    if shown.yanked {
        out.push_str("Every version that matches is yanked, so `embr add` finds none to install\n");
    } else {
        out.push_str(&format!("Add it with `{add}`\n"));
    }
    out
}

/** the `--json` details: the package-wide data, the version that the
text shows as `shown`, and every version, newest first, with each of its
targets. */
fn document(
    name: &str,
    entries: &[Candidate],
    shown: &Candidate,
    meta: &Meta,
) -> serde_json::Value {
    let text = |entry: &Candidate, key: &str| {
        entry
            .entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let clean =
        |text: Option<&String>| text.map(|text| ui::strip_controls(text).trim().to_string());
    let target = |entry: &Candidate| {
        let environment = entry.environment.map_or_else(
            || entry.target.clone().unwrap_or_default(),
            |env| env.to_string(),
        );
        json!({ "environment": environment, "language": entry.language.as_str() })
    };
    let same_version: Vec<&Candidate> = entries
        .iter()
        .filter(|entry| entry.version == shown.version)
        .collect();
    let mut requires = serde_json::Map::new();
    for entry in &same_version {
        for (key, req) in entry
            .entry
            .get("requires")
            .and_then(Value::as_table)
            .into_iter()
            .flatten()
        {
            requires
                .entry(key.clone())
                .or_insert_with(|| json!(req.as_str()));
        }
    }
    let (dependencies, dev_dependencies) = dependencies_json(&shown.entry);
    let authors: Vec<&str> = shown
        .entry
        .get("authors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();

    let mut versions: Vec<&semver::Version> = entries.iter().map(|entry| &entry.version).collect();
    versions.dedup();
    let versions: Vec<serde_json::Value> = versions
        .into_iter()
        .map(|version| {
            let targets: Vec<serde_json::Value> = entries
                .iter()
                .filter(|entry| entry.version == *version)
                .map(|entry| {
                    let mut target = target(entry);
                    target["yanked"] = json!(entry.yanked);
                    target["deprecated"] = json!(clean(entry.deprecated.as_ref()));
                    target
                })
                .collect();
            json!({ "version": version.to_string(), "targets": targets })
        })
        .collect();

    let notices: Vec<serde_json::Value> = meta
        .notices
        .iter()
        .map(|notice| {
            json!({
                "id": notice.id,
                "level": match notice.level {
                    Level::Security => "security",
                    Level::Warning => "warning",
                    Level::Notice => "notice",
                },
                "message": ui::strip_controls(&notice.message).trim(),
                "versions": notice.versions,
                "posted_by": notice.posted_by,
                "posted_at": notice.posted_at,
            })
        })
        .collect();

    json!({
        "version": ui::JSON_VERSION,
        "name": name,
        "source": null,
        "deprecated": clean(meta.deprecated.as_ref()),
        "replacement": meta.replacement.as_deref().map(ui::strip_controls),
        "notices": notices,
        "shown": {
            "version": shown.version.to_string(),
            "yanked": shown.yanked,
            "deprecated": clean(shown.deprecated.as_ref()),
            "description": text(shown, "description"),
            "license": text(shown, "license"),
            "repository": text(shown, "repository"),
            "homepage": text(shown, "homepage"),
            "authors": authors,
            "published_at": text(shown, "published_at"),
            "published_by": text(shown, "published_by"),
            "targets": same_version.iter().map(|entry| target(entry)).collect::<Vec<_>>(),
            "requires": requires,
            "dependencies": dependencies,
            "features": feature_table(&shown.entry),
            "feature_files": file_feature_table(&shown.entry),
            "dev_dependencies": dev_dependencies,
        },
        "versions": versions,
    })
}

/** the dependencies and dev dependencies of an entry for `--json`, read
as [`dependencies`] reads them: `{alias, name, version, source, index,
features, default_features, optional, peer}`. the `features` of the entry
say which features gate a dependency. */
fn dependencies_json(entry: &Value) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let (mut runtime, mut dev) = (Vec::new(), Vec::new());
    for (table, in_dev) in [
        ("dependencies", false),
        ("optional-dependencies", false),
        ("feature-dependencies", false),
        ("peer-dependencies", false),
        ("dev-dependencies", true),
    ] {
        for (alias, spec) in entry
            .get(table)
            .and_then(Value::as_table)
            .into_iter()
            .flatten()
        {
            let (spec, kind) = match spec {
                Value::Array(pair) => (pair.first(), pair.get(1).and_then(Value::as_str)),
                spec => (Some(spec), None),
            };
            let Some(spec) = spec else {
                continue;
            };
            let field = |key: &str| spec.get(key).and_then(Value::as_str);
            let name = ["name", "wally", "workspace"].into_iter().find_map(field);
            // a git or path entry names its source, pesde's git entry a `repo`
            let source = if let Some(url) = field("git").or_else(|| field("repo")) {
                json!({
                    "type": "git",
                    "url": url,
                    "rev": field("rev"),
                    "tag": field("tag"),
                    "branch": field("branch"),
                })
            } else if let Some(path) = field("path") {
                json!({ "type": "path", "path": path })
            } else {
                serde_json::Value::Null
            };
            let asked: Vec<&str> = spec
                .get("features")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let dependency = json!({
                "alias": alias,
                "name": name.map(|name| name.strip_prefix("wally#").unwrap_or(name)),
                // a git or path entry needs no version
                "version": field("version").or(source.is_null().then_some("*")),
                "source": source,
                "index": field("index"),
                "features": asked,
                "default_features": spec.get("default-features").and_then(Value::as_bool)
                    != Some(false),
                "optional": table == "optional-dependencies"
                    || spec.get("optional").and_then(Value::as_bool) == Some(true),
                "peer": table == "peer-dependencies" || kind == Some("peer"),
            });
            if in_dev || kind == Some("dev") {
                dev.push(dependency);
            } else {
                runtime.push(dependency);
            }
        }
    }
    (runtime, dev)
}

/// the Dependencies, Features and Dev dependencies sections of an entry or a manifest.
fn dependency_sections(entry: &Value) -> String {
    let mut out = String::new();
    let (dependencies, dev_dependencies) = dependencies(entry);
    for (title, list) in [
        ("Dependencies", dependencies),
        ("Features", features(entry)),
        ("Dev dependencies", dev_dependencies),
    ] {
        if list.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{}\n", title.bold()));
        let width = list.iter().map(|(alias, _)| alias.len()).max().unwrap_or(0);
        for (alias, spec) in list {
            out.push_str(&format!("  {alias:<width$}  {spec}\n"));
        }
    }
    out
}

/// a target as a person reads it: the environment, and the language when it is not luau.
fn target(entry: &Candidate) -> String {
    let environment = entry.environment.map_or_else(
        || entry.target.clone().unwrap_or_default(),
        |env| env.to_string(),
    );
    if entry.language == Language::Luau {
        environment
    } else {
        format!("{environment} ({})", entry.language.as_str())
    }
}

/// one dependency as info shows it: the alias, then the package and its requirement.
type Spec = (String, String);

/** the entry's `[features]` table, read as one: an index entry and a
shipped ember.toml spell it the same. */
fn feature_table(entry: &Value) -> features::Features {
    entry
        .get("features")
        .cloned()
        .and_then(|table| table.try_into().ok())
        .unwrap_or_default()
}

/** the entry's `[feature-files]` table. an index entry and a shipped
ember.toml spell it the same, and a table that does not parse shows as
none, as install reads it. */
fn file_feature_table(entry: &Value) -> features::FileFeatures {
    entry
        .get("feature-files")
        .cloned()
        .and_then(|table| table.try_into().ok())
        .unwrap_or_default()
}

/** the (feature, what it turns on) pairs of an entry. a default feature
says so, and a file feature names its files and its entry. */
fn features(entry: &Value) -> Vec<Spec> {
    let table = feature_table(entry);
    let owned = file_feature_table(entry);
    let defaults = table.get(features::DEFAULT);
    table
        .iter()
        .filter(|(name, _)| *name != features::DEFAULT)
        .map(|(name, items)| {
            let mut text = match items.as_slice() {
                [] => "-".to_string(),
                items => items.join(", "),
            };
            if let Some(owned) = owned.get(name) {
                text.push_str(&format!(
                    " (files: {}; entry: {})",
                    owned.files.join(", "),
                    owned.entry
                ));
            }
            if defaults.is_some_and(|defaults| defaults.contains(name)) {
                text.push_str(&" (default)".dark_grey().to_string());
            }
            (name.clone(), text)
        })
        .collect()
}

/** the (alias, `name requirement`) pairs of an entry's dependencies and
dev dependencies, from the raw entry. an ember entry keeps its optional,
gated, peer and dev dependencies in tables of their own, and a pesde entry
marks one `peer` or `dev` in a [spec, kind] pair. a gated one names the
features that turn it on. */
fn dependencies(entry: &Value) -> (Vec<Spec>, Vec<Spec>) {
    let feature_table = feature_table(entry);
    let (mut runtime, mut dev) = (Vec::new(), Vec::new());
    for (table, in_dev) in [
        ("dependencies", false),
        ("optional-dependencies", false),
        ("feature-dependencies", false),
        ("peer-dependencies", false),
        ("dev-dependencies", true),
    ] {
        for (alias, spec) in entry
            .get(table)
            .and_then(Value::as_table)
            .into_iter()
            .flatten()
        {
            let (spec, kind) = match spec {
                Value::Array(pair) => (pair.first(), pair.get(1).and_then(Value::as_str)),
                spec => (Some(spec), None),
            };
            let Some(spec) = spec else {
                continue;
            };
            let field = |key: &str| spec.get(key).and_then(Value::as_str);
            let name = ["name", "wally", "workspace"]
                .into_iter()
                .find_map(field)
                .unwrap_or("?");
            // pesde writes a wally package name with a "wally#" prefix
            let name = name.strip_prefix("wally#").unwrap_or(name);
            // a git or path entry names its source, pesde's git entry a `repo`
            let mut text = if let Some(url) = field("git").or_else(|| field("repo")) {
                let reference = ["rev", "tag", "branch"]
                    .into_iter()
                    .find_map(|key| Some(format!(" {key} {}", field(key)?)))
                    .unwrap_or_default();
                format!("git {url}{reference}")
            } else if let Some(path) = field("path") {
                format!("path {path}")
            } else {
                format!("{name} {}", field("version").unwrap_or("*"))
            };
            if let Some(index) = field("index") {
                text.push_str(&format!(" (index {index})"));
            }
            let asked: Vec<&str> = spec
                .get("features")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if !asked.is_empty() {
                text.push_str(&format!(" [{}]", asked.join(", ")));
            }
            if spec.get("default-features").and_then(Value::as_bool) == Some(false) {
                text.push_str(" (no default features)");
            }
            // the features that list this alias turn it on
            let turned_on_by: Vec<&str> = feature_table
                .iter()
                .filter(|(name, items)| *name != features::DEFAULT && items.contains(alias))
                .map(|(name, _)| name.as_str())
                .collect();
            if !turned_on_by.is_empty() {
                text.push_str(&format!(" (feature {})", turned_on_by.join(", ")));
            }
            let optional = spec.get("optional").and_then(Value::as_bool) == Some(true);
            if table == "optional-dependencies" || optional {
                text.push_str(" (optional)");
            }
            if table == "peer-dependencies" || kind == Some("peer") {
                text.push_str(" (peer)");
            }
            if in_dev || kind == Some("dev") {
                dev.push((alias.clone(), text));
            } else {
                runtime.push((alias.clone(), text));
            }
        }
    }
    (runtime, dev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::index::pesde;

    /// drops the colour escapes, so a test reads the text alone.
    fn plain(text: &str) -> String {
        let mut out = String::new();
        let mut characters = text.chars();
        while let Some(character) = characters.next() {
            if character == '\x1b' {
                characters.by_ref().find(|&escape| escape == 'm');
            } else {
                out.push(character);
            }
        }
        out
    }

    fn entries(file: &str) -> (Vec<Candidate>, Meta) {
        let file: Value = toml::from_str(file).unwrap();
        let (mut entries, _) = pesde::entries(&file, DEFAULT_INDEX_URL, "acme/rocket").unwrap();
        entries.sort_by(|a, b| {
            b.version
                .cmp(&a.version)
                .then_with(|| a.target.cmp(&b.target))
        });
        (entries, Meta::of(&file))
    }

    // the keys that the registry writes, from a real entry and API.md
    const ENTRY: &str = r#"
["2.0.0 lune-alloy"]
name = "acme/rocket"
version = "2.0.0"
target = "lune"
type = "lune"
language = "alloy"
description = "Fast rockets"
repository = "https://github.com/acme/rocket"
homepage = "https://rocket.acme.dev"
download = "https://cdn.emberpm.dev/acme/rocket/2.0.0/lune-alloy.tar.gz"
published_by = "acme"
published_at = "2026-10-21T18:30:00Z"
yanked = true

["1.1.0 lune-alloy"]
name = "acme/rocket"
version = "1.1.0"
target = "lune"
type = "lune"
language = "alloy"
description = "Fast rockets"
repository = "https://github.com/acme/rocket"
homepage = "https://rocket.acme.dev"
download = "https://cdn.emberpm.dev/acme/rocket/1.1.0/lune-alloy.tar.gz"
published_by = "acme"
published_at = "2026-10-20T18:30:00Z"
deprecated = "use 2.0"

["1.1.0 lune-alloy".dependencies.Signal]
name = "acme/signal"
version = "^1.0.0"
features = ["fast"]
default-features = false

["1.1.0 lune-alloy".dependencies.jecs]
name = "ukendio/jecs"
version = "^0.11.0"
index = "wally"

["1.1.0 lune-alloy".optional-dependencies.trace]
name = "acme/trace"
version = "^2.0.0"
optional = true

["1.1.0 lune-alloy".feature-dependencies.icons]
name = "acme/icons"
version = "^1.0.0"

["1.1.0 lune-alloy".features]
default = ["pretty"]
pretty = ["icons", "Signal/fast"]
plain = []

["1.1.0 lune-alloy".feature-files.plain]
entry = "src/plain"
files = ["src/plain", "assets/plain/**"]

["1.1.0 lune-alloy".peer-dependencies.React]
name = "jsdotlua/react"
version = "^17.0.0"

["1.1.0 lune-alloy".dev-dependencies.testkit]
name = "evaera/testkit"
version = "^1.0.0"

["1.1.0 lune-alloy".requires]
alloy = "^0.4.0"
lune = ">=0.9.0"

["1.1.0 roblox"]
name = "acme/rocket"
version = "1.1.0"
target = "roblox"
type = "roblox"
download = "https://cdn.emberpm.dev/acme/rocket/1.1.0/roblox.tar.gz"

["1.0.0 roblox"]
name = "acme/rocket"
version = "1.0.0"
target = "roblox"
type = "roblox"
download = "https://cdn.emberpm.dev/acme/rocket/1.0.0.tar.gz"
"#;

    /// a git or path entry, ember's or pesde's `repo`, names its source in place of a version.
    #[test]
    fn git_and_path_dependencies_name_their_source() {
        let entry: Value = toml::from_str(
            "[dependencies]\n\
             ui = { git = \"https://github.com/acme/ui\", tag = \"v2.0.0\" }\n\
             lib = { path = \"../lib\" }\n\
             old = { repo = \"https://github.com/acme/old\", rev = \"abc1234\" }\n",
        )
        .unwrap();
        let (runtime, _) = dependencies(&entry);
        assert_eq!(
            runtime,
            [
                ("lib".to_string(), "path ../lib".to_string()),
                (
                    "old".to_string(),
                    "git https://github.com/acme/old rev abc1234".to_string()
                ),
                (
                    "ui".to_string(),
                    "git https://github.com/acme/ui tag v2.0.0".to_string()
                ),
            ]
        );
        // --json gives the source as data, and no name or version that the entry lacks
        let (runtime, _) = dependencies_json(&entry);
        assert_eq!(
            runtime[0]["source"],
            json!({ "type": "path", "path": "../lib" })
        );
        assert_eq!(
            runtime[2]["source"],
            json!({ "type": "git", "url": "https://github.com/acme/ui", "rev": null, "tag": "v2.0.0", "branch": null })
        );
        assert_eq!(
            (&runtime[2]["name"], &runtime[2]["version"]),
            (&json!(null), &json!(null))
        );
        let document = local_document(
            "acme/lib",
            &DownloadSource::Path {
                path: "../lib".to_string(),
            },
            &toml::from_str("[package]\nname = \"acme/lib\"\nversion = \"0.1.0\"\n").unwrap(),
        );
        assert_eq!(
            document["source"],
            json!({ "type": "path", "path": "../lib" })
        );
        assert_eq!(document["shown"]["version"], "0.1.0");
        assert_eq!(document["shown"]["dependencies"], json!([]));
        // a package with no file features has an empty table, as with features
        assert_eq!(document["shown"]["feature_files"], json!({}));
    }

    #[test]
    fn info_shows_the_newest_open_version_and_its_details() {
        let (entries, meta) = entries(ENTRY);
        // 2.0.0 is yanked, so 1.1.0 is the one an install takes
        let shown = entries.iter().find(|entry| !entry.yanked).unwrap();
        let text = plain(&describe(
            "acme/rocket",
            &entries,
            shown,
            &meta,
            "embr add acme/rocket",
        ));
        assert_eq!(
            text,
            "acme/rocket 1.1.0\n\
             Fast rockets\n\
             Deprecated: use 2.0\n\
             \n\
             Repository  https://github.com/acme/rocket\n\
             Homepage    https://rocket.acme.dev\n\
             Published   2026-10-20 by acme\n\
             Targets     lune (alloy), roblox\n\
             Requires    alloy ^0.4.0, lune >=0.9.0\n\
             \n\
             Dependencies\n  \
               Signal  acme/signal ^1.0.0 [fast] (no default features)\n  \
               jecs    ukendio/jecs ^0.11.0 (index wally)\n  \
               trace   acme/trace ^2.0.0 (optional)\n  \
               icons   acme/icons ^1.0.0 (feature pretty)\n  \
               React   jsdotlua/react ^17.0.0 (peer)\n\
             \n\
             Features\n  \
               plain   - (files: src/plain, assets/plain/**; entry: src/plain)\n  \
               pretty  icons, Signal/fast (default)\n\
             \n\
             Dev dependencies\n  \
               testkit  evaera/testkit ^1.0.0\n\
             \n\
             Versions\n  \
               2.0.0  lune (alloy) yanked\n  \
               1.1.0  lune (alloy), roblox deprecated\n  \
               1.0.0  roblox\n\
             \n\
             Add it with `embr add acme/rocket`\n"
        );
    }

    #[test]
    fn a_pesde_entry_gives_its_license_and_authors_and_the_list_stops_at_ten() {
        let mut file = String::new();
        for minor in 0..12 {
            file.push_str(&format!(
                "[\"0.{minor}.0 luau\"]\nlicense = \"MIT\"\nauthors = [\"Erica <e@x.dev>\"]\n\
                 published_at = \"2024-11-12T16:16:28.286Z\"\n\
                 [\"0.{minor}.0 luau\".target]\nenvironment = \"luau\"\n\
                 [\"0.{minor}.0 luau\".dependencies]\n\
                 frktest = [{{ name = \"itsfrank/frktest\", version = \"^0.0.2\" }}, \"dev\"]\n\
                 option = [{{ name = \"lukadev_0/option\", version = \"^1.2.0\" }}, \"peer\"]\n"
            ));
        }
        let (entries, _) = entries(&file);
        let text = plain(&describe(
            "acme/rocket",
            &entries,
            &entries[0],
            &Meta::default(),
            "embr add x",
        ));
        assert!(text.starts_with("acme/rocket 0.11.0\n\nLicense    MIT\nAuthors    Erica <e@x.dev>\nPublished  2024-11-12\n"), "{text}");
        assert!(
            text.contains("Dependencies\n  option  lukadev_0/option ^1.2.0 (peer)\n"),
            "{text}"
        );
        assert!(
            text.contains("Dev dependencies\n  frktest  itsfrank/frktest ^0.0.2\n"),
            "{text}"
        );
        assert!(text.contains("  0.2.0   luau\n  and 2 more\n"), "{text}");
        assert!(!text.contains("0.1.0"), "{text}");
    }

    #[test]
    fn info_names_the_replacement_and_every_notice_with_its_id() {
        let file = r#"
["1.0.0 roblox"]
version = "1.0.0"
target = "roblox"

[meta]
deprecated = "no longer kept up"
replacement = "acme/jet"

[[meta.notices]]
id = "aaaaaaaaaaaa"
level = "warning"
message = "1.x leaks memory\nupdate to 2.0"
versions = "<2.0.0"

[[meta.notices]]
id = "bbbbbbbbbbbb"
level = "notice"
message = "docs moved\u001b[2J"

[[meta.notices]]
id = "cccccccccccc"
level = "security"
message = "1.0.0 sends the place file to a remote host"
versions = "=1.0.0"
"#;
        let (entries, meta) = entries(file);
        let text = plain(&describe(
            "acme/rocket",
            &entries,
            &entries[0],
            &meta,
            "embr add x",
        ));
        assert!(
            text.contains(
                "Deprecated: no longer kept up. Use acme/jet instead\n\
                 Warning aaaaaaaaaaaa for <2.0.0: 1.x leaks memory\n  update to 2.0\n\
                 Notice bbbbbbbbbbbb: docs moved\n\
                 Security cccccccccccc for =1.0.0: 1.0.0 sends the place file to a remote host\n"
            ),
            "{text}"
        );
    }

    /// the `--json` shape of `embr info`.
    #[test]
    fn the_json_details_hold_the_shown_version_and_every_version() {
        let file = format!(
            "{ENTRY}\n[meta]\ndeprecated = \"use acme/jet\"\nreplacement = \"acme/jet\"\n\n\
             [[meta.notices]]\nid = \"aaaaaaaaaaaa\"\nlevel = \"security\"\n\
             message = \"1.1.0 runs a miner\"\nversions = \"=1.1.0\"\n\
             posted_by = \"acme\"\nposted_at = \"2026-11-02T12:00:00Z\"\n"
        );
        let (entries, meta) = entries(&file);
        let shown = entries.iter().find(|entry| !entry.yanked).unwrap();
        let target = |environment: &str, language: &str| json!({ "environment": environment, "language": language });
        let dependency = |alias: &str, name: &str, version: &str, index: Option<&str>| {
            json!({
                "alias": alias,
                "name": name,
                "version": version,
                "source": null,
                "index": index,
                "features": [],
                "default_features": true,
                "optional": false,
                "peer": false,
            })
        };
        let mut signal = dependency("Signal", "acme/signal", "^1.0.0", None);
        signal["features"] = json!(["fast"]);
        signal["default_features"] = json!(false);
        let mut trace = dependency("trace", "acme/trace", "^2.0.0", None);
        trace["optional"] = json!(true);
        let mut react = dependency("React", "jsdotlua/react", "^17.0.0", None);
        react["peer"] = json!(true);
        let flagged =
            |environment: &str, language: &str, yanked: bool, deprecated: Option<&str>| {
                json!({
                    "environment": environment,
                    "language": language,
                    "yanked": yanked,
                    "deprecated": deprecated,
                })
            };
        assert_eq!(
            document("acme/rocket", &entries, shown, &meta),
            json!({
                "version": 1,
                "name": "acme/rocket",
                "source": null,
                "deprecated": "use acme/jet",
                "replacement": "acme/jet",
                "notices": [{
                    "id": "aaaaaaaaaaaa",
                    "level": "security",
                    "message": "1.1.0 runs a miner",
                    "versions": "=1.1.0",
                    "posted_by": "acme",
                    "posted_at": "2026-11-02T12:00:00Z",
                }],
                "shown": {
                    "version": "1.1.0",
                    "yanked": false,
                    "deprecated": "use 2.0",
                    "description": "Fast rockets",
                    "license": null,
                    "repository": "https://github.com/acme/rocket",
                    "homepage": "https://rocket.acme.dev",
                    "authors": [],
                    "published_at": "2026-10-20T18:30:00Z",
                    "published_by": "acme",
                    "targets": [target("lune", "alloy"), target("roblox", "luau")],
                    "requires": { "alloy": "^0.4.0", "lune": ">=0.9.0" },
                    "dependencies": [
                        signal,
                        dependency("jecs", "ukendio/jecs", "^0.11.0", Some("wally")),
                        trace,
                        dependency("icons", "acme/icons", "^1.0.0", None),
                        react,
                    ],
                    "features": {
                        "default": ["pretty"],
                        "plain": [],
                        "pretty": ["icons", "Signal/fast"],
                    },
                    "feature_files": {
                        "plain": { "entry": "src/plain", "files": ["src/plain", "assets/plain/**"] },
                    },
                    "dev_dependencies": [
                        dependency("testkit", "evaera/testkit", "^1.0.0", None),
                    ],
                },
                "versions": [
                    { "version": "2.0.0", "targets": [flagged("lune", "alloy", true, None)] },
                    {
                        "version": "1.1.0",
                        "targets": [
                            flagged("lune", "alloy", false, Some("use 2.0")),
                            flagged("roblox", "luau", false, None),
                        ],
                    },
                    { "version": "1.0.0", "targets": [flagged("roblox", "luau", false, None)] },
                ],
            })
        );
    }

    #[test]
    fn a_yanked_version_shows_only_when_nothing_else_matches() {
        let (entries, _) = entries(ENTRY);
        let text = plain(&describe(
            "acme/rocket",
            &entries,
            &entries[0],
            &Meta::default(),
            "embr add x",
        ));
        assert!(text.starts_with("acme/rocket 2.0.0 (yanked)\n"), "{text}");
        assert!(text.ends_with("finds none to install\n"), "{text}");
    }
}
