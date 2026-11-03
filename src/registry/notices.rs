/*! Package notices, deprecations and yanks, as `embr install`, `embr add`
and `embr notices` show them.

The owners of a package post a notice on the whole package or on a semver
range of its versions. It lives in the package's index file, beside
pesde's `[meta] deprecated`:

```toml
[meta]
deprecated = "no longer kept up"
replacement = "acme/jet"

[[meta.notices]]
id = "3f9c0a1b2c4d"
level = "warning"            # or "notice", or "security"
message = "1.x leaks memory"
versions = ">=1.0.0, <2.0.0" # absent: every version
posted_by = "evaera"
posted_at = "2026-11-02T12:00:00Z"
```

So an install reads them from the index copy that it already has, with
no request of its own. A private or unlisted package gets them from the
API, as it gets its versions. */

use crate::error::Error;
use crate::project::lockfile::LockedPackage;
use crate::project::manifest::DEFAULT_INDEX_URL;
use crate::registry::index::{self, DownloadSource, Index, Refresh, forest, hidden, pesde};
use crate::registry::local;
use crate::ui;
use crossterm::style::Stylize;
use std::collections::{BTreeSet, HashMap};
use toml::Value;

/** how loud a notice is. a warning and a security notice show on every
install. a security notice names a known vulnerability or a malicious
version, and `embr audit` fails on it. */
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Notice,
    Warning,
    Security,
}

/// one `[[meta.notices]]` entry.
#[derive(Debug)]
pub struct Notice {
    pub id: String,
    pub level: Level,
    pub message: String,
    /// the range as the registry wrote it, None for every version.
    pub versions: Option<String>,
    range: Option<semver::VersionReq>,
    pub posted_by: String,
    pub posted_at: String,
}

impl Notice {
    /** one notice of the file. a level this embr does not know reads as a
    warning, so a newer, louder level never goes quiet. a range that does
    not parse drops the notice, since it is not clear what it covers. */
    fn of(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
        let versions = text("versions");
        let range = match &versions {
            Some(versions) => Some(semver::VersionReq::parse(versions).ok()?),
            None => None,
        };
        Some(Notice {
            id: text("id").unwrap_or_default(),
            level: match text("level").as_deref() {
                Some("notice") => Level::Notice,
                Some("security") => Level::Security,
                _ => Level::Warning,
            },
            message: text("message").filter(|message| !message.trim().is_empty())?,
            versions,
            range,
            posted_by: text("posted_by").unwrap_or_default(),
            posted_at: text("posted_at").unwrap_or_default(),
        })
    }

    /** whether the notice covers `version`, by the semver crate's rule,
    as for a dependency: a prerelease is in a range only when the range
    names a prerelease of the same version. no range covers every version. */
    pub fn covers(&self, version: &semver::Version) -> bool {
        self.range
            .as_ref()
            .is_none_or(|range| range.matches(version))
    }
}

/// a package file's `[meta]` table: the package-wide data.
#[derive(Debug, Default)]
pub struct Meta {
    pub deprecated: Option<String>,
    pub replacement: Option<String>,
    pub notices: Vec<Notice>,
}

impl Meta {
    /** the `[meta]` of a parsed package file. pesde writes an empty
    string for "not deprecated", so only text that is not blank counts. */
    pub fn of(file: &Value) -> Self {
        let meta = file.get("meta");
        let text = |key: &str| {
            meta.and_then(|meta| meta.get(key))
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(str::to_string)
        };
        let notices = meta
            .and_then(|meta| meta.get("notices"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Notice::of)
            .collect();
        Meta {
            deprecated: text("deprecated"),
            replacement: text("replacement"),
            notices,
        }
    }
}

/// what one line says. the order is the order of the install block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Security,
    Warning,
    Deprecated,
    Yanked,
    Notice,
}

impl Kind {
    /// the label of a line, also the `kind` of a `--json` item.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Security => "security",
            Kind::Warning => "warning",
            Kind::Deprecated => "deprecated",
            Kind::Yanked => "yanked",
            Kind::Notice => "notice",
        }
    }
}

/// one line of the install block or of `embr notices`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Item {
    pub kind: Kind,
    /// scope/name, lowercase.
    pub package: String,
    /// the installed version when the item is about it, None when it is
    /// about the whole package.
    pub version: Option<String>,
    /// the message or the reason. empty for a yank.
    pub text: String,
    /// the package to use in place of a deprecated one.
    pub replacement: Option<String>,
}

impl Item {
    /// the message or the reason as embr prints it, with no escape sequence.
    pub fn message(&self) -> String {
        ui::strip_controls(&self.text).trim().to_string()
    }

    /** the line, from the kind on: `warning: scope/name@1.2.0: text`. the
    package part is left out when `with_package` is false, for a list that
    heads each package. a message of more lines goes on under the first,
    two spaces in. */
    pub fn line(&self, with_package: bool) -> String {
        let label = self.kind.as_str();
        let mut text = match self.kind {
            Kind::Yanked => "the owners withdrew this version, so new installs skip it".to_string(),
            _ => self.message(),
        };
        if let Some(replacement) = &self.replacement {
            // "Old.", so not "Old., use"
            let reason = text.trim_end_matches('.');
            let replacement = ui::strip_controls(replacement);
            text = format!("{reason}, use {replacement} instead");
        }
        let text = text.replace('\n', "\n  ");
        let line = match (with_package, &self.version) {
            (false, _) => format!("{label}: {text}"),
            (true, Some(version)) => format!("{label}: {}@{version}: {text}", self.package),
            (true, None) => format!("{label}: {}: {text}", self.package),
        };
        match self.kind {
            Kind::Security => line.red().to_string(),
            Kind::Warning | Kind::Deprecated | Kind::Yanked => line.yellow().to_string(),
            Kind::Notice => line,
        }
    }
}

/** the items of `name` at `version` from its package `file`: the
deprecations, a yank, and the notices that cover the version. `download`
is the URL that the lock names, which picks the installed target out of
the version's targets; None takes every target of the version. */
pub fn items(
    name: &str,
    version: &semver::Version,
    download: Option<&str>,
    file: &Value,
    index_url: &str,
) -> Vec<Item> {
    let package = name.to_lowercase();
    let at = Some(version.to_string());
    let item = |kind, version: Option<String>, text: &str, replacement: Option<String>| Item {
        kind,
        package: package.clone(),
        version,
        text: text.to_string(),
        replacement,
    };
    let meta = Meta::of(file);
    let mut out = Vec::new();
    if let Some(reason) = &meta.deprecated {
        out.push(item(
            Kind::Deprecated,
            None,
            reason,
            meta.replacement.clone(),
        ));
    }

    let (candidates, _) = pesde::entries(file, index_url, name).unwrap_or_default();
    let of_version = |exact: bool| {
        candidates
            .iter()
            .filter(|candidate| candidate.version == *version)
            .filter(|candidate| {
                !exact || candidate.entry.get("download").and_then(Value::as_str) == download
            })
            .collect::<Vec<_>>()
    };
    let mut targets = of_version(download.is_some());
    if targets.is_empty() {
        targets = of_version(false);
    }
    for candidate in &targets {
        if let Some(reason) = &candidate.deprecated {
            out.push(item(Kind::Deprecated, at.clone(), reason, None));
        }
    }
    if targets.iter().any(|candidate| candidate.yanked) {
        out.push(item(Kind::Yanked, at.clone(), "", None));
    }

    for notice in meta.notices.iter().filter(|notice| notice.covers(version)) {
        let kind = match notice.level {
            Level::Security => Kind::Security,
            Level::Warning => Kind::Warning,
            Level::Notice => Kind::Notice,
        };
        let version = notice.versions.as_ref().and(at.clone());
        out.push(item(kind, version, &notice.message, None));
    }
    out.dedup();
    out
}

/** an index to read notices from. `fetch` false reads only a copy that is
on disk already, with no request: an install that resolved nothing must
not reach the network for this. Forest keeps no package files. */
fn open(url: &str, fetch: bool) -> Option<Index> {
    if forest::is_forest(url) {
        return None;
    }
    let on_disk =
        url == local::NAME || index::cache_dir(url).is_ok_and(|dir| dir.join(".git").exists());
    match (fetch, on_disk) {
        (true, _) => Index::open(url, Refresh::Ttl).ok(),
        (false, true) => Index::open(url, Refresh::Never).ok(),
        (false, false) => None,
    }
}

/** the package file of `name` in `index`. a private or unlisted package
is not in ember's git index, so with `fetch` the API answers for it, when
embr holds a token or a link key for it. */
fn file(index: &Index, url: &str, name: &str, fetch: bool) -> Result<Option<Value>, Error> {
    match index.file(name) {
        Err(Error::PackageNotFound { .. }) if fetch && url == DEFAULT_INDEX_URL => {
            hidden::file(name)
        }
        Err(Error::PackageNotFound { .. }) => Ok(None),
        found => found,
    }
}

/** the items of each locked package, in lock order. a package whose index
cannot be read gets none, and `problems` says why: notices never fail the
command that shows them. */
pub fn gather(
    packages: &[LockedPackage],
    fetch: bool,
    problems: &mut Vec<String>,
) -> Vec<Vec<Item>> {
    let mut indices: HashMap<&str, Option<Index>> = HashMap::new();
    packages
        .iter()
        .map(|package| {
            let download = match &package.source {
                DownloadSource::Workspace { .. } => return Vec::new(),
                DownloadSource::Zip { url } | DownloadSource::TarGz { url } => url.as_str(),
            };
            let Ok(version) = semver::Version::parse(&package.version) else {
                return Vec::new();
            };
            let url = package.index.as_str();
            let Some(index) = indices.entry(url).or_insert_with(|| open(url, fetch)) else {
                return Vec::new();
            };
            match file(index, url, &package.name, fetch) {
                Ok(Some(file)) => items(&package.name, &version, Some(download), &file, url),
                Ok(None) => Vec::new(),
                Err(error) => {
                    problems.push(format!("{}: {error}", package.name));
                    Vec::new()
                }
            }
        })
        .collect()
}

/** the install block: every security notice, warning and deprecation,
and the notices of the packages that this install added or changed, so
a repeat install stays quiet about them. a yank is left out, the
resolver says it when it keeps a yanked pin. one line for each item,
also when a package sits in two environments. */
pub fn install_lines(items: &[Vec<Item>], fresh: &BTreeSet<String>) -> Vec<String> {
    let shown: BTreeSet<&Item> = items
        .iter()
        .flatten()
        .filter(|item| match item.kind {
            Kind::Yanked => false,
            Kind::Notice => fresh.contains(&item.package),
            Kind::Security | Kind::Warning | Kind::Deprecated => true,
        })
        .collect();
    shown.into_iter().map(|item| item.line(true)).collect()
}

/** prints the install block for `installed`, the lock that the install
left, against `previous`, the lock before it. `fetch` as for [`gather`].
`peers` are the peer warnings, which come last. */
pub fn report(
    previous: &[LockedPackage],
    installed: &[LockedPackage],
    fetch: bool,
    peers: &[String],
) {
    let fresh: BTreeSet<String> = installed
        .iter()
        .filter(|package| {
            !previous.iter().any(|old| {
                old.name.eq_ignore_ascii_case(&package.name) && old.version == package.version
            })
        })
        .map(|package| package.name.to_lowercase())
        .collect();
    let items = gather(installed, fetch, &mut Vec::new());
    let mut lines = install_lines(&items, &fresh);
    lines.extend(
        peers
            .iter()
            .map(|line| ui::printable(line).yellow().to_string()),
    );
    if !lines.is_empty() {
        println!();
        for line in lines {
            println!("{line}");
        }
    }
}

/** prints the items of one package that `embr add` just added. all of
them show, since the package is new to the project. */
pub fn report_added(index_url: &str, name: &str, version: &semver::Version) {
    let Some(index) = open(index_url, true) else {
        return;
    };
    let Ok(Some(file)) = file(&index, index_url, name, true) else {
        return;
    };
    let added = items(name, version, None, &file, index_url);
    let fresh = BTreeSet::from([name.to_lowercase()]);
    let lines = install_lines(&[added], &fresh);
    if !lines.is_empty() {
        println!();
        for line in lines {
            println!("{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
["1.0.0 roblox"]
version = "1.0.0"
target = "roblox"
download = "https://cdn/acme/rocket/1.0.0.tar.gz"
deprecated = "1.0.0 drops events"

["1.0.0 lune"]
version = "1.0.0"
target = "lune"
download = "https://cdn/acme/rocket/1.0.0/lune.tar.gz"
yanked = true

["1.2.0 roblox"]
version = "1.2.0"
target = "roblox"
download = "https://cdn/acme/rocket/1.2.0.tar.gz"

[meta]
deprecated = "no longer kept up."
replacement = "acme/jet"

[[meta.notices]]
id = "aaaaaaaaaaaa"
level = "warning"
message = "1.x leaks memory\nupdate to 2.0"
versions = ">=1.0.0, <1.2.0"
posted_by = "evaera"
posted_at = "2026-11-02T12:00:00Z"

[[meta.notices]]
id = "bbbbbbbbbbbb"
level = "notice"
message = "docs moved to rocket.dev"

[[meta.notices]]
id = "cccccccccccc"
level = "critical"
message = "a level from a newer registry"
versions = "^2"

[[meta.notices]]
id = "dddddddddddd"
level = "warning"
message = "a range that does not parse"
versions = "one point two"

[[meta.notices]]
id = "eeeeeeeeeeee"
level = "security"
message = "1.0.0 sends the place file to a remote host"
versions = "=1.0.0"
"#;

    fn file() -> Value {
        toml::from_str(FILE).unwrap()
    }

    fn v(text: &str) -> semver::Version {
        semver::Version::parse(text).unwrap()
    }

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

    fn lines(items: &[Item]) -> Vec<String> {
        items.iter().map(|item| plain(&item.line(true))).collect()
    }

    #[test]
    fn meta_reads_the_deprecation_the_replacement_and_the_notices() {
        let meta = Meta::of(&file());
        assert_eq!(meta.deprecated.as_deref(), Some("no longer kept up."));
        assert_eq!(meta.replacement.as_deref(), Some("acme/jet"));
        // the notice with a range that does not parse drops
        let ids: Vec<&str> = meta.notices.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "aaaaaaaaaaaa",
                "bbbbbbbbbbbb",
                "cccccccccccc",
                "eeeeeeeeeeee"
            ]
        );
        assert_eq!(meta.notices[0].posted_by, "evaera");
        // an unknown level is as loud as a warning
        assert_eq!(meta.notices[2].level, Level::Warning);
        assert_eq!(meta.notices[3].level, Level::Security);
        // a file without [meta], and pesde's empty "not deprecated"
        let none = Meta::of(&toml::from_str("[meta]\ndeprecated = \"\"\n").unwrap());
        assert!(none.deprecated.is_none() && none.notices.is_empty());
    }

    #[test]
    fn a_range_covers_versions_by_the_semver_rule() {
        let notice = |versions: Option<&str>| {
            let mut value = toml::Table::new();
            value.insert("message".into(), "x".into());
            if let Some(versions) = versions {
                value.insert("versions".into(), versions.into());
            }
            Notice::of(&Value::Table(value)).unwrap()
        };
        let range = notice(Some(">=1.0.0, <2.0.0"));
        assert!(range.covers(&v("1.0.0")) && range.covers(&v("1.9.3")));
        assert!(!range.covers(&v("2.0.0")) && !range.covers(&v("0.9.0")));
        // a prerelease only where the range names one of the same version
        assert!(!range.covers(&v("1.5.0-rc.1")));
        assert!(notice(Some(">=1.5.0-rc.1")).covers(&v("1.5.0-rc.1")));
        let caret = notice(Some("^1.2"));
        assert!(caret.covers(&v("1.4.0")) && !caret.covers(&v("1.1.0")));
        // future versions in the range are covered too
        assert!(notice(Some(">=3.0.0")).covers(&v("7.0.0")));
        // no range covers every version, a prerelease too
        assert!(notice(None).covers(&v("0.0.1-alpha")));
    }

    #[test]
    fn items_cover_the_installed_version_and_its_target() {
        let file = file();
        let url = DEFAULT_INDEX_URL;
        let roblox = items(
            "acme/rocket",
            &v("1.0.0"),
            Some("https://cdn/acme/rocket/1.0.0.tar.gz"),
            &file,
            url,
        );
        assert_eq!(
            lines(&roblox),
            [
                "deprecated: acme/rocket: no longer kept up, use acme/jet instead",
                "deprecated: acme/rocket@1.0.0: 1.0.0 drops events",
                "warning: acme/rocket@1.0.0: 1.x leaks memory\n  update to 2.0",
                "notice: acme/rocket: docs moved to rocket.dev",
                "security: acme/rocket@1.0.0: 1.0.0 sends the place file to a remote host",
            ]
        );
        // the lune target of 1.0.0 is the yanked one
        let lune = items(
            "acme/rocket",
            &v("1.0.0"),
            Some("https://cdn/acme/rocket/1.0.0/lune.tar.gz"),
            &file,
            url,
        );
        assert!(lune.iter().any(|item| item.kind == Kind::Yanked));
        assert!(!lune.iter().any(|item| item.text == "1.0.0 drops events"));
        // 1.2.0 is out of the warning's range
        let newer = items("acme/rocket", &v("1.2.0"), None, &file, url);
        assert!(!newer.iter().any(|item| item.kind == Kind::Warning));
    }

    #[test]
    fn the_install_block_quiets_notices_of_packages_that_did_not_change() {
        let file = file();
        let rocket = items("acme/rocket", &v("1.2.0"), None, &file, DEFAULT_INDEX_URL);
        let fresh = BTreeSet::from(["acme/rocket".to_string()]);
        let all = install_lines(std::slice::from_ref(&rocket), &fresh);
        assert_eq!(
            all.iter().map(|line| plain(line)).collect::<Vec<_>>(),
            [
                "deprecated: acme/rocket: no longer kept up, use acme/jet instead",
                "notice: acme/rocket: docs moved to rocket.dev",
            ]
        );
        // a repeat install: the deprecation again, the notice no more
        let again = install_lines(std::slice::from_ref(&rocket), &BTreeSet::new());
        assert_eq!(
            again.iter().map(|line| plain(line)).collect::<Vec<_>>(),
            ["deprecated: acme/rocket: no longer kept up, use acme/jet instead"]
        );
        // one package in two environments prints each line once
        let twice = install_lines(&[rocket.clone(), rocket], &fresh);
        assert_eq!(twice, all);
    }

    #[test]
    fn the_install_block_orders_security_first_and_leaves_out_yanks() {
        let file = file();
        let lune = items(
            "acme/rocket",
            &v("1.0.0"),
            Some("https://cdn/acme/rocket/1.0.0/lune.tar.gz"),
            &file,
            DEFAULT_INDEX_URL,
        );
        let block: Vec<String> = install_lines(&[lune], &BTreeSet::new())
            .iter()
            .map(|line| plain(line))
            .collect();
        assert_eq!(
            block,
            [
                "security: acme/rocket@1.0.0: 1.0.0 sends the place file to a remote host",
                "warning: acme/rocket@1.0.0: 1.x leaks memory\n  update to 2.0",
                "deprecated: acme/rocket: no longer kept up, use acme/jet instead",
            ]
        );
        // a security line is red, so it stands out from the yellow ones
        let security = Item {
            kind: Kind::Security,
            package: "acme/rocket".to_string(),
            version: None,
            text: "x".to_string(),
            replacement: None,
        };
        assert_eq!(
            security.line(true),
            "security: acme/rocket: x".red().to_string()
        );
    }

    /** the index is text that anyone can edit, so a notice that the
    registry would have cleaned may still hold escapes. */
    #[test]
    fn a_line_holds_no_escape_from_the_index() {
        let item = Item {
            kind: Kind::Notice,
            package: "acme/rocket".to_string(),
            version: None,
            text: "ok\r\x1b[2K\x1b[1Afake: all good\x1b]52;c;ZXZpbA==\x07".to_string(),
            replacement: Some("acme/\x1b[31mjet".to_string()),
        };
        assert_eq!(
            item.line(true),
            "notice: acme/rocket: okfake: all good, use acme/jet instead"
        );
        assert_eq!(
            item.line(false),
            "notice: okfake: all good, use acme/jet instead"
        );
    }
}
