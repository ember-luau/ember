pub mod pesde;
pub mod wally;

use crate::error::Error;
use crate::net::http;
use crate::project::manifest::Environment;
use crate::sys::git;
use crate::sys::paths;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// A package index: a git repository cached under ~/.lpm/index-cache.
/// Wally indices are identified by a root config.json, pesde indices by a
/// root config.toml.
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
    /// Gzipped tarball (pesde registry APIs).
    TarGz { url: String },
    /// A workspace member, linked in place instead of downloaded; `path` is
    /// the member's directory relative to the consuming project.
    Workspace { path: String },
}

impl Index {
    /// Opens an index, cloning or refreshing its git cache. When refreshing
    /// fails but a cached copy exists, the stale copy is used with a warning.
    pub fn open(url: &str, refresh: bool) -> Result<Self, Error> {
        let root = ensure_cached(url, refresh)?;
        let kind = if root.join("config.json").exists() {
            Kind::Wally(wally::load_config(&root)?)
        } else if root.join("config.toml").exists() {
            Kind::Pesde(pesde::load_config(&root)?)
        } else {
            // An empty or half-set-up index repo; a raw io error here would
            // read as a bug in lpm rather than a problem with the index.
            return Err(Error::IndexFetch {
                url: url.to_string(),
                reason: "the index has no config.json or config.toml at its root".to_string(),
            });
        };

        Ok(Index {
            url: url.to_string(),
            root,
            kind,
        })
    }

    /// GitHub OAuth app client id publishes authenticate against, from the
    /// index's config. Wally indices carry one too, but lpm never publishes
    /// to wally, so only pesde-format configs are consulted.
    pub fn github_oauth_id(&self) -> Option<&str> {
        match &self.kind {
            Kind::Wally(_) => None,
            Kind::Pesde(config) => config.github_oauth_id.as_deref(),
        }
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
}

/// Downloads and extracts a resolved package into `dest`. All downloads are
/// anonymous: lpm's registry serves tarballs from a public CDN, wally/pesde
/// registries are public too. If credentialed downloads ever come back,
/// restore the old restraint — the token went only to GitHub-owned hosts, so
/// an arbitrary registry url in an index entry could not harvest it.
pub fn download(source: &DownloadSource, dest: &Path) -> Result<(), Error> {
    let (DownloadSource::Zip { url } | DownloadSource::TarGz { url }) = source else {
        // Workspace members are linked in place; install never gets here.
        return Err(Error::IndexFetch {
            url: String::new(),
            reason: "workspace dependencies are linked in place, not downloaded".to_string(),
        });
    };
    std::fs::create_dir_all(dest)?;

    let mut headers: Vec<(&str, &str)> = Vec::new();
    if matches!(source, DownloadSource::Zip { .. }) {
        headers.push(("Wally-Version", wally::VERSION_HEADER));
    }

    let bytes = http::get_bytes(url, &headers)?;
    match source {
        DownloadSource::Zip { .. } => {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            archive.extract(dest)?;
        }
        DownloadSource::TarGz { .. } => {
            // ureq transparently decodes Content-Encoding: gzip, in which
            // case the body is already the raw tar; sniff the gzip magic.
            if bytes.starts_with(&[0x1f, 0x8b]) {
                let decoder = flate2::read::GzDecoder::new(bytes.as_slice());
                tar::Archive::new(decoder).unpack(dest)?;
            } else {
                tar::Archive::new(bytes.as_slice()).unpack(dest)?;
            }
        }
        DownloadSource::Workspace { .. } => unreachable!("rejected above"),
    }
    Ok(())
}

fn cache_dir(url: &str) -> Result<PathBuf, Error> {
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

    Ok(paths::index_cache_dir()?.join(format!("{slug}-{:016x}", hasher.finish())))
}

fn ensure_cached(url: &str, refresh: bool) -> Result<PathBuf, Error> {
    let dir = cache_dir(url)?;

    if dir.join(".git").exists() {
        if refresh
            && let Err(reason) = git::run(&["-C", &dir.to_string_lossy(), "pull", "--ff-only"])
        {
            eprintln!("warning: could not refresh index {url} ({reason}); using cached copy");
        }
        return Ok(dir);
    }

    std::fs::create_dir_all(dir.parent().expect("cache dir has a parent"))?;
    git::run(&["clone", "--depth", "1", url, &dir.to_string_lossy()]).map_err(|reason| {
        Error::IndexFetch {
            url: url.to_string(),
            reason,
        }
    })?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dirs_are_per_url() {
        let Ok(wally) = cache_dir("https://github.com/UpliftGames/wally-index") else {
            return; // no home directory on this machine
        };
        let pesde = cache_dir("https://github.com/pesde-pkg/index").unwrap();

        assert!(wally.starts_with(paths::index_cache_dir().unwrap()));
        // The readable half of the folder name comes from the url's last
        // segment; the hash keeps same-named indices from colliding.
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
