use crate::error::Error;
use crate::project::manifest::Environment;
use crate::registry::index::DownloadSource;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const LOCKFILE: &str = "ember.lock";

/// the lockfile inside `dir`.
pub fn lockfile_in(dir: &Path) -> PathBuf {
    dir.join(LOCKFILE)
}

/// The lockfile to read in the current directory.
pub fn lockfile_path() -> PathBuf {
    lockfile_in(Path::new(""))
}

/** the lockfile format that this embr writes, and the newest one it reads.
1.x keeps it at 2: a new key is additive, and an older reader ignores it. */
pub const FORMAT: u32 = 2;

/// exactly what `embr install` resolved, so `--locked` can replay it without touching any index.
#[derive(Serialize, Deserialize, Debug)]
pub struct Lockfile {
    pub version: u32,
    /** what ember.toml asked for at the install that wrote this lock, as
    [`crate::project::manifest::Manifest::dependency_hash`] has it.
    `--locked` refuses to install when ember.toml asks for something else
    now. additive within v2: a lock from before it has none, and `--locked`
    then says so and installs. */
    #[serde(
        rename = "manifest-hash",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub manifest_hash: Option<String>,
    #[serde(default, rename = "package")]
    pub packages: Vec<LockedPackage>,
    /** the release assets of the project's [tools] that an install has
    downloaded, each with its SHA-256. a GitHub release can be replaced
    under the same version, and this is what notices. additive within v2. */
    #[serde(default, rename = "tool", skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<LockedTool>,
}

/** one release asset of a pinned tool. a tool has one asset for each
platform, so a lock that two platforms have written holds two entries for
the tool. a tool that npm publishes has none: npm checks its own. */
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockedTool {
    /// the tool as [tools] names it, "owner/repo" or "ember:scope/name".
    pub name: String,
    pub version: String,
    /// the file name of the release asset.
    pub asset: String,
    /// lowercase hex SHA-256 of the asset as it was downloaded.
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LockedPackage {
    pub name: String,
    /// empty for a path dependency, whose folder is the source and has no pin.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// the package's own environment. names the nested-link folder
    /// dependents require it through.
    pub environment: Environment,
    /** which environment root the package installs under, the root of the
    dependency tree that pulled it in. absent when it equals `environment`,
    also absent in pre-v2 lockfiles which knew no such distinction. read it
    through [`LockedPackage::context`]. */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Environment>,
    /// top-level link file name, absent for transitives which get none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub index: String,
    /** this package's [overrides]-rewritten edges, declared alias -> the
    replacement's package name, and its path or git edges without `name`.
    additive, locks without it parse fine, and `--locked` needs it to
    relink redirected dependencies. */
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub redirects: std::collections::BTreeMap<String, String>,
    /** the patch this copy was built with, when [patches] named it.
    additive within v2 like `redirects`. `--locked` replays it straight
    from here without consulting the manifest. */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchRecord>,
    /** only optional dependencies lead here. `--locked` skips it when its
    download fails, as the install that wrote the lock did. */
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /** its enabled features: the union over its dependents in this
    environment root, with the features those turn on. absent with none.
    the nested-link pass reads it to leave the gated dependencies that no
    enabled feature lists without a link. install reads it to delete the
    files of each file feature that is off, and the archive's ember.toml
    says which files those are, so the lock needs no key for them. */
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /** the channel that picked `version`, when a `channel` dependency
    resolved it. an install keeps `version` while ember.toml names this
    channel, and `embr update` reads the channel again. `--locked` does not
    read it. additive within v2. */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(flatten)]
    pub source: DownloadSource,
    /** lowercase hex SHA-256 of the archive: the index entry's, else the
    one the first install computed. later installs and `--locked` refuse
    an archive with another hash. additive within v2, a lock from before
    it gets the hashes on its next install. none for workspace members,
    path folders and the local registry, and none for a git commit, whose
    `rev` is its integrity. */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/** a patch as the lockfile remembers it, the repo-relative file plus a
hex FNV-1a of its bytes, so an edited patch reads as a change even when
every version resolved identically. */
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PatchRecord {
    pub path: String,
    pub hash: String,
}

impl PatchRecord {
    pub fn new(path: String, bytes: &[u8]) -> Self {
        PatchRecord {
            path,
            hash: format!("{:016x}", crate::sys::hash::fnv1a(bytes)),
        }
    }

    /// whether `bytes` are the bytes this record was made from.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        self.hash == format!("{:016x}", crate::sys::hash::fnv1a(bytes))
    }
}

/// `scope/name@version` in output, or the name alone when there is no version.
pub fn package_id(name: &str, version: &str) -> String {
    if version.is_empty() {
        name.to_string()
    } else {
        format!("{name}@{version}")
    }
}

impl LockedPackage {
    /// see [`package_id`].
    pub fn id(&self) -> String {
        package_id(&self.name, &self.version)
    }

    /// the environment root this package installs under.
    pub fn context(&self) -> Environment {
        self.context.unwrap_or(self.environment)
    }
}

impl Lockfile {
    pub fn new(packages: Vec<LockedPackage>) -> Self {
        Lockfile {
            /* in v2 `link` went optional, absent on transitives, and `context`
            arrived. v1 locks load fine. */
            version: FORMAT,
            manifest_hash: None,
            packages,
            tools: Vec::new(),
        }
    }

    /// this lock, written for the ember.toml with the dependency hash `hash`.
    pub fn for_manifest(mut self, hash: String) -> Self {
        self.manifest_hash = Some(hash);
        self
    }

    /// this lock with the tool assets `tools`, in a fixed order.
    pub fn with_tools(mut self, mut tools: Vec<LockedTool>) -> Self {
        tools.sort();
        tools.dedup();
        self.tools = tools;
        self
    }

    pub fn load() -> Result<Self, Error> {
        let path = lockfile_path();
        if !path.exists() {
            return Err(Error::LockfileMissing);
        }
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /** reads a lockfile's text. a format above [`FORMAT`] is refused before
    the parse, so a newer embr's lock is never read wrong, and never
    written back in the older format. */
    pub fn parse(text: &str) -> Result<Self, Error> {
        let raw: toml::Table = text.parse()?;
        if let Some(version) = raw.get("version").and_then(toml::Value::as_integer)
            && version > i64::from(FORMAT)
        {
            return Err(Error::LockfileNewer(version));
        }
        let lock: Lockfile = toml::from_str(text)?;
        lock.check()?;
        Ok(lock)
    }

    /** refuses the values that embr never writes and that would reach git
    or a path. a lockfile is text that a pull request can change, and few
    people read its diff. ember.toml gets the same checks when it loads,
    but `--locked` installs from here alone. */
    fn check(&self) -> Result<(), Error> {
        for package in &self.packages {
            /* the name and the version become folder names: the name in
            the package store, and both in the working copy of `embr patch`.
            a registry name is scope/name. a workspace member, a path folder
            and a git package say their own name */
            let registry = matches!(
                package.source,
                DownloadSource::Zip { .. } | DownloadSource::TarGz { .. }
            );
            let named = if registry {
                crate::project::manifest::split_package_name(&package.name).is_ok()
            } else {
                crate::sys::paths::store_folder(&package.name).is_ok()
            };
            if !named {
                return Err(Error::LockfileInvalid(format!(
                    "has the package name '{}', which is not the name of a package",
                    package.name
                )));
            }
            // a workspace member or a path folder has no pin, so it may have no version
            let unpinned = package.version.is_empty() && package.source.in_place().is_some();
            if !unpinned && semver::Version::parse(&package.version).is_err() {
                return Err(Error::LockfileInvalid(format!(
                    "has the version '{}' for {}, which is not a version",
                    package.version, package.name
                )));
            }
            if let DownloadSource::Git { url, rev, .. } = &package.source {
                crate::registry::index::git::check_url(url).map_err(|_| {
                    Error::LockfileInvalid(format!(
                        "has the git URL '{url}' for {}, which is not an https or ssh URL",
                        package.name
                    ))
                })?;
                // git would read a leading dash as an option, and a commit is hex
                if rev.is_empty() || !rev.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(Error::LockfileInvalid(format!(
                        "has the git commit '{rev}' for {}, which is not a commit id",
                        package.name
                    )));
                }
            }
            if let Some(sha256) = &package.sha256
                && !crate::sys::hash::is_sha256_hex(sha256)
            {
                return Err(Error::LockfileInvalid(format!(
                    "has a sha256 for {} that is not 64 lowercase hex characters",
                    package.name
                )));
            }
        }
        for tool in &self.tools {
            if !crate::sys::hash::is_sha256_hex(&tool.sha256) {
                return Err(Error::LockfileInvalid(format!(
                    "has a sha256 for the tool {} that is not 64 lowercase hex characters",
                    tool.name
                )));
            }
        }
        Ok(())
    }

    /** writes ember.lock through a temp file beside it and a rename. a run
    that stops in the middle of the write then leaves the old lock, whole,
    and not the first half of the new one.

    the temp file has a random name and must not exist yet. a name from
    the pid can be a link that a repository ships, and the write then goes
    through it to a file outside the project. */
    pub fn save(&self) -> Result<(), Error> {
        use std::io::Write;

        let header = "# This file is generated by `embr install`; do not edit it manually.\n";
        let body = toml::to_string(self).expect("lockfile serializes");
        let staging = PathBuf::from(format!(
            "{LOCKFILE}.tmp-{}",
            crate::sys::paths::random_hex()?
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        file.write_all(format!("{header}{body}").as_bytes())
            .and_then(|()| std::fs::rename(&staging, LOCKFILE))
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&staging);
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /** each key that embr writes to ember.lock. the "ember.lock format" page
    on the site documents each one. a new key fails
    `every_written_key_is_known` until it is here, so add it to that page
    in the same change. a key is a promise for all of 1.x. */
    const KNOWN_FIELDS: &[&str] = &[
        "version",
        "package",
        "package.name",
        "package.version",
        "package.environment",
        "package.context",
        "package.link",
        "package.index",
        "package.redirects",
        "package.patch",
        "package.patch.path",
        "package.patch.hash",
        "package.optional",
        "package.features",
        "package.channel",
        "package.type",
        "package.url",
        "package.path",
        "package.rev",
        "package.branch",
        "package.tag",
        "package.sha256",
        "manifest-hash",
        "tool",
        "tool.name",
        "tool.version",
        "tool.asset",
        "tool.sha256",
    ];

    /// the dotted path of each key in `table`. a map of names, such as `redirects`, is one key.
    fn keys(prefix: &str, table: &toml::Table, out: &mut std::collections::BTreeSet<String>) {
        for (key, value) in table {
            let path = format!("{prefix}{key}");
            match value {
                toml::Value::Table(inner) if key != "redirects" => {
                    keys(&format!("{path}."), inner, out)
                }
                toml::Value::Array(items) if key == "package" || key == "tool" => {
                    for item in items.iter().filter_map(toml::Value::as_table) {
                        keys(&format!("{path}."), item, out);
                    }
                }
                _ => {}
            }
            out.insert(path);
        }
    }

    /** a package with every field set, once for each source, so that each
    key that embr can write shows. */
    #[test]
    fn every_written_key_is_known() {
        let git = |branch: Option<&str>, tag: Option<&str>| DownloadSource::Git {
            url: "https://github.com/acme/ui".to_string(),
            rev: "0123456789abcdef0123456789abcdef01234567".to_string(),
            branch: branch.map(str::to_string),
            tag: tag.map(str::to_string),
        };
        let sources = [
            DownloadSource::Zip {
                url: "https://example.com/a.zip".to_string(),
            },
            DownloadSource::TarGz {
                url: "https://example.com/a.tar.gz".to_string(),
            },
            DownloadSource::Workspace {
                path: "../core".to_string(),
            },
            DownloadSource::Path {
                path: "../lib".to_string(),
            },
            git(Some("main"), None),
            git(None, Some("v1.0.0")),
        ];
        let packages = sources
            .into_iter()
            .map(|source| LockedPackage {
                name: "acme/thing".to_string(),
                version: "1.0.0".to_string(),
                environment: Environment::Roblox,
                context: Some(Environment::Server),
                link: Some("thing".to_string()),
                index: "https://example.com/index".to_string(),
                redirects: [("dep".to_string(), "acme/fork".to_string())].into(),
                patch: Some(PatchRecord::new("patches/x.patch".to_string(), b"x")),
                optional: true,
                features: vec!["fast".to_string()],
                channel: Some("beta".to_string()),
                source,
                sha256: Some(crate::sys::hash::sha256_hex(b"thing")),
            })
            .collect();
        let lock = Lockfile::new(packages)
            .for_manifest(crate::sys::hash::sha256_hex(b"manifest"))
            .with_tools(vec![LockedTool {
                name: "rojo-rbx/rojo".to_string(),
                version: "7.4.4".to_string(),
                asset: "rojo-7.4.4-linux-x86_64.zip".to_string(),
                sha256: crate::sys::hash::sha256_hex(b"rojo"),
            }]);
        let text = toml::to_string(&lock).unwrap();
        // what embr writes, embr reads
        Lockfile::parse(&text).unwrap();
        let written: toml::Table = toml::from_str(&text).unwrap();
        let mut found = std::collections::BTreeSet::new();
        keys("", &written, &mut found);
        let known: std::collections::BTreeSet<String> =
            KNOWN_FIELDS.iter().map(|key| key.to_string()).collect();
        assert_eq!(
            found, known,
            "a lockfile key is not in KNOWN_FIELDS, or a known one is never written"
        );
    }

    #[test]
    fn round_trips() {
        let lockfile = Lockfile::new(vec![
            LockedPackage {
                name: "evaera/promise".to_string(),
                version: "4.0.0".to_string(),
                environment: Environment::Roblox,
                // a roblox package pulled in under a server root
                context: Some(Environment::Server),
                link: Some("Promise".to_string()),
                index: "https://github.com/UpliftGames/wally-index".to_string(),
                redirects: [("Signal".to_string(), "acme/signal-fork".to_string())].into(),
                patch: Some(PatchRecord::new(
                    "patches/evaera_promise@4.0.0.patch".to_string(),
                    b"--- a/init.lua\n+++ b/init.lua\n",
                )),
                optional: false,
                features: Vec::new(),
                channel: None,
                source: DownloadSource::Zip {
                    url: "https://api.wally.run/v1/package-contents/evaera/promise/4.0.0"
                        .to_string(),
                },
                sha256: Some(crate::sys::hash::sha256_hex(b"promise")),
            },
            LockedPackage {
                name: "pesde/hello".to_string(),
                version: "1.0.2".to_string(),
                environment: Environment::Luau,
                // a transitive in its own environment, no link, no context
                context: None,
                link: None,
                index: "https://github.com/pesde-pkg/index".to_string(),
                redirects: Default::default(),
                patch: None,
                // only an optional dependency brings it in
                optional: true,
                features: vec!["icons".to_string()],
                channel: None,
                source: DownloadSource::TarGz {
                    url: "https://registry.pesde.daimond113.com/v1/packages/pesde%2Fhello/1.0.2/luau/archive".to_string(),
                },
                sha256: None,
            },
        ]);

        let text = toml::to_string(&lockfile).unwrap();
        // the portable environment keeps the name that older embr reads
        assert!(text.contains("environment = \"luau\""), "{text}");
        assert!(!text.contains("portable"), "{text}");
        let parsed: Lockfile = toml::from_str(&text).unwrap();
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.packages.len(), 2);
        assert_eq!(parsed.packages[0].name, "evaera/promise");
        // redirects round-trip when present and stay absent otherwise
        assert_eq!(
            parsed.packages[0]
                .redirects
                .get("Signal")
                .map(String::as_str),
            Some("acme/signal-fork")
        );
        assert!(parsed.packages[1].redirects.is_empty());
        assert_eq!(text.matches("redirects").count(), 1);

        /* the patch record round-trips where present, stays absent
        otherwise, and an edited patch stops matching */
        let record = parsed.packages[0].patch.as_ref().unwrap();
        assert_eq!(record.path, "patches/evaera_promise@4.0.0.patch");
        assert!(record.matches(b"--- a/init.lua\n+++ b/init.lua\n"));
        assert!(!record.matches(b"something else"));
        assert!(parsed.packages[1].patch.is_none());
        assert_eq!(text.matches("[package.patch]").count(), 1);
        assert!(matches!(
            &parsed.packages[1].source,
            DownloadSource::TarGz { url } if url.contains("pesde%2Fhello")
        ));

        // context and link round-trip, absent fields staying absent
        assert_eq!(parsed.packages[0].context(), Environment::Server);
        assert_eq!(parsed.packages[0].link.as_deref(), Some("Promise"));
        assert_eq!(parsed.packages[1].context(), Environment::Luau);
        assert_eq!(parsed.packages[1].link, None);
        assert_eq!(text.matches("context").count(), 1);
        assert_eq!(text.matches("link").count(), 1);
        // `optional` is written only where it holds
        assert!(!parsed.packages[0].optional);
        assert!(parsed.packages[1].optional);
        assert_eq!(text.matches("optional").count(), 1);
        // so is `features`
        assert!(parsed.packages[0].features.is_empty());
        assert_eq!(parsed.packages[1].features, ["icons"]);
        assert_eq!(text.matches("features").count(), 1);

        // the hash round-trips beside the url, and stays absent otherwise
        assert_eq!(
            parsed.packages[0].sha256.as_deref(),
            Some(crate::sys::hash::sha256_hex(b"promise").as_str())
        );
        assert_eq!(parsed.packages[1].sha256, None);
        assert_eq!(text.matches("sha256 = ").count(), 1);
    }

    /** a v2 lock from before the hashes loads with none, and an old embr
    reads a lock with them, since it ignores keys it does not know. */
    #[test]
    fn locks_with_and_without_hashes_load() {
        let entry = "version = 2\n\n\
             [[package]]\n\
             name = \"acme/thing\"\n\
             version = \"1.0.0\"\n\
             environment = \"roblox\"\n\
             index = \"https://example.com/index\"\n\
             type = \"tar-gz\"\n\
             url = \"https://example.com/thing/1.0.0\"\n";
        let old: Lockfile = toml::from_str(entry).unwrap();
        assert_eq!(old.packages[0].sha256, None);

        let hash = crate::sys::hash::sha256_hex(b"thing");
        let new: Lockfile = toml::from_str(&format!("{entry}sha256 = \"{hash}\"\n")).unwrap();
        assert_eq!(new.packages[0].sha256.as_deref(), Some(hash.as_str()));

        /// the fields of 0.13's LockedPackage, which knew no hash.
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Old {
            name: String,
            version: String,
            environment: Environment,
            index: String,
            #[serde(flatten)]
            source: DownloadSource,
        }
        #[derive(Deserialize)]
        struct OldLock {
            package: Vec<Old>,
        }
        let text = toml::to_string(&new).unwrap();
        let read: OldLock = toml::from_str(&text).unwrap();
        assert!(
            matches!(&read.package[0].source, DownloadSource::TarGz { url } if url.ends_with("1.0.0"))
        );
    }

    /** a path entry keeps the folder and no version. a git entry keeps the
    commit and the branch that ember.toml asked for. */
    #[test]
    fn path_and_git_entries_keep_their_source() {
        let text = "version = 2\n\n\
             [[package]]\n\
             name = \"acme/lib\"\n\
             environment = \"roblox\"\n\
             link = \"lib\"\n\
             index = \"path\"\n\
             type = \"path\"\n\
             path = \"../lib\"\n\n\
             [[package]]\n\
             name = \"acme/ui\"\n\
             version = \"1.2.0\"\n\
             environment = \"roblox\"\n\
             link = \"ui\"\n\
             index = \"git\"\n\
             type = \"git\"\n\
             url = \"https://github.com/acme/ui\"\n\
             rev = \"0123456789abcdef0123456789abcdef01234567\"\n\
             branch = \"main\"\n";
        let parsed: Lockfile = toml::from_str(text).unwrap();
        assert_eq!(parsed.packages[0].id(), "acme/lib");
        assert_eq!(
            parsed.packages[0].source,
            DownloadSource::Path {
                path: "../lib".to_string()
            }
        );
        assert_eq!(parsed.packages[1].id(), "acme/ui@1.2.0");
        assert_eq!(
            parsed.packages[1].source.local_label().as_deref(),
            Some("git https://github.com/acme/ui 0123456")
        );
        let written = toml::to_string(&parsed).unwrap();
        assert_eq!(written.matches("version = ").count(), 2, "{written}");
        let again: Lockfile = toml::from_str(&written).unwrap();
        assert_eq!(again.packages[1].source, parsed.packages[1].source);
    }

    #[test]
    fn server_locks_still_load_and_write_roblox_server() {
        let old: Lockfile = toml::from_str(
            "version = 2\n\n\
             [[package]]\n\
             name = \"acme/svc\"\n\
             version = \"1.0.0\"\n\
             environment = \"server\"\n\
             context = \"server\"\n\
             index = \"https://example.com/index\"\n\
             type = \"tar-gz\"\n\
             url = \"https://example.com/svc/1.0.0\"\n",
        )
        .unwrap();
        assert_eq!(old.packages[0].environment, Environment::Server);
        assert_eq!(old.packages[0].context(), Environment::Server);

        let text = toml::to_string(&old).unwrap();
        assert!(text.contains(r#"environment = "roblox_server""#), "{text}");
        assert!(!text.contains(r#""server""#), "{text}");
    }

    #[test]
    fn a_lockfile_from_a_newer_format_is_refused() {
        let error = Lockfile::parse("version = 3\n\n[[package]]\nname = \"acme/x\"\n").unwrap_err();
        assert!(matches!(error, Error::LockfileNewer(3)), "{error}");
        assert!(Lockfile::parse("version = 2\n").is_ok());
    }

    #[test]
    fn v1_lockfiles_still_load() {
        /* the pre-context format, version 1, `link` on every entry, no
        `context` anywhere. context() falls back to the environment. */
        let parsed: Lockfile = toml::from_str(
            "version = 1\n\n\
             [[package]]\n\
             name = \"acme/thing\"\n\
             version = \"1.0.0\"\n\
             environment = \"shared\"\n\
             link = \"thing\"\n\
             index = \"https://example.com/index\"\n\
             type = \"zip\"\n\
             url = \"https://example.com/thing/1.0.0\"\n",
        )
        .unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.packages[0].link.as_deref(), Some("thing"));
        assert_eq!(parsed.packages[0].context(), Environment::Roblox);
    }

    fn git_lock(url: &str, rev: &str) -> String {
        format!(
            "version = 2\n\n[[package]]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n\
             environment = \"luau\"\nindex = \"git\"\ntype = \"git\"\n\
             url = \"{url}\"\nrev = \"{rev}\"\n"
        )
    }

    /** `--locked` hands the URL and the commit of a git package to git
    with no look at ember.toml, so the lock itself has to refuse the ones
    that the manifest check refuses. */
    #[test]
    fn a_git_source_that_embr_never_writes_is_refused() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        assert!(Lockfile::parse(&git_lock("https://github.com/acme/lib", commit)).is_ok());
        for url in [
            "ext::sh -c id",
            "--upload-pack=id",
            "file:///etc",
            "http://github.com/acme/lib",
        ] {
            let error = Lockfile::parse(&git_lock(url, commit)).unwrap_err();
            assert!(
                matches!(&error, Error::LockfileInvalid(reason) if reason.contains("git URL")),
                "{url}: {error}"
            );
        }
        for rev in ["--output=/tmp/x", "", "main", "HEAD~1"] {
            let error = Lockfile::parse(&git_lock("https://github.com/acme/lib", rev)).unwrap_err();
            assert!(
                matches!(&error, Error::LockfileInvalid(reason) if reason.contains("commit")),
                "{rev}: {error}"
            );
        }
    }

    /** the name and the version of a package become folder names, in the
    store and in the working copy of `embr patch`, so the lock refuses the
    ones that no install writes. */
    #[test]
    fn a_name_or_a_version_that_is_no_folder_name_is_refused() {
        let lock = |name: &str, version: &str, source: &str| {
            format!(
                "version = 2\n\n[[package]]\nname = '{name}'\nversion = '{version}'\n\
                 environment = \"luau\"\nindex = \"x\"\n{source}\n"
            )
        };
        let registry = "type = \"tar-gz\"\nurl = \"https://example.com/a\"";
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let git = format!("type = \"git\"\nurl = \"https://github.com/a/b\"\nrev = \"{commit}\"");
        let path = "type = \"path\"\npath = \"../lib\"";

        assert!(Lockfile::parse(&lock("acme/lib", "1.0.0", registry)).is_ok());
        assert!(Lockfile::parse(&lock("rocks/penlight", "1.14.0+3", registry)).is_ok());
        assert!(Lockfile::parse(&lock("acme/lib", "1.0.0-rc.1", &git)).is_ok());
        // a git or path package says its own name, which may have no scope
        assert!(Lockfile::parse(&lock("mylib", "0.0.0", &git)).is_ok());
        assert!(Lockfile::parse(&lock("mylib", "", path)).is_ok());

        for name in ["a\\..\\..\\..\\.git\\hooks", "..", "C:x", "nul"] {
            let error = Lockfile::parse(&lock(name, "1.0.0", &git)).unwrap_err();
            assert!(
                matches!(&error, Error::LockfileInvalid(reason) if reason.contains("package name")),
                "{name}: {error}"
            );
        }
        for name in ["lib", "Acme/Lib", "acme/lib/x", "acme/..", "acme\\lib"] {
            assert!(
                Lockfile::parse(&lock(name, "1.0.0", registry)).is_err(),
                "{name}"
            );
        }
        for version in ["1.0.0/../../../X", "", "..", "1.0", "1.0.0\\x"] {
            let error = Lockfile::parse(&lock("acme/lib", version, registry)).unwrap_err();
            assert!(
                matches!(&error, Error::LockfileInvalid(reason) if reason.contains("version")),
                "{version}: {error}"
            );
        }
        // only a workspace member or a path folder has no version
        assert!(Lockfile::parse(&lock("acme/lib", "", &git)).is_err());
    }

    #[test]
    fn a_hash_that_is_not_a_sha256_is_refused() {
        let tool = |sha256: &str| {
            format!(
                "version = 2\n\n[[tool]]\nname = \"rojo-rbx/rojo\"\nversion = \"7.4.4\"\n\
                 asset = \"rojo.zip\"\nsha256 = \"{sha256}\"\n"
            )
        };
        assert!(Lockfile::parse(&tool(&crate::sys::hash::sha256_hex(b"x"))).is_ok());
        assert!(matches!(
            Lockfile::parse(&tool("abc")),
            Err(Error::LockfileInvalid(_))
        ));
    }

    /// an embr from before the keys reads a lock that has them, and the other way round.
    #[test]
    fn the_new_keys_are_optional() {
        let old = Lockfile::parse("version = 2\n").unwrap();
        assert_eq!(old.manifest_hash, None);
        assert!(old.tools.is_empty());
        let text = toml::to_string(&Lockfile::new(Vec::new())).unwrap();
        assert!(
            !text.contains("manifest-hash") && !text.contains("tool"),
            "{text}"
        );
    }
}
