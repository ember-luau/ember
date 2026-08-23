use super::{DownloadSource, ResolvedPackage, TransitiveDependency};
use crate::error::Error;
use crate::project::manifest::{Environment, split_package_name};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/** Version sent as the Wally-Version header. Wally registries answer 426 to
clients without a recent-enough one. */
pub const VERSION_HEADER: &str = "0.3.2";

/// Root config.json of a wally index.
#[derive(Deserialize)]
pub struct Config {
    pub api: String,
}

/** One JSON line of a wally index package file. Dev-dependencies aren't
modeled, embr never installs them. */
#[derive(Deserialize)]
struct Entry {
    package: EntryPackage,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "server-dependencies")]
    server_dependencies: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct EntryPackage {
    version: String,
    realm: String,
}

pub fn load_config(root: &Path) -> Result<Config, Error> {
    Ok(serde_json::from_str(&std::fs::read_to_string(
        root.join("config.json"),
    )?)?)
}

pub fn resolve(
    root: &Path,
    index_url: &str,
    config: &Config,
    name: &str,
    req: &semver::VersionReq,
) -> Result<ResolvedPackage, Error> {
    let (scope, package) = split_package_name(name)?;
    let path = root.join(scope).join(package);
    if !path.exists() {
        return Err(Error::PackageNotFound {
            name: name.to_string(),
            index: index_url.to_string(),
        });
    }

    /* One JSON object per line, one line per published version. Unparsable
    lines are skipped so one odd entry can't sink the whole resolve. */
    let best = std::fs::read_to_string(&path)?
        .lines()
        .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
        .filter_map(|entry| {
            let version = semver::Version::parse(&entry.package.version).ok()?;
            req.matches(&version).then_some((version, entry))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b));

    let Some((version, entry)) = best else {
        return Err(Error::NoMatchingVersion {
            name: name.to_string(),
            req: req.to_string(),
        });
    };

    /* Both realms' deps resolve in this same wally index. Each dep's own
    realm decides its install environment. */
    let dependencies = entry
        .dependencies
        .iter()
        .chain(entry.server_dependencies.iter())
        .filter_map(|(alias, spec)| parse_dependency(alias, spec))
        .collect();

    let download_url = format!(
        "{}/v1/package-contents/{scope}/{package}/{version}",
        config.api.trim_end_matches('/')
    );

    Ok(ResolvedPackage {
        version,
        environment: Some(Environment::from_wally_realm(&entry.package.realm)?),
        dependencies,
        source: DownloadSource::Zip { url: download_url },
    })
}

/// Wally dependency specs are "scope/name@version-requirement".
fn parse_dependency(alias: &str, spec: &str) -> Option<TransitiveDependency> {
    let (name, req) = spec.split_once('@')?;
    Some(TransitiveDependency {
        /* lowercased like every other name entering the install set, two
        case-variants of one package must never become two parallel jobs
        racing on the same storage folder */
        name: name.to_lowercase(),
        version_req: req.to_string(),
        index_url: None,
        alias: alias.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROMISE_LINE: &str = r#"{"package":{"name":"evaera/promise","version":"4.0.0","registry":"https://github.com/UpliftGames/wally-index","realm":"shared","description":"Promise implementation for Roblox","license":"MIT","authors":[],"include":[],"exclude":[]},"place":{},"dependencies":{},"server-dependencies":{},"dev-dependencies":{}}"#;

    #[test]
    fn parses_index_entry() {
        let entry: Entry = serde_json::from_str(PROMISE_LINE).unwrap();
        assert_eq!(entry.package.version, "4.0.0");
        assert_eq!(entry.package.realm, "shared");
        assert!(entry.dependencies.is_empty());
    }

    #[test]
    fn parses_dependency_specs() {
        let dep = parse_dependency("Promise", "evaera/Promise@^4.0.0").unwrap();
        assert_eq!(dep.name, "evaera/promise");
        assert_eq!(dep.version_req, "^4.0.0");
        assert_eq!(dep.alias, "Promise");
        assert!(parse_dependency("Broken", "missing-at-sign").is_none());
    }
}
