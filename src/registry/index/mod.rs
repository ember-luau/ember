pub mod pesde;
pub mod wally;

use crate::error::Error;
use crate::net::http;
use crate::project::manifest::Environment;
use crate::sys::git;
use crate::sys::hash::{fnv1a, fnv1a_parts};
use crate::sys::paths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/** How stale an index cache may be before `open` shells out to git again.
Every refresh *attempt* stamps the cache dir — attempts, not successes, so
an offline machine warns once per window instead of re-hanging on git for
every install. */
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refresh {
    /// never refresh an existing cache (a cold cache still clones)
    Never,
    /// refresh unless the cache was refreshed within the TTL
    Ttl,
    /// refresh no matter what (`--refresh`)
    Force,
}

/// default TTL; `LPM_INDEX_TTL_SECS` overrides it, `0` meaning always refresh.
const DEFAULT_INDEX_TTL: Duration = Duration::from_secs(5 * 60);

/** computed once per process: it never changes mid-run, and a malformed
value should warn exactly once, not per index. a garbage value falls back
to the default *with* a warning — silently ignoring "-1" or "30s" would
read as the variable doing nothing. */
fn index_ttl() -> Duration {
    static TTL: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *TTL.get_or_init(|| match std::env::var("LPM_INDEX_TTL_SECS") {
        Err(_) => DEFAULT_INDEX_TTL,
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(seconds) => Duration::from_secs(seconds),
            Err(_) => {
                eprintln!(
                    "warning: LPM_INDEX_TTL_SECS is '{value}', not a whole number of seconds; using the default ({}s)",
                    DEFAULT_INDEX_TTL.as_secs()
                );
                DEFAULT_INDEX_TTL
            }
        },
    })
}

/** A package index: a git repo cached under ~/.lpm/index-cache. Root
config.json means wally, root config.toml means pesde. */
pub struct Index {
    url: String,
    root: PathBuf,
    kind: Kind,
    /// a requested refresh was skipped because the cache was TTL-fresh
    ttl_skipped: bool,
}

enum Kind {
    Wally(wally::Config),
    Pesde(pesde::Config),
}

/// A concrete package version picked from an index.
pub struct ResolvedPackage {
    pub version: semver::Version,
    /** Known from index metadata; None means inspect the archive after
    extraction (lpm.toml -> pesde.toml -> wally.toml). */
    pub environment: Option<Environment>,
    pub dependencies: Vec<TransitiveDependency>,
    pub source: DownloadSource,
}

pub struct TransitiveDependency {
    pub name: String,
    pub version_req: String,
    /// None = resolve in the same index as the parent package.
    pub index_url: Option<String>,
    /// the alias the parent declared this dependency under; [overrides] paths address edges by it.
    pub alias: String,
}

/** Everything needed to re-download a resolved package; also stored in
lpm.lock. Resolution bakes the full URL so --locked installs never consult
an index. */
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum DownloadSource {
    /// Zip archive (wally registry APIs).
    Zip { url: String },
    /// Gzipped tarball (pesde registry APIs).
    TarGz { url: String },
    /** A workspace member, linked in place instead of downloaded; `path` is
    the member's directory relative to the consuming project. */
    Workspace { path: String },
}

impl Index {
    /** Opens an index, cloning or refreshing its git cache per `refresh`.
    If the refresh fails but a cached copy exists, the stale copy is used
    with a warning. */
    pub fn open(url: &str, refresh: Refresh) -> Result<Self, Error> {
        let (root, ttl_skipped) = ensure_cached(url, refresh)?;
        let kind = if root.join("config.json").exists() {
            Kind::Wally(wally::load_config(&root)?)
        } else if root.join("config.toml").exists() {
            Kind::Pesde(pesde::load_config(&root)?)
        } else {
            /* Empty or half-set-up index repo; a raw io error here would
            read as an lpm bug rather than an index problem. */
            return Err(Error::IndexFetch {
                url: url.to_string(),
                reason: "the index has no config.json or config.toml at its root".to_string(),
            });
        };

        Ok(Index {
            url: url.to_string(),
            root,
            kind,
            ttl_skipped,
        })
    }

    /** whether a requested refresh was skipped because the cache was still
    TTL-fresh — the signal resolution failures use to retry against a
    forced refresh before surfacing an error. */
    pub fn ttl_skipped(&self) -> bool {
        self.ttl_skipped
    }

    /** GitHub OAuth client id publishes authenticate against. Lives in the
    index's config, not the binary. Wally indices carry one too, but lpm
    never publishes to wally, so only pesde-format configs are consulted. */
    pub fn github_oauth_id(&self) -> Option<&str> {
        match &self.kind {
            Kind::Wally(_) => None,
            Kind::Pesde(config) => config.github_oauth_id.as_deref(),
        }
    }

    /** Finds the highest version of `name` matching `req`. For pesde
    indices, `prefer_environment` picks between a version's targets. */
    pub fn resolve(
        &self,
        name: &str,
        req: &semver::VersionReq,
        prefer_environment: Option<Environment>,
    ) -> Result<ResolvedPackage, Error> {
        match &self.kind {
            Kind::Wally(config) => wally::resolve(&self.root, &self.url, config, name, req),
            Kind::Pesde(config) => {
                pesde::resolve(&self.root, &self.url, config, name, req, prefer_environment)
            }
        }
    }
}

/** Downloads and extracts a resolved package into `dest`. All downloads are
anonymous: lpm's registry serves tarballs from a public CDN, wally/pesde
registries are public too. If credentialed downloads ever come back, restore
the old restraint: tokens only to GitHub-owned hosts, so an arbitrary
registry url in an index entry can't harvest them. */
pub fn download(source: &DownloadSource, dest: &Path, cache: CachePolicy) -> Result<(), Error> {
    let (DownloadSource::Zip { url } | DownloadSource::TarGz { url }) = source else {
        // Workspace members are linked in place; install never gets here.
        return Err(Error::IndexFetch {
            url: String::new(),
            reason: "workspace dependencies are linked in place, not downloaded".to_string(),
        });
    };

    let mut headers: Vec<(&str, &str)> = Vec::new();
    if matches!(source, DownloadSource::Zip { .. }) {
        headers.push(("Wally-Version", wally::VERSION_HEADER));
    }

    std::fs::create_dir_all(dest)?;
    let (bytes, from_cache) = fetch_archive(url, &headers, cache)?;
    match extract(source, &bytes, dest) {
        Ok(()) => Ok(()),
        /* cached bytes that no longer extract are poison (bitrot the
        integrity record missed, or a bad write): drop the entry, fetch
        fresh once, and try again on a clean dest */
        Err(_) if from_cache => {
            evict_archive(url);
            fs::remove_dir_all(dest)?;
            fs::create_dir_all(dest)?;
            let (bytes, _) = fetch_archive(url, &headers, CachePolicy::Bypass)?;
            extract(source, &bytes, dest)
        }
        Err(error) => Err(error),
    }
}

/** which side of the archive cache a download takes. `--refresh` bypasses,
which also overwrites the entry with what the registry serves now. */
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CachePolicy {
    Use,
    Bypass,
}

fn extract(source: &DownloadSource, bytes: &[u8], dest: &Path) -> Result<(), Error> {
    match source {
        DownloadSource::Zip { .. } => {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            archive.extract(dest)?;
        }
        DownloadSource::TarGz { .. } => {
            /* ureq already gunzips Content-Encoding: gzip bodies (leaving a
            raw tar), so sniff the magic bytes instead of assuming. */
            if bytes.starts_with(&[0x1f, 0x8b]) {
                let decoder = flate2::read::GzDecoder::new(bytes);
                tar::Archive::new(decoder).unpack(dest)?;
            } else {
                tar::Archive::new(bytes).unpack(dest)?;
            }
        }
        DownloadSource::Workspace { .. } => unreachable!("rejected by download"),
    }
    Ok(())
}

/** the archive's bytes, from the cache when allowed and present. published
registry versions are immutable by policy (registries reject duplicate
publishes), which is what makes caching by URL sound; `--refresh` is the
escape hatch for the exceptions (takedowns, republished storage), and
`lpm cache clean` drops everything. the bool is "these came from the
cache", which decides whether an extract failure warrants a refetch. */
fn fetch_archive(
    url: &str,
    headers: &[(&str, &str)],
    cache: CachePolicy,
) -> Result<(Vec<u8>, bool), Error> {
    let entry = archive_entry(url);

    if cache == CachePolicy::Use
        && let Some(entry) = &entry
        && let Some(bytes) = load_archive(entry, url)
    {
        return Ok((bytes, true));
    }

    let started = std::time::Instant::now();
    let bytes = http::get_bytes(url, headers)?;
    crate::ui::timing(&format!("download {url}"), started);

    /* caching is best-effort: a full disk or permission problem must not
    fail an install that already has the bytes in hand. no size budget or
    eviction yet (`lpm cache clean` is the release valve); an
    extracted-file store shared across projects, pesde-style, is the
    natural next step if this ever needs more */
    if let Some(entry) = &entry {
        let _ = store_archive(entry, &bytes, url);
    }
    Ok((bytes, false))
}

/** where `url`'s archive lives in the cache; None when there's no home
directory to cache under. readable slug + stable FNV hash, same naming
scheme as the index cache — but never DefaultHasher, whose output may
change between Rust releases and would orphan every entry. */
fn archive_entry(url: &str) -> Option<PathBuf> {
    let slug: String = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("archive")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(40)
        .collect();
    let dir = paths::archive_cache_dir().ok()?;
    Some(dir.join(format!("{slug}-{:016x}", fnv1a(url.as_bytes()))))
}

/** cached bytes, verified against the sidecar record: length, FNV, and the
url itself, so a filename-key collision can never serve another package's
archive. any mismatch or unreadable half deletes the entry and reads as a
miss. */
fn load_archive(entry: &Path, url: &str) -> Option<Vec<u8>> {
    let meta = fs::read_to_string(meta_path(entry)).ok()?;
    let bytes = fs::read(entry).ok()?;

    // "v1 <len> <hash> <url>"; the url comes last since it can contain anything
    let mut fields = meta.trim_end().splitn(4, ' ');
    let valid = fields.next() == Some("v1")
        && fields.next().and_then(|len| len.parse::<usize>().ok()) == Some(bytes.len())
        && fields.next() == Some(format!("{:016x}", fnv1a(&bytes)).as_str())
        && fields.next() == Some(url);
    if !valid {
        evict_entry(entry);
        return None;
    }
    Some(bytes)
}

/** temp-file + rename, so a torn write can never be mistaken for an entry.
the sidecar lands before the payload: a reader catching the pair mid-swap
sees a mismatch, treats it as a miss, and refetches — wasteful, never
wrong. */
fn store_archive(entry: &Path, bytes: &[u8], url: &str) -> std::io::Result<()> {
    let dir = entry.parent().expect("cache entries have a parent");
    fs::create_dir_all(dir)?;

    let meta = format!("v1 {} {:016x} {url}\n", bytes.len(), fnv1a(bytes));
    replace_file(&meta_path(entry), meta.as_bytes())?;
    replace_file(entry, bytes)
}

/** write-to-staging + rename. staging names carry pid and a counter, so
concurrent writers — other processes or this one's worker threads — can't
tear each other's half-written files. */
fn replace_file(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let staging = paths::with_suffix(
        target,
        &format!(
            ".tmp-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ),
    );
    fs::write(&staging, contents)?;
    fs::rename(&staging, target).inspect_err(|_| {
        let _ = fs::remove_file(&staging);
    })
}

fn evict_archive(url: &str) {
    if let Some(entry) = archive_entry(url) {
        evict_entry(&entry);
    }
}

fn evict_entry(entry: &Path) {
    let _ = fs::remove_file(entry);
    let _ = fs::remove_file(meta_path(entry));
}

fn meta_path(entry: &Path) -> PathBuf {
    paths::with_suffix(entry, ".meta")
}

/** existing index caches were keyed with DefaultHasher, whose output is
stable in practice within a compiler release series but documented as
unstable; fnv1a_parts with a marker keeps the same "slug-hash" shape under
a deliberate, stable function. old-key caches are simply re-cloned once. */
pub(crate) fn cache_dir(url: &str) -> Result<PathBuf, Error> {
    let slug: String = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("index")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();

    let key = fnv1a_parts(&[b"index-cache", url.as_bytes()]);
    Ok(paths::index_cache_dir()?.join(format!("{slug}-{key:016x}")))
}

fn ensure_cached(url: &str, refresh: Refresh) -> Result<(PathBuf, bool), Error> {
    let dir = cache_dir(url)?;
    let stamp = dir.join(REFRESH_STAMP);

    if dir.join(".git").exists() {
        let wanted = match refresh {
            Refresh::Never => false,
            Refresh::Force => true,
            Refresh::Ttl => !stamp_fresh(&stamp, index_ttl()),
        };
        if refresh == Refresh::Ttl && !wanted {
            return Ok((dir, true));
        }
        if wanted {
            let started = std::time::Instant::now();
            let result = git::run(&["-C", &dir.to_string_lossy(), "pull", "--ff-only"]);
            crate::ui::timing(&format!("index-refresh {url}"), started);
            /* stamp the attempt, success or not: an unreachable remote
            should warn once per TTL window, not hang every install */
            touch(&stamp);
            if let Err(reason) = result {
                eprintln!("warning: could not refresh index {url} ({reason}); using cached copy");
            }
        }
        return Ok((dir, false));
    }

    std::fs::create_dir_all(dir.parent().expect("cache dir has a parent"))?;
    let started = std::time::Instant::now();
    git::run(&["clone", "--depth", "1", url, &dir.to_string_lossy()]).map_err(|reason| {
        Error::IndexFetch {
            url: url.to_string(),
            reason,
        }
    })?;
    crate::ui::timing(&format!("index-clone {url}"), started);
    touch(&stamp);
    Ok((dir, false))
}

/** whether `url`'s index cache was refreshed within the TTL — what
install's fast path asks before trusting a lockfile without resolving:
inside the window, re-resolving against the same index state could only
reproduce the lock, so skipping is free; past it, `^` requirements deserve
a real look. */
pub fn is_fresh(url: &str) -> bool {
    cache_dir(url).is_ok_and(|dir| stamp_fresh(&dir.join(REFRESH_STAMP), index_ttl()))
}

/// marks when an index cache last attempted a refresh (see `Refresh`).
const REFRESH_STAMP: &str = ".lpm-refreshed";

/// whether `stamp` exists and is younger than `ttl`. a zero ttl is never fresh.
fn stamp_fresh(stamp: &Path, ttl: Duration) -> bool {
    if ttl.is_zero() {
        return false;
    }
    fs::metadata(stamp)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age < ttl)
}

/** (re)writes `stamp` so its mtime is now. the payload matters: writing
zero bytes over an already-empty file is a no-op Windows won't bump the
mtime for, which would freeze the stamp at its creation time forever.
best-effort: worst case is an extra pull. */
fn touch(stamp: &Path) {
    let _ = fs::write(stamp, b"refreshed\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_cache_verifies_what_it_returns() {
        let dir = std::env::temp_dir().join("lpm-test-archive-cache");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("thing-0123");
        let url = "https://example.com/thing/1.0.0";

        // roundtrip
        store_archive(&entry, b"payload", url).unwrap();
        assert_eq!(load_archive(&entry, url), Some(b"payload".to_vec()));

        // an entry recorded for another url is a miss, not a wrong answer
        assert_eq!(
            load_archive(&entry, "https://example.com/other/2.0.0"),
            None
        );

        // tampered payload reads as a miss and evicts both halves
        store_archive(&entry, b"payload", url).unwrap();
        fs::write(&entry, b"payload!").unwrap();
        assert_eq!(load_archive(&entry, url), None);
        assert!(!entry.exists());
        assert!(!meta_path(&entry).exists());

        // tampered sidecar likewise
        store_archive(&entry, b"payload", url).unwrap();
        fs::write(meta_path(&entry), format!("v1 7 0000000000000000 {url}\n")).unwrap();
        assert_eq!(load_archive(&entry, url), None);
        assert!(!entry.exists());

        // payload without a sidecar is not an entry
        fs::write(&entry, b"orphan").unwrap();
        assert_eq!(load_archive(&entry, url), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stamps_age_out_by_ttl() {
        let dir = std::env::temp_dir().join("lpm-test-refresh-stamp");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let stamp = dir.join(REFRESH_STAMP);

        // missing stamp is never fresh
        assert!(!stamp_fresh(&stamp, Duration::from_secs(300)));
        touch(&stamp);
        assert!(stamp_fresh(&stamp, Duration::from_secs(300)));
        // a zero ttl means "always refresh", however new the stamp
        assert!(!stamp_fresh(&stamp, Duration::ZERO));

        /* re-touching an EXISTING stamp must renew it. this is where a
        zero-byte write bites on Windows: no bytes, no truncation, no
        mtime update — the stamp would age out once and stay expired */
        let file = fs::OpenOptions::new().write(true).open(&stamp).unwrap();
        file.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        drop(file);
        assert!(!stamp_fresh(&stamp, Duration::from_secs(300)));
        touch(&stamp);
        assert!(
            stamp_fresh(&stamp, Duration::from_secs(300)),
            "touch must renew an existing stamp's mtime"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /** the WS1 contract, against a real local git index: a TTL-fresh cache
    skips the pull (and serves stale data, by design), a resolve that fails
    on that stale data forces one refresh and succeeds, and Force always
    pulls. */
    #[test]
    fn ttl_skips_pulls_and_failed_resolves_force_one() {
        /* the assertions below assume the default TTL; a dev running the
        suite with the knob exported gets a skip, not a bogus failure */
        if std::env::var_os("LPM_INDEX_TTL_SECS").is_some() {
            return;
        }

        /// removes the fixture dirs even when an assertion panics.
        struct Cleanup(Vec<PathBuf>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for dir in &self.0 {
                    let _ = fs::remove_dir_all(dir);
                }
            }
        }

        let origin = std::env::temp_dir().join("lpm-test-ttl-origin");
        let _ = fs::remove_dir_all(&origin);
        fs::create_dir_all(origin.join("acme")).unwrap();
        let url = origin.to_string_lossy().replace('\\', "/");
        // same url = same cache dir; start every run from nothing
        let cache = cache_dir(&url).unwrap();
        let _ = fs::remove_dir_all(&cache);
        let _cleanup = Cleanup(vec![origin.clone(), cache.clone()]);

        let git = |args: &[&str]| {
            let mut full = vec![
                "-C",
                origin.to_str().unwrap(),
                "-c",
                "user.name=lpm-test",
                "-c",
                "user.email=lpm-test@localhost",
            ];
            full.extend_from_slice(args);
            git::run(&full).unwrap();
        };
        let publish = |version: &str| {
            let entry = format!(
                "{{\"package\":{{\"name\":\"acme/thing\",\"version\":\"{version}\",\"realm\":\"shared\",\"registry\":\"\"}}}}\n"
            );
            let path = origin.join("acme/thing");
            let mut lines = fs::read_to_string(&path).unwrap_or_default();
            lines.push_str(&entry);
            fs::write(&path, lines).unwrap();
        };

        fs::write(
            origin.join("config.json"),
            r#"{"api":"https://example.com"}"#,
        )
        .unwrap();
        publish("1.0.0");
        git(&["init"]);
        git(&["add", "."]);
        git(&["commit", "-m", "v1"]);

        // first open clones; nothing was skipped
        let index = Index::open(&url, Refresh::Ttl).unwrap();
        assert!(!index.ttl_skipped());
        let any = semver::VersionReq::STAR;
        assert_eq!(
            index.resolve("acme/thing", &any, None).unwrap().version,
            semver::Version::new(1, 0, 0)
        );

        // the origin moves on
        publish("2.0.0");
        git(&["add", "."]);
        git(&["commit", "-m", "v2"]);

        // within the TTL the pull is skipped: still v1, and v2 can't resolve
        let index = Index::open(&url, Refresh::Ttl).unwrap();
        assert!(index.ttl_skipped());
        assert_eq!(
            index.resolve("acme/thing", &any, None).unwrap().version,
            semver::Version::new(1, 0, 0)
        );
        let two = semver::VersionReq::parse("^2.0.0").unwrap();
        assert!(index.resolve("acme/thing", &two, None).is_err());

        /* the resolver-level guardrail: a failing resolve on a skipped
        index earns one forced refresh and then succeeds */
        let manifest: crate::project::manifest::Manifest = toml::from_str(&format!(
            "[package]\nname = \"acme/consumer\"\nversion = \"0.1.0\"\n\n\
             [indices]\ndefault = \"{url}\"\n\n\
             [dependencies]\nthing = {{ name = \"acme/thing\", version = \"^2.0.0\" }}\n"
        ))
        .unwrap();
        // project_dir only matters for workspace deps; origin keeps it hermetic
        let resolved =
            crate::registry::resolver::resolve(&manifest, &origin, Refresh::Ttl, &mut Vec::new())
                .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].version, semver::Version::new(2, 0, 0));

        // Force always pulls, fresh stamp or not
        let index = Index::open(&url, Refresh::Force).unwrap();
        assert!(!index.ttl_skipped());
        assert_eq!(
            index.resolve("acme/thing", &two, None).unwrap().version,
            semver::Version::new(2, 0, 0)
        );
    }

    #[test]
    fn cache_dirs_are_per_url() {
        let Ok(wally) = cache_dir("https://github.com/UpliftGames/wally-index") else {
            return; // no home directory on this machine
        };
        let pesde = cache_dir("https://github.com/pesde-pkg/index").unwrap();

        assert!(wally.starts_with(paths::index_cache_dir().unwrap()));
        /* Readable half of the folder name comes from the url's last
        segment; the hash keeps same-named indices apart. */
        assert!(
            wally
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("wally-index-")
        );
        assert_ne!(wally, pesde);
        assert_eq!(
            pesde,
            cache_dir("https://github.com/pesde-pkg/index").unwrap()
        );
    }
}
