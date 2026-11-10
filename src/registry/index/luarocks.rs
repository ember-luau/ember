/*! LuaRocks (luarocks.org), the Lua registry. a rock is plain Lua with no
Luau layout, so the ember registry converts it: the modules become a tree
of files, and each `require("a.b")` a path that Roblox and the Luau
runtimes resolve. embr reads the converted rock through the ember API,
two GETs like Forest: the version list, then the picked version, which
holds the archive URL and its SHA-256.

a rock is named `rocks/<name>`. LuaRocks names have no scope, and the
scope keeps them apart from every other index. a rock is portable Luau,
so it installs in any environment. */

use super::{DownloadSource, ResolvedPackage, TransitiveDependency};
use crate::error::Error;
use crate::net::http::{self, error::HttpError};
use crate::project::manifest::{Environment, split_package_name, takes};
use serde::Deserialize;

/// the API embr reads, and the URL the known index name `luarocks` stands for.
pub const API_URL: &str = "https://api.emberpm.dev/v1/luarocks";

/// the scope of every rock.
pub const SCOPE: &str = "rocks";

/** whether `url` names a LuaRocks API: a path that ends in `/v1/luarocks`.
any host counts, so a staging or local registry works too. */
pub fn is_luarocks(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .trim_end_matches('/')
        .ends_with("/v1/luarocks")
}

/// `GET {api}/packages/{name}`, newest first.
#[derive(Deserialize)]
struct Listing {
    versions: Vec<Listed>,
}

#[derive(Deserialize)]
struct Listed {
    version: String,
}

/// `GET {api}/packages/{name}/{version}`, the fields embr reads.
#[derive(Deserialize)]
struct Metadata {
    #[serde(default)]
    dependencies: Vec<Dependency>,
    #[serde(default)]
    archive: String,
    #[serde(default)]
    sha256: String,
    /// why the rock does not convert, such as a rock of C alone.
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    version: String,
}

/// the rock name of `rocks/<name>`.
fn rock_name(name: &str) -> Result<&str, Error> {
    let (scope, rock) = split_package_name(name)?;
    if scope != SCOPE {
        return Err(Error::IndexFetch {
            url: API_URL.to_string(),
            reason: format!("a rock is named {SCOPE}/<name>, so {name} names none"),
        });
    }
    Ok(rock)
}

pub fn resolve(api: &str, name: &str, req: &semver::VersionReq) -> Result<ResolvedPackage, Error> {
    let name = name.to_lowercase();
    let rock = rock_name(&name)?;
    let base = format!("{}/packages/{rock}", api.trim_end_matches('/'));

    let listing: Listing = match http::get_json(&base, &[]) {
        Err(HttpError::NotFound) => {
            return Err(Error::PackageNotFound {
                name,
                index: api.to_string(),
            });
        }
        listing => listing?,
    };
    let Some(version) = pick(&listing, req) else {
        return Err(Error::NoMatchingVersion {
            name,
            req: req.to_string(),
        });
    };

    /* a rock that does not convert answers 422 with its reason. ureq reads
    that as an error, so the body is read from the error */
    let url = format!("{base}/{version}");
    let metadata: Metadata = match http::get_json(&url, &[]) {
        Err(HttpError::Request(error)) => match *error {
            ureq::Error::Status(422, response) => response.into_json().map_err(HttpError::from)?,
            other => return Err(HttpError::Request(Box::new(other)).into()),
        },
        metadata => metadata?,
    };
    resolved(version, metadata, api, &name)
}

/// the highest listed version `req` matches. unparsable versions are skipped.
fn pick(listing: &Listing, req: &semver::VersionReq) -> Option<semver::Version> {
    let versions: Vec<semver::Version> = listing
        .versions
        .iter()
        .filter_map(|listed| semver::Version::parse(&listed.version).ok())
        .collect();
    let no_stable = !versions.iter().any(|version| version.pre.is_empty());
    versions
        .into_iter()
        .filter(|version| takes(req, version, no_stable))
        .max()
}

fn resolved(
    version: semver::Version,
    metadata: Metadata,
    api: &str,
    name: &str,
) -> Result<ResolvedPackage, Error> {
    if let Some(reason) = metadata.error {
        return Err(Error::IndexFetch {
            url: api.to_string(),
            reason: format!("{name}@{version} does not install: {reason}"),
        });
    }
    let sha256 = metadata.sha256.to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::IndexFetch {
            url: api.to_string(),
            reason: format!("{name}@{version} has no valid archive hash"),
        });
    }
    if !metadata.archive.starts_with("https://") && !metadata.archive.starts_with("http://") {
        return Err(Error::IndexFetch {
            url: api.to_string(),
            reason: format!("{name}@{version} has no archive URL"),
        });
    }

    let dependencies = metadata
        .dependencies
        .into_iter()
        .map(|dependency| TransitiveDependency {
            name: format!("{SCOPE}/{}", dependency.name.to_lowercase()),
            version_req: dependency.version,
            index_url: None,
            /* the converted rock requires a dependency as `../<rock>`, so
            the link must carry the rock's own name */
            alias: dependency.name,
            optional: false,
            features: Vec::new(),
            default_features: true,
        })
        .collect();

    Ok(ResolvedPackage {
        version,
        environment: Some(Environment::Luau),
        language: Default::default(),
        // a LuaRocks version is never yanked; a new revision replaces it
        yanked: false,
        deprecated: None,
        package_deprecated: None,
        dependencies,
        features: Default::default(),
        source: DownloadSource::TarGz {
            url: metadata.archive,
        },
        sha256: Some(sha256),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"{"name":"inspect","versions":[
        {"rock":"3.1.3-0","version":"3.1.3+0"},
        {"rock":"3.1.2-0","version":"3.1.2+0"},
        {"rock":"3.0-4","version":"3.0.0+3.0-4"}]}"#;

    const META: &str = r#"{"name":"penlight","version":"1.14.0+3","rock":"1.14.0-3",
        "summary":"Lua utility libraries","license":"MIT/X11","homepage":"",
        "dependencies":[{"name":"inspect","version":">=3.1.0, <3.2.0"}],
        "archive":"https://cdn.emberpm.dev/luarocks.org/penlight/1.14.0-3.tar.gz",
        "sha256":"E4BE32438015409ACF9D95DBE59A146EEDA120E8D4AF9BC3671303B4157E2E34","notes":[]}"#;

    #[test]
    fn knows_luarocks_by_its_path() {
        assert!(is_luarocks(API_URL));
        assert!(is_luarocks("http://localhost:8787/v1/luarocks/"));
        assert!(!is_luarocks("https://api.emberpm.dev"));
        assert!(!is_luarocks("https://api.forest.dev"));
    }

    #[test]
    fn picks_the_highest_match() {
        let listing: Listing = serde_json::from_str(LISTING).unwrap();
        let pick_for = |req: &str| {
            pick(&listing, &semver::VersionReq::parse(req).unwrap()).map(|v| v.to_string())
        };
        assert_eq!(pick_for("^3").as_deref(), Some("3.1.3+0"));
        assert_eq!(pick_for("~3.0").as_deref(), Some("3.0.0+3.0-4"));
        assert_eq!(pick_for("^4"), None);
    }

    #[test]
    fn metadata_becomes_a_portable_package_with_rock_dependencies() {
        let metadata: Metadata = serde_json::from_str(META).unwrap();
        let version = semver::Version::parse("1.14.0+3").unwrap();
        let package = resolved(version, metadata, API_URL, "rocks/penlight").unwrap();
        assert_eq!(package.environment, Some(Environment::Luau));
        assert_eq!(
            package.sha256.as_deref(),
            Some("e4be32438015409acf9d95dbe59a146eeda120e8d4af9bc3671303b4157e2e34")
        );
        let edges: Vec<_> = package
            .dependencies
            .iter()
            .map(|d| (d.alias.as_str(), d.name.as_str(), d.version_req.as_str()))
            .collect();
        assert_eq!(edges, [("inspect", "rocks/inspect", ">=3.1.0, <3.2.0")]);
    }

    #[test]
    fn a_rock_that_does_not_convert_says_why() {
        let metadata: Metadata = serde_json::from_str(
            r#"{"error":"the rock is built from C (lfs), which Luau cannot load"}"#,
        )
        .unwrap();
        let error = resolved(
            semver::Version::new(1, 8, 0),
            metadata,
            API_URL,
            "rocks/luafilesystem",
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("built from C"), "{error}");
    }

    #[test]
    fn a_name_outside_the_rocks_scope_is_refused() {
        assert!(rock_name("rocks/inspect").is_ok());
        assert!(rock_name("kikito/inspect").is_err());
    }
}
