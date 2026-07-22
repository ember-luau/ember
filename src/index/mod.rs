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

    /// GitHub OAuth client id from the index config, driving publish's
    /// device flow.
    pub fn github_oauth_id(&self) -> Option<&str> {
        match &self.kind {
            Kind::Wally(config) => config.github_oauth_id.as_deref(),
            Kind::Pesde(config) => config.github_oauth_id.as_deref(),
        }
    }

    /// Directory of the cached clone under ~/.lpm/index-cache.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the index marks itself private (its downloads need auth).
    /// Wally configs have no such flag; wally indices are never private.
    /// Not consulted yet: downloads currently attach the token by host
    /// instead, since --locked installs never open an index.
    #[allow(dead_code)]
    pub fn is_private(&self) -> bool {
        match &self.kind {
            Kind::Wally(_) => false,
            Kind::Pesde(config) => config.private,
        }
    }
}

/// Version reported to wally registries, which reject clients that do not
/// send a recent-enough Wally-Version header (HTTP 426 otherwise).
const WALLY_VERSION: &str = "0.3.2";

/// Downloads and extracts a resolved package into `dest`. `token` is a
/// GitHub token for private downloads; it is only ever attached for
/// GitHub-owned hosts.
pub fn download(source: &DownloadSource, dest: &Path, token: Option<&str>) -> Result<(), Error> {
    std::fs::create_dir_all(dest)?;
    let (DownloadSource::Zip { url } | DownloadSource::TarGz { url }) = source;

    // The token never goes to a non-GitHub host: an arbitrary registry url in
    // an index entry must not be able to harvest the credential.
    let bearer = token
        .filter(|_| is_github_host(url))
        .map(|token| format!("Bearer {token}"));
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if matches!(source, DownloadSource::Zip { .. }) {
        headers.push(("Wally-Version", WALLY_VERSION));
    }
    if let Some(bearer) = bearer.as_deref() {
        headers.push(("Authorization", bearer));
        // octet-stream is what makes a private release-asset API url serve
        // the asset bytes instead of its JSON metadata.
        headers.push(("Accept", "application/octet-stream"));
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
    }
    Ok(())
}

/// Exact-match allowlist of GitHub-owned hosts (the only ones a download
/// token may be sent to). https only, no suffix matching, so lookalikes such
/// as `notgithub.com` or `github.com.evil.example` never qualify.
fn is_github_host(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = match rest.find(['/', '?', '#']) {
        Some(end) => &rest[..end],
        None => rest,
    };
    [
        "github.com",
        "api.github.com",
        "uploads.github.com",
        "objects.githubusercontent.com",
        "raw.githubusercontent.com",
    ]
    .contains(&host)
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

#[cfg(test)]
mod tests {
    use super::is_github_host;

    #[test]
    fn recognizes_github_owned_hosts() {
        assert!(is_github_host(
            "https://github.com/luaupm/pindex/releases/download/v0.1.0/pkg.tar.gz"
        ));
        assert!(is_github_host(
            "https://api.github.com/repos/luaupm/pindex/releases/assets/123"
        ));
        assert!(is_github_host("https://uploads.github.com/repos/a/b"));
        assert!(is_github_host("https://objects.githubusercontent.com/blob"));
        assert!(is_github_host(
            "https://raw.githubusercontent.com/a/b/main/x"
        ));
        assert!(is_github_host("https://github.com"));
    }

    #[test]
    fn rejects_everything_else() {
        assert!(!is_github_host("https://registry.example.com/pkg.tar.gz"));
        assert!(!is_github_host("https://notgithub.com/pkg.tar.gz"));
        assert!(!is_github_host(
            "https://github.com.evil.example/pkg.tar.gz"
        ));
        assert!(!is_github_host("https://gist.github.com/a/b"));
        // Path or query mentioning github must not count as the host.
        assert!(!is_github_host(
            "https://evil.example/github.com/pkg.tar.gz"
        ));
        assert!(!is_github_host("https://evil.example/x?u=api.github.com"));
        // Plain http would send the token in the clear.
        assert!(!is_github_host("http://github.com/pkg.tar.gz"));
    }
}
