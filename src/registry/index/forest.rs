/*! Forest (forest.dev), the Roblox registry with an HTTP API instead of a git
index. two GETs resolve a package: the version list, then the picked
version's metadata. archives sit on a CDN, each named by its SHA-256.

Forest packages carry no realm. A package mirrored from wally keeps its
wally.toml, so embr reads the realm from the archive for those. */

use super::{DownloadSource, ResolvedPackage, TransitiveDependency};
use crate::error::Error;
use crate::net::http::{self, error::HttpError};
use crate::project::manifest::{Environment, split_package_name, takes};
use serde::Deserialize;
use std::collections::BTreeMap;

/// the API embr reads, and the URL the known index name `forest` stands for.
pub const API_URL: &str = "https://api.forest.dev";

/// public archives, `<sha256>.tgz`. the Forest CLI builds the same URL.
const CDN_URL: &str = "https://registry.forest.dev/public";

/// embr installs Roblox packages only. Forest's other platform is UEFN.
const PLATFORM: &str = "roblox";

/** whether `url` names Forest's API. Forest has no git index to clone, so
the host decides the kind, not a config file. */
pub fn is_forest(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .is_some_and(|host| host.eq_ignore_ascii_case("api.forest.dev"))
}

/// `GET /v1/package/{scope}/{platform}/{name}`, only the field embr reads.
#[derive(Deserialize)]
struct Listing {
    versions: Vec<Listed>,
}

#[derive(Deserialize)]
struct Listed {
    version: String,
}

/// `GET /v1/package/{scope}/{platform}/{name}/{version}`.
#[derive(Deserialize)]
struct Metadata {
    #[serde(default)]
    dependencies: BTreeMap<String, Dependency>,
    /// hex SHA-256 of the archive, which also names it on the CDN.
    #[serde(default)]
    integrity: String,
    /// the registry this version is copied from, `"wally"` today. null on a package published to Forest.
    #[serde(default, rename = "mirroredFrom")]
    mirrored_from: Option<String>,
    #[serde(default)]
    archived: bool,
}

/// a forest.json dependency: a range, or a range with an alias.
#[derive(Deserialize)]
#[serde(untagged)]
enum Dependency {
    Range(String),
    Full {
        version: String,
        #[serde(default)]
        alias: Option<String>,
    },
}

pub fn resolve(api: &str, name: &str, req: &semver::VersionReq) -> Result<ResolvedPackage, Error> {
    // Forest matches names in any case, embr keys them lowercase
    let name = name.to_lowercase();
    let (scope, package) = split_package_name(&name)?;
    let base = format!(
        "{}/v1/package/{scope}/{PLATFORM}/{package}",
        api.trim_end_matches('/')
    );

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

    let metadata: Metadata = http::get_json(&format!("{base}/{version}"), &[])?;
    resolved(version, metadata, api)
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

/// `name` of `scope/name`, the whole string when it has no scope.
pub(crate) fn short_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn resolved(
    version: semver::Version,
    metadata: Metadata,
    api: &str,
) -> Result<ResolvedPackage, Error> {
    /* the hash goes into the download URL, so it must be a hash and
    nothing else, not a path a hostile response could steer */
    let integrity = metadata.integrity.to_ascii_lowercase();
    if integrity.len() != 64 || !integrity.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::IndexFetch {
            url: api.to_string(),
            reason: format!("version {version} has no valid archive hash"),
        });
    }

    let dependencies = metadata
        .dependencies
        .into_iter()
        .map(|(name, dependency)| {
            let (version_req, alias) = match dependency {
                Dependency::Range(range) => (range, None),
                Dependency::Full { version, alias } => (version, alias),
            };
            /* Forest's default alias is the package's name as declared,
            case included, since Roblox finds instances by exact name */
            let alias = alias.unwrap_or_else(|| short_name(&name).to_string());
            TransitiveDependency {
                name: name.to_lowercase(),
                version_req,
                index_url: None,
                alias,
            }
        })
        .collect();

    Ok(ResolvedPackage {
        version,
        /* a mirror keeps the source registry's manifest, so its realm
        decides. a package published to Forest is plain Roblox */
        environment: metadata
            .mirrored_from
            .is_none()
            .then_some(Environment::Roblox),
        language: Default::default(),
        // Forest versions are immutable and never yanked
        yanked: false,
        deprecated: None,
        package_deprecated: metadata
            .archived
            .then(|| "its author archived it on Forest".to_string()),
        dependencies,
        source: DownloadSource::TarGz {
            url: format!("{CDN_URL}/{integrity}.tgz"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // trimmed from api.forest.dev, evaera/promise
    const LISTING: &str = r#"{"name":"promise","scope":"evaera","latest":"4.0.0","versions":[
        {"version":"3.1.0","createdAt":"2026-07-19T19:30:07.000Z"},
        {"version":"3.2.1","createdAt":"2026-07-19T19:30:08.000Z"},
        {"version":"4.0.0-rc.2","createdAt":"2026-07-19T19:30:09.000Z"},
        {"version":"4.0.0","createdAt":"2026-07-19T19:30:09.000Z"}]}"#;

    // sleitnick/knit 1.7.0, a wally mirror with aliased dependencies
    const MIRRORED: &str = r#"{"name":"knit","scope":"sleitnick","version":"1.7.0",
        "dependencies":{"sleitnick/comm":{"version":"^1.0.0","alias":"Comm"},
                        "evaera/promise":{"version":"^4.0.0","alias":"Promise"}},
        "archiveRoot":"knit/init.lua","public":true,"archived":false,
        "integrity":"7999efe9356b4f0db3951c555cd36b2bafaf33328352d7417e2c771e4dc5a94e",
        "ownerType":"unclaimed","mirroredFrom":"wally"}"#;

    // stratiz/DataStream 2.1.0, published to Forest, both dependency forms
    const ORIGINAL: &str = r#"{"name":"DataStream","scope":"stratiz","version":"2.1.0",
        "dependencies":{"stratiz/Cleaner":"^1.0.0","stratiz/Signal":{"version":"^1.0.0"}},
        "archiveRoot":"src/init.luau","public":true,"archived":true,
        "integrity":"5F2B8A0C0E1D4B7A9C3E6F8D1A2B3C4D5E6F7A8B9C0D1E2F3A4B5C6D7E8F9A0B",
        "ownerType":"user","mirroredFrom":null}"#;

    #[test]
    fn knows_forest_by_its_host() {
        assert!(is_forest("https://api.forest.dev"));
        assert!(is_forest("https://API.forest.dev/"));
        assert!(is_forest("http://api.forest.dev/v1"));
        assert!(!is_forest("https://forest.dev"));
        assert!(!is_forest("https://api.forest.dev.example.com"));
        assert!(!is_forest("https://github.com/UpliftGames/wally-index"));
    }

    #[test]
    fn picks_the_highest_match_and_skips_prereleases() {
        let listing: Listing = serde_json::from_str(LISTING).unwrap();
        let pick_for = |req: &str| pick(&listing, &semver::VersionReq::parse(req).unwrap());
        assert_eq!(pick_for("^4.0.0"), Some(semver::Version::new(4, 0, 0)));
        assert_eq!(pick_for("^3"), Some(semver::Version::new(3, 2, 1)));
        assert_eq!(pick_for("=3.1.0"), Some(semver::Version::new(3, 1, 0)));
        assert_eq!(pick_for("^5"), None);
    }

    #[test]
    fn a_mirror_leaves_the_environment_to_its_archive() {
        let metadata: Metadata = serde_json::from_str(MIRRORED).unwrap();
        let package = resolved(semver::Version::new(1, 7, 0), metadata, API_URL).unwrap();
        assert_eq!(package.environment, None);
        assert_eq!(package.package_deprecated, None);
        assert_eq!(
            package.source,
            DownloadSource::TarGz {
                url: "https://registry.forest.dev/public/7999efe9356b4f0db3951c555cd36b2bafaf33328352d7417e2c771e4dc5a94e.tgz".to_string()
            }
        );
        let edges: Vec<_> = package
            .dependencies
            .iter()
            .map(|dep| {
                (
                    dep.alias.as_str(),
                    dep.name.as_str(),
                    dep.version_req.as_str(),
                )
            })
            .collect();
        assert_eq!(
            edges,
            [
                ("Promise", "evaera/promise", "^4.0.0"),
                ("Comm", "sleitnick/comm", "^1.0.0"),
            ]
        );
        assert!(
            package
                .dependencies
                .iter()
                .all(|dep| dep.index_url.is_none())
        );
    }

    #[test]
    fn an_original_is_roblox_and_aliases_default_to_the_declared_name() {
        let metadata: Metadata = serde_json::from_str(ORIGINAL).unwrap();
        let package = resolved(semver::Version::new(2, 1, 0), metadata, API_URL).unwrap();
        assert_eq!(package.environment, Some(Environment::Roblox));
        assert!(
            package.package_deprecated.is_some(),
            "archived reads as deprecated"
        );
        let DownloadSource::TarGz { url } = &package.source else {
            panic!("Forest serves tarballs");
        };
        assert!(
            url.ends_with("/5f2b8a0c0e1d4b7a9c3e6f8d1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b.tgz")
        );
        let edges: Vec<_> = package
            .dependencies
            .iter()
            .map(|dep| (dep.alias.as_str(), dep.name.as_str()))
            .collect();
        assert_eq!(
            edges,
            [("Cleaner", "stratiz/cleaner"), ("Signal", "stratiz/signal")]
        );
    }

    #[test]
    fn refuses_an_archive_hash_that_is_not_one() {
        for integrity in ["", "../../evil", &"g".repeat(64)] {
            let metadata = Metadata {
                dependencies: BTreeMap::new(),
                integrity: integrity.to_string(),
                mirrored_from: None,
                archived: false,
            };
            assert!(resolved(semver::Version::new(1, 0, 0), metadata, API_URL).is_err());
        }
    }
}
