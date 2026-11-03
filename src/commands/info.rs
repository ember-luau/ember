use crate::error::Error;
use crate::project::manifest::{
    DEFAULT_INDEX_URL, Language, Manifest, parse_version_req, split_package_name, takes,
};
use crate::registry::index::pesde::{self, Candidate};
use crate::registry::index::{Index, Refresh, hidden};
use crate::registry::notices::{Level, Meta};
use crate::ui;
use clap::Args;
use crossterm::style::Stylize;
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
}

/// the versions listed before "and N more".
const RECENT: usize = 10;

/** shows one package from its index entries. it only reads, and works
outside a project too. */
pub fn run(args: InfoArgs) -> Result<(), Error> {
    let name = args.package.trim().to_lowercase();
    // a bad name fails here, before the index clone
    split_package_name(&name)?;
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

    let (dependencies, dev_dependencies) = dependencies(entry);
    for (title, list) in [
        ("Dependencies", dependencies),
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

/** the (alias, `name requirement`) pairs of an entry's dependencies and
dev dependencies, from the raw entry. an ember entry keeps its dev
dependencies in a table of their own, and a pesde entry marks one `dev`
in a [spec, kind] pair. */
fn dependencies(entry: &Value) -> (Vec<Spec>, Vec<Spec>) {
    let (mut runtime, mut dev) = (Vec::new(), Vec::new());
    for (table, in_dev) in [("dependencies", false), ("dev-dependencies", true)] {
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
            let name = ["name", "wally", "workspace", "repo"]
                .into_iter()
                .find_map(field)
                .unwrap_or("?");
            // pesde writes a wally package name with a "wally#" prefix
            let name = name.strip_prefix("wally#").unwrap_or(name);
            let mut text = format!("{name} {}", field("version").unwrap_or("*"));
            if let Some(index) = field("index") {
                text.push_str(&format!(" (index {index})"));
            }
            if kind == Some("peer") {
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

["1.1.0 lune-alloy".dependencies.jecs]
name = "ukendio/jecs"
version = "^0.11.0"
index = "wally"

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
               Signal  acme/signal ^1.0.0\n  \
               jecs    ukendio/jecs ^0.11.0 (index wally)\n\
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
                 Notice bbbbbbbbbbbb: docs moved\n"
            ),
            "{text}"
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
