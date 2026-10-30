pub mod forest;
pub mod hidden;
pub mod pesde;
pub mod wally;

use crate::error::Error;
use crate::net::{http, registry};
use crate::project::manifest::{DEFAULT_INDEX_URL, Environment, Language};
use crate::registry::local;
use crate::sys::git;
use crate::sys::hash::{fnv1a, fnv1a_parts};
use crate::sys::paths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/** How stale an index cache may be before `open` shells out to git again.
Every refresh *attempt* stamps the cache dir, attempts not successes, so
an offline machine warns once per window instead of re-hanging on git for
every install. */
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refresh {
    /// never refresh an existing cache, a cold cache still clones
    Never,
    /// refresh unless the cache was refreshed within the TTL
    Ttl,
    /// refresh no matter what, `--refresh`
    Force,
}

/// default TTL. `EMBER_INDEX_TTL_SECS` overrides it, `0` meaning always refresh.
const DEFAULT_INDEX_TTL: Duration = Duration::from_secs(5 * 60);

/// the knob's name before the ember rename, still honoured for existing setups.
const LEGACY_TTL_VAR: &str = "LPM_INDEX_TTL_SECS";
const TTL_VAR: &str = "EMBER_INDEX_TTL_SECS";

/** computed once per process. it never changes mid-run, and a malformed
value should warn exactly once, not per index. a garbage value falls back
to the default *with* a warning, silently ignoring "-1" or "30s" would
read as the variable doing nothing. */
pub(crate) fn index_ttl() -> Duration {
    static TTL: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *TTL.get_or_init(|| {
        let set = std::env::var(TTL_VAR)
            .map(|value| (TTL_VAR, value))
            .or_else(|_| std::env::var(LEGACY_TTL_VAR).map(|value| (LEGACY_TTL_VAR, value)));
        match set {
            Err(_) => DEFAULT_INDEX_TTL,
            Ok((name, value)) => match value.trim().parse::<u64>() {
                Ok(seconds) => Duration::from_secs(seconds),
                Err(_) => {
                    eprintln!(
                        "warning: {name} is '{value}', not a whole number of seconds; using the default ({}s)",
                        DEFAULT_INDEX_TTL.as_secs()
                    );
                    DEFAULT_INDEX_TTL
                }
            },
        }
    })
}

/** A package index, a git repo cached under ~/.ember/index-cache. Root
config.json means wally, root config.toml means pesde. Forest's API is the
one index that is no git repo, see [`forest::is_forest`]. */
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
    Forest,
}

/// A concrete package version picked from an index.
pub struct ResolvedPackage {
    pub version: semver::Version,
    /** Known from index metadata. None means inspect the archive after
    extraction, ember.toml -> pesde.toml -> wally.toml. */
    pub environment: Option<Environment>,
    /// from the entry's `language`, luau when it has none.
    pub language: Language,
    /** the entry is yanked. only a version in `pinned` can come back
    yanked, a fresh pick skips them. */
    pub yanked: bool,
    /// the version's own deprecation reason, `deprecated` on its entry.
    pub deprecated: Option<String>,
    /// the whole package's deprecation reason, pesde's `[meta] deprecated`.
    pub package_deprecated: Option<String>,
    pub dependencies: Vec<TransitiveDependency>,
    pub source: DownloadSource,
}

pub struct TransitiveDependency {
    pub name: String,
    pub version_req: String,
    /// None = resolve in the same index as the parent package.
    pub index_url: Option<String>,
    /// the alias the parent declared this dependency under. [overrides] paths address edges by it.
    pub alias: String,
}

/** Everything needed to re-download a resolved package, also stored in
ember.lock. Resolution bakes the full URL so --locked installs never consult
an index. */
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum DownloadSource {
    /// Zip archive, wally registry APIs.
    Zip { url: String },
    /// Gzipped tarball, pesde registry APIs and Forest's CDN.
    TarGz { url: String },
    /** A workspace member, linked in place instead of downloaded. `path` is
    the member's directory relative to the consuming project. */
    Workspace { path: String },
}

impl Index {
    /** Opens an index, cloning or refreshing its git cache per `refresh`.
    If the refresh fails but a cached copy exists, the stale copy is used
    with a warning. */
    pub fn open(url: &str, refresh: Refresh) -> Result<Self, Error> {
        if url == local::NAME {
            return Ok(Index::local_at(local::index_dir()?));
        }
        // nothing to clone, every resolve asks the API
        if forest::is_forest(url) {
            return Ok(Index {
                url: url.to_string(),
                root: PathBuf::new(),
                kind: Kind::Forest,
                ttl_skipped: false,
            });
        }
        let (root, ttl_skipped) = ensure_cached(url, refresh)?;
        let kind = if root.join("config.json").exists() {
            Kind::Wally(wally::load_config(&root)?)
        } else if root.join("config.toml").exists() {
            Kind::Pesde(pesde::load_config(&root)?)
        } else {
            /* Empty or half-set-up index repo. a raw io error here would
            read as an embr bug rather than an index problem. */
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

    /** the local registry with its index files in `root`. it is a folder,
    not a git clone, so there is nothing to refresh. */
    pub(crate) fn local_at(root: PathBuf) -> Self {
        Index {
            url: local::NAME.to_string(),
            root,
            kind: Kind::Pesde(pesde::Config {
                api: None,
                github_oauth_id: None,
            }),
            ttl_skipped: false,
        }
    }

    /** whether a requested refresh was skipped because the cache was still
    TTL-fresh, the signal resolution failures use to retry against a
    forced refresh before surfacing an error. */
    pub fn ttl_skipped(&self) -> bool {
        self.ttl_skipped
    }

    /** GitHub OAuth client id publishes authenticate against. Lives in the
    index's config, not the binary. Wally indices carry one too, but embr
    never publishes to wally, so only pesde-format configs are consulted. */
    pub fn github_oauth_id(&self) -> Option<&str> {
        match &self.kind {
            Kind::Wally(_) | Kind::Forest => None,
            Kind::Pesde(config) => config.github_oauth_id.as_deref(),
        }
    }

    /** Finds the highest version of `name` matching `req`. For pesde
    indices, `prefer`, a project's environment and language, picks between
    a version's targets, see [`pesde::resolve`]. */
    pub fn resolve(
        &self,
        name: &str,
        req: &semver::VersionReq,
        prefer: Option<(Environment, Language)>,
        pinned: &[semver::Version],
    ) -> Result<ResolvedPackage, Error> {
        match &self.kind {
            Kind::Wally(config) => wally::resolve(&self.root, &self.url, config, name, req),
            Kind::Pesde(config) => {
                match pesde::resolve(&self.root, &self.url, config, name, req, prefer, pinned) {
                    // a private or unlisted package is not in the git index
                    Err(missing @ Error::PackageNotFound { .. })
                        if self.url == DEFAULT_INDEX_URL =>
                    {
                        hidden::resolve(name, req, prefer, pinned)?.ok_or(missing)
                    }
                    found => found,
                }
            }
            Kind::Forest => forest::resolve(&self.url, name, req),
        }
    }

    /** every entry of `name`, yanked ones included, and the package-wide
    deprecation reason. it reads pesde-format indices only, which the ember
    index is. */
    pub fn read(&self, name: &str) -> Result<(Vec<pesde::Candidate>, Option<String>), Error> {
        match &self.kind {
            Kind::Pesde(_) => pesde::read(&self.root, &self.url, name),
            Kind::Wally(_) | Kind::Forest => Err(Error::IndexFetch {
                url: self.url.clone(),
                reason: "only a pesde-format index lists its versions this way".to_string(),
            }),
        }
    }
}

/** Downloads and extracts a resolved package into `dest`. Downloads are
anonymous, except from ember's API host, which serves private and unlisted
packages. That host alone gets the Ember token and the link key, so an
arbitrary registry url in an index entry can't harvest them. */
pub fn download(source: &DownloadSource, dest: &Path, cache: CachePolicy) -> Result<(), Error> {
    download_with(source, dest, cache, &mut |_, _| {})
}

/** [`download`], and `on_bytes` hears the archive arrive: bytes so far and
the whole size when known. a cached archive reports itself whole at once. */
pub fn download_with(
    source: &DownloadSource,
    dest: &Path,
    cache: CachePolicy,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(), Error> {
    let (DownloadSource::Zip { url } | DownloadSource::TarGz { url }) = source else {
        // Workspace members are linked in place, install never gets here.
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
    let (bytes, from_cache) = fetch_archive(url, &headers, cache, on_bytes)?;
    match extract(source, &bytes, dest) {
        Ok(()) => Ok(()),
        /* cached bytes that no longer extract are poison, bitrot the
        integrity record missed or a bad write. drop the entry, fetch
        fresh once, and try again on a clean dest */
        Err(_) if from_cache => {
            evict_archive(url);
            fs::remove_dir_all(dest)?;
            fs::create_dir_all(dest)?;
            let (bytes, _) = fetch_archive(url, &headers, CachePolicy::Bypass, on_bytes)?;
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
            /* ureq already gunzips Content-Encoding: gzip bodies, leaving a
            raw tar, so sniff the magic bytes instead of assuming. */
            if bytes.starts_with(&[0x1f, 0x8b]) {
                let decoder = flate2::read::GzDecoder::new(bytes);
                tar::Archive::new(decoder).unpack(dest)?;
            } else {
                tar::Archive::new(bytes).unpack(dest)?;
            }
        }
        DownloadSource::Workspace { .. } => unreachable!("rejected by download"),
    }
    remove_links(dest)
}

/** removes every symbolic link under `dir`. the tar and zip crates refuse
to write through a link, but they keep the link itself. a link to a file
out of the package folder then reaches the steps after the extract: the
require rewrite reads that file and writes it back, and Rojo syncs it into
the place. a package needs no links, so none stay. */
fn remove_links(dir: &Path) -> Result<(), Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            // a link to a folder is a folder entry on Windows
            let path = entry.path();
            fs::remove_file(&path).or_else(|_| fs::remove_dir(&path))?;
        } else if kind.is_dir() {
            remove_links(&entry.path())?;
        }
    }
    Ok(())
}

/** the archive's bytes, from the cache when allowed and present. published
registry versions are immutable by policy, registries reject duplicate
publishes, which is what makes caching by URL sound. `--refresh` is the
escape hatch for the exceptions like takedowns or republished storage, and
`embr cache clean` drops everything. the bool is "these came from the
cache", which decides whether an extract failure warrants a refetch. */
fn fetch_archive(
    url: &str,
    headers: &[(&str, &str)],
    cache: CachePolicy,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(Vec<u8>, bool), Error> {
    if let Some(bytes) = local::read_archive(url) {
        let bytes = bytes?;
        let size = bytes.len() as u64;
        on_bytes(size, Some(size));
        return Ok((bytes, false));
    }
    let entry = archive_entry(url);

    if cache == CachePolicy::Use
        && let Some(entry) = &entry
        && let Some(bytes) = load_archive(entry, url)
    {
        let size = bytes.len() as u64;
        on_bytes(size, Some(size));
        return Ok((bytes, true));
    }

    /* the cache keeps the URL as written, so an archive cached from an old
    host stays a hit */
    let from = registry::moved(url);
    let started = std::time::Instant::now();
    let bytes = if registry::on_api_host(&from, &registry::api_url()) {
        registry::download(&from, on_bytes)?
    } else {
        http::get_bytes_with(&from, headers, on_bytes)?
    };
    crate::ui::timing(&format!("download {from}"), started);

    /* caching is best-effort, a full disk or permission problem must not
    fail an install that already has the bytes in hand. no size budget or
    eviction yet, `embr cache clean` is the release valve. an
    extracted-file store shared across projects, pesde-style, is the
    natural next step if this ever needs more */
    if let Some(entry) = &entry {
        let _ = store_archive(entry, &bytes, url);
    }
    Ok((bytes, false))
}

/** where `url`'s archive lives in the cache, None when there's no home
directory to cache under. readable slug + stable FNV hash, same naming
scheme as the index cache, but never DefaultHasher, whose output may
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

/** cached bytes, verified against the sidecar record of length, FNV, and
the url itself, so a filename-key collision can never serve another
package's archive. any mismatch or unreadable half deletes the entry and
reads as a miss. */
fn load_archive(entry: &Path, url: &str) -> Option<Vec<u8>> {
    let meta = fs::read_to_string(meta_path(entry)).ok()?;
    let bytes = fs::read(entry).ok()?;

    // "v1 <len> <hash> <url>". the url comes last since it can contain anything
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
the sidecar lands before the payload, a reader catching the pair mid-swap
sees a mismatch, treats it as a miss, and refetches. wasteful, never
wrong. */
fn store_archive(entry: &Path, bytes: &[u8], url: &str) -> std::io::Result<()> {
    let dir = entry.parent().expect("cache entries have a parent");
    fs::create_dir_all(dir)?;

    let meta = format!("v1 {} {:016x} {url}\n", bytes.len(), fnv1a(bytes));
    replace_file(&meta_path(entry), meta.as_bytes())?;
    replace_file(entry, bytes)
}

/** write-to-staging + rename. staging names carry pid and a counter, so
concurrent writers, other processes or this one's worker threads, can't
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
unstable. fnv1a_parts with a marker keeps the same "slug-hash" shape under
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
            let result = fetch(
                url,
                &["-C", &dir.to_string_lossy(), "pull", "--ff-only"],
                STALL_SECS,
            );
            crate::ui::timing(&format!("index-refresh {url}"), started);
            /* stamp the attempt, success or not. an unreachable remote
            should warn once per TTL window, not hang every install */
            touch(&stamp);
            if let Err(error) = result {
                eprintln!("warning: embr uses the cached copy of the index. {error}");
            }
        }
        return Ok((dir, false));
    }

    std::fs::create_dir_all(dir.parent().expect("cache dir has a parent"))?;
    let started = std::time::Instant::now();
    fetch(
        url,
        &["clone", "--depth", "1", url, &dir.to_string_lossy()],
        STALL_SECS,
    )?;
    crate::ui::timing(&format!("index-clone {url}"), started);
    touch(&stamp);
    Ok((dir, false))
}

/** git sets no deadline of its own. a host that holds the connection open,
as GitHub can in an outage, kept resolution waiting forever (#34). a
transfer slower than one byte a second for this long fails instead. */
const STALL_SECS: u32 = 30;

/// how many tries a clone or a pull gets when the host fails.
const FETCH_ATTEMPTS: u32 = 3;

/** runs a git clone or pull against the index at `url`. a server error or
a stall can pass, so it gets FETCH_ATTEMPTS tries with a short wait between
them. any other failure, a 404 or a bad url, fails at once. */
fn fetch(url: &str, args: &[&str], stall_secs: u32) -> Result<(), Error> {
    let stall = format!("http.lowSpeedTime={stall_secs}");
    let mut full = vec!["-c", "http.lowSpeedLimit=1", "-c", &stall];
    full.extend_from_slice(args);

    let mut attempt = 0;
    loop {
        attempt += 1;
        let Err(reason) = git::run(&full) else {
            return Ok(());
        };
        // a 503 reads "returned error: 503", a stall "Operation too slow"
        let host_failed = reason.contains("returned error: 5") || reason.contains("too slow");
        if !host_failed {
            return Err(Error::IndexFetch {
                url: url.to_string(),
                reason,
            });
        }
        if attempt == FETCH_ATTEMPTS {
            return Err(Error::IndexUnavailable {
                url: url.to_string(),
                attempts: FETCH_ATTEMPTS,
                // git's last line holds the cause, the lines above it are progress
                reason: reason.lines().last().unwrap_or_default().to_string(),
            });
        }
        std::thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
    }
}

/** whether `url`'s index cache was refreshed within the TTL, what
install's fast path asks before trusting a lockfile without resolving.
inside the window, re-resolving against the same index state could only
reproduce the lock, so skipping is free. past it, `^` requirements deserve
a real look. */
pub fn is_fresh(url: &str) -> bool {
    cache_dir(url).is_ok_and(|dir| stamp_fresh(&dir.join(REFRESH_STAMP), index_ttl()))
}

/// marks when an index cache last attempted a refresh, see `Refresh`.
const REFRESH_STAMP: &str = ".ember-refreshed";

/** whether `stamp` exists and is younger than `ttl`. a zero ttl is never
fresh. also behind the tools module's latest-release stamps, which share
the index TTL knob on purpose. */
pub(crate) fn stamp_fresh(stamp: &Path, ttl: Duration) -> bool {
    if ttl.is_zero() {
        return false;
    }
    fs::metadata(stamp)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age < ttl)
}

/** (re)writes `stamp` so its mtime is now. the payload matters, writing
zero bytes over an already-empty file is a no-op Windows won't bump the
mtime for, which would freeze the stamp at its creation time forever.
best-effort, worst case is an extra pull. */
fn touch(stamp: &Path) {
    let _ = fs::write(stamp, b"refreshed\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_cache_verifies_what_it_returns() {
        let dir = std::env::temp_dir().join("embr-test-archive-cache");
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
        let dir = std::env::temp_dir().join("embr-test-refresh-stamp");
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
        zero-byte write bites on Windows. no bytes, no truncation, no
        mtime update, the stamp would age out once and stay expired */
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

    /** the WS1 contract, against a real local git index. a TTL-fresh cache
    skips the pull and serves stale data by design, a resolve that fails
    on that stale data forces one refresh and succeeds, and Force always
    pulls. */
    #[test]
    fn ttl_skips_pulls_and_failed_resolves_force_one() {
        /* the assertions below assume the default TTL. a dev running the
        suite with the knob exported gets a skip, not a bogus failure */
        if std::env::var_os(TTL_VAR).is_some() || std::env::var_os(LEGACY_TTL_VAR).is_some() {
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

        let origin = std::env::temp_dir().join("embr-test-ttl-origin");
        let _ = fs::remove_dir_all(&origin);
        fs::create_dir_all(origin.join("acme")).unwrap();
        let url = origin.to_string_lossy().replace('\\', "/");
        // same url = same cache dir, start every run from nothing
        let cache = cache_dir(&url).unwrap();
        let _ = fs::remove_dir_all(&cache);
        let _cleanup = Cleanup(vec![origin.clone(), cache.clone()]);

        let git = |args: &[&str]| {
            let mut full = vec![
                "-C",
                origin.to_str().unwrap(),
                "-c",
                "user.name=embr-test",
                "-c",
                "user.email=embr-test@localhost",
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

        // first open clones, nothing was skipped
        let index = Index::open(&url, Refresh::Ttl).unwrap();
        assert!(!index.ttl_skipped());
        let any = semver::VersionReq::STAR;
        assert_eq!(
            index
                .resolve("acme/thing", &any, None, &[])
                .unwrap()
                .version,
            semver::Version::new(1, 0, 0)
        );

        // the origin moves on
        publish("2.0.0");
        git(&["add", "."]);
        git(&["commit", "-m", "v2"]);

        // within the TTL the pull is skipped, still v1, and v2 can't resolve
        let index = Index::open(&url, Refresh::Ttl).unwrap();
        assert!(index.ttl_skipped());
        assert_eq!(
            index
                .resolve("acme/thing", &any, None, &[])
                .unwrap()
                .version,
            semver::Version::new(1, 0, 0)
        );
        let two = semver::VersionReq::parse("^2.0.0").unwrap();
        assert!(index.resolve("acme/thing", &two, None, &[]).is_err());

        /* the resolver-level guardrail, a failing resolve on a skipped
        index earns one forced refresh and then succeeds */
        let manifest: crate::project::manifest::Manifest = toml::from_str(&format!(
            "[package]\nname = \"acme/consumer\"\nversion = \"0.1.0\"\n\n\
             [indices]\ndefault = \"{url}\"\n\n\
             [dependencies]\nthing = {{ name = \"acme/thing\", version = \"^2.0.0\" }}\n"
        ))
        .unwrap();
        // project_dir only matters for workspace deps, origin keeps it hermetic
        let resolved = crate::registry::resolver::resolve(
            &manifest,
            &origin,
            Refresh::Ttl,
            &Default::default(),
            None,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].version, semver::Version::new(2, 0, 0));

        // Force always pulls, fresh stamp or not
        let index = Index::open(&url, Refresh::Force).unwrap();
        assert!(!index.ttl_skipped());
        assert_eq!(
            index
                .resolve("acme/thing", &two, None, &[])
                .unwrap()
                .version,
            semver::Version::new(2, 0, 0)
        );
    }

    /** a local index host. it answers every request with `reply`, or with
    no reply it holds the connection open and says nothing. the count is
    the number of connections it took. */
    fn host(
        reply: Option<&'static str>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/index", listener.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for mut stream in listener.incoming().flatten() {
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = stream.read(&mut [0u8; 4096]);
                match reply {
                    Some(reply) => drop(stream.write_all(reply.as_bytes())),
                    None => held.push(stream),
                }
            }
        });
        (url, hits)
    }

    /// #34: a 503 gets three tries, then an error that names the host.
    #[test]
    fn a_failing_host_gets_bounded_tries() {
        let (url, hits) = host(Some(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ));
        if cache_dir(&url).is_err() {
            return; // no home directory on this machine
        }

        let Err(error) = Index::open(&url, Refresh::Ttl) else {
            panic!("a host that answers 503 opened as an index");
        };
        assert!(
            matches!(error, Error::IndexUnavailable { attempts: 3, .. }),
            "{error}"
        );
        assert!(error.to_string().contains("503"), "{error}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    /// #34: a host that never answers fails the clone, it does not hang it.
    #[test]
    fn a_stalled_host_fails_instead_of_hanging() {
        let (url, hits) = host(None);
        let dir = std::env::temp_dir().join("embr-test-stalled-clone");
        let _ = fs::remove_dir_all(&dir);

        let started = std::time::Instant::now();
        let clone: [&str; 5] = ["clone", "--depth", "1", &url, &dir.to_string_lossy()];
        let error = fetch(&url, &clone, 1).unwrap_err();
        assert!(matches!(error, Error::IndexUnavailable { .. }), "{error}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert!(started.elapsed() < Duration::from_secs(60));
    }

    /** a package that ships `init.luau` as a link to a file outside its
    folder. without the link removal, the require rewrite reads the victim
    through the link and writes the rewritten text back to it. */
    #[cfg(unix)]
    #[test]
    fn extract_drops_links_that_point_out_of_the_package() {
        use std::io::Write;

        let root = std::env::temp_dir().join("embr-test-extract-links");
        let _ = fs::remove_dir_all(&root);
        let victim = root.join("victim.luau");
        fs::create_dir_all(&root).unwrap();
        let source = "local Foo = require(script.Foo)\nreturn Foo\n";
        fs::write(&victim, source).unwrap();

        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "init.luau", &victim).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "Foo.luau", &b"ok\n\n"[..])
            .unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar.into_inner().unwrap()).unwrap();
        let tar_gz = gz.finish().unwrap();

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.add_symlink("init.luau", victim.to_string_lossy(), options)
            .unwrap();
        zip.start_file("Foo.luau", options).unwrap();
        zip.write_all(b"ok\n\n").unwrap();
        let zip = zip.finish().unwrap().into_inner();

        let url = String::new();
        for (source_kind, bytes) in [
            (DownloadSource::TarGz { url: url.clone() }, tar_gz),
            (DownloadSource::Zip { url: url.clone() }, zip),
        ] {
            let dest = root.join("package");
            let _ = fs::remove_dir_all(&dest);
            fs::create_dir_all(&dest).unwrap();
            extract(&source_kind, &bytes, &dest).unwrap();

            assert!(
                fs::symlink_metadata(dest.join("init.luau")).is_err(),
                "{source_kind:?} kept the link"
            );
            assert!(dest.join("Foo.luau").is_file(), "{source_kind:?}");
            let _ = crate::project::requires::rewrite_instance_requires(&dest, "init");
            assert_eq!(fs::read_to_string(&victim).unwrap(), source);
        }
        let _ = fs::remove_dir_all(&root);
    }

    /** a `..` entry, and a file written through a link to a folder out of
    the package. neither may land outside `dest`. */
    #[cfg(unix)]
    #[test]
    fn extract_never_writes_outside_the_package() {
        use std::io::Write;

        let root = std::env::temp_dir().join("embr-test-extract-escape");
        let _ = fs::remove_dir_all(&root);
        let dest = root.join("package");

        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.as_mut_bytes()[..13].copy_from_slice(b"../escape.txt");
        header.set_size(2);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, &b"x\n"[..]).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "out", &root).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(2);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "out/through.txt", &b"x\n"[..])
            .unwrap();
        let tar = tar.into_inner().unwrap();

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.add_symlink("out", root.to_string_lossy(), options)
            .unwrap();
        zip.start_file("out/through.txt", options).unwrap();
        zip.write_all(b"x\n").unwrap();
        zip.start_file("../escape.txt", options).unwrap();
        zip.write_all(b"x\n").unwrap();
        let zip = zip.finish().unwrap().into_inner();

        let url = String::new();
        for (source_kind, bytes) in [
            (DownloadSource::TarGz { url: url.clone() }, tar),
            (DownloadSource::Zip { url: url.clone() }, zip),
        ] {
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&dest).unwrap();
            let _ = extract(&source_kind, &bytes, &dest);
            assert!(!root.join("escape.txt").exists(), "{source_kind:?}");
            assert!(!root.join("through.txt").exists(), "{source_kind:?}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cache_dirs_are_per_url() {
        let Ok(wally) = cache_dir("https://github.com/UpliftGames/wally-index") else {
            return; // no home directory on this machine
        };
        let pesde = cache_dir("https://github.com/pesde-pkg/index").unwrap();

        assert!(wally.starts_with(paths::index_cache_dir().unwrap()));
        /* Readable half of the folder name comes from the url's last
        segment. the hash keeps same-named indices apart. */
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
