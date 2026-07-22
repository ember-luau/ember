pub mod pesde;
pub mod wally;

use crate::error::Error;
use crate::http;
use crate::manifest::Environment;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A package index: a git repository cached under ~/.lpm/index-cache.
/// Wally indices are identified by a root config.json; pesde/lpm indices by
/// a root config.toml (lpm entries additionally carry direct download URLs).
pub struct Index {
    url: String,
    root: PathBuf,
    kind: Kind,
}

enum Kind {
    Wally(wally::Config),
    Pesde(pesde::Config),
}

/// A concrete package version picked from an index.
pub struct ResolvedPackage {
    pub version: semver::Version,
    /// Known from index metadata; None means "inspect the archive after
    /// extraction" (lpm.toml -> pesde.toml -> wally.toml fallback).
    pub environment: Option<Environment>,
    pub dependencies: Vec<TransitiveDependency>,
    pub source: DownloadSource,
}

pub struct TransitiveDependency {
    pub name: String,
    pub version_req: String,
    /// None = resolve in the same index as the parent package.
    pub index_url: Option<String>,
}

/// Everything needed to re-download a resolved package (also stored in
/// lpm.lock for --locked installs). Resolution bakes the full URL so locked
/// installs never re-consult an index.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum DownloadSource {
    /// Zip archive (wally registry APIs).
    Zip { url: String },
    /// Gzipped tarball (pesde registry APIs and lpm direct URLs).
    TarGz { url: String },
}

impl Index {
    /// Opens an index, cloning or refreshing its git cache. When refreshing
    /// fails but a cached copy exists, the stale copy is used with a warning.
    pub fn open(url: &str, refresh: bool) -> Result<Self, Error> {
        let root = ensure_cached(url, refresh)?;
        let kind = if root.join("config.json").exists() {
            Kind::Wally(wally::load_config(&root)?)
        } else {
            Kind::Pesde(pesde::load_config(&root)?)
        };

        Ok(Index {
            url: url.to_string(),
            root,
            kind,
        })
    }

    /// Finds the highest version of `name` matching `req`. For pesde indices,
    /// `prefer_environment` picks between multiple targets of one version.
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

    /// GitHub OAuth client id from the index config, for future publishing.
    #[allow(dead_code)]
    pub fn github_oauth_id(&self) -> Option<&str> {
        match &self.kind {
            Kind::Wally(config) => config.github_oauth_id.as_deref(),
            Kind::Pesde(config) => config.github_oauth_id.as_deref(),
        }
    }
}

/// Version reported to wally registries, which reject clients that do not
/// send a recent-enough Wally-Version header (HTTP 426 otherwise).
const WALLY_VERSION: &str = "0.3.2";

/// Downloads and extracts a resolved package into `dest`.
pub fn download(source: &DownloadSource, dest: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dest)?;
    match source {
        DownloadSource::Zip { url } => {
            let bytes = http::get_bytes(url, &[("Wally-Version", WALLY_VERSION)])?;
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            archive.extract(dest)?;
            Ok(())
        }
        DownloadSource::TarGz { url } => {
            let bytes = http::get_bytes(url, &[])?;
            // ureq transparently decodes Content-Encoding: gzip, in which
            // case the body is already the raw tar; sniff the gzip magic.
            if bytes.starts_with(&[0x1f, 0x8b]) {
                let decoder = flate2::read::GzDecoder::new(bytes.as_slice());
                tar::Archive::new(decoder).unpack(dest)?;
            } else {
                tar::Archive::new(bytes.as_slice()).unpack(dest)?;
            }
            Ok(())
        }
    }
}

fn cache_dir(url: &str) -> Result<PathBuf, Error> {
    let home = dirs::home_dir().ok_or(Error::NoHomeDir)?;

    let slug: String = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("index")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);

    Ok(home
        .join(".lpm")
        .join("index-cache")
        .join(format!("{slug}-{:016x}", hasher.finish())))
}

fn ensure_cached(url: &str, refresh: bool) -> Result<PathBuf, Error> {
    let dir = cache_dir(url)?;

    if dir.join(".git").exists() {
        if refresh
            && let Err(reason) = run_git(&["-C", &dir.to_string_lossy(), "pull", "--ff-only"])
        {
            eprintln!("warning: could not refresh index {url} ({reason}); using cached copy");
        }
        return Ok(dir);
    }

    std::fs::create_dir_all(dir.parent().expect("cache dir has a parent"))?;
    run_git(&["clone", "--depth", "1", url, &dir.to_string_lossy()]).map_err(|reason| {
        Error::IndexFetch {
            url: url.to_string(),
            reason,
        }
    })?;
    Ok(dir)
}

/// Runs git, returning stderr as the error message on failure.
fn run_git(args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}
