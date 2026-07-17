use super::{DownloadSource, ResolvedPackage, TransitiveDependency};
use crate::error::Error;
use crate::manifest::{Environment, split_package_name};
use serde::Deserialize;
use std::path::Path;
use toml::Value;

const DEFAULT_DOWNLOAD_TEMPLATE: &str =
    "{API_URL}/v1/packages/{PACKAGE}/{PACKAGE_VERSION}/{PACKAGE_TARGET}/archive";

/// Root config.toml of a pesde-format index. `api` may be absent for lpm
/// indices whose entries all carry direct download URLs.
#[derive(Deserialize)]
pub struct Config {
    #[serde(default)]
    pub api: Option<String>,
    /// Optional download URL template with {API_URL}, {PACKAGE},
    /// {PACKAGE_VERSION}, and {PACKAGE_TARGET} placeholders.
    #[serde(default)]
    pub download: Option<String>,
    #[serde(default)]
    pub github_oauth_id: Option<String>,
}

pub fn load_config(root: &Path) -> Result<Config, Error> {
    Ok(toml::from_str(&std::fs::read_to_string(
        root.join("config.toml"),
    )?)?)
}

struct Candidate {
    version: semver::Version,
    /// Raw target string from the entry key ("luau", "roblox", ...).
    target: Option<String>,
    environment: Option<Environment>,
    entry: Value,
}

pub fn resolve(
    root: &Path,
    index_url: &str,
    config: &Config,
    name: &str,
    req: &semver::VersionReq,
    prefer_environment: Option<Environment>,
) -> Result<ResolvedPackage, Error> {
    let (scope, package) = split_package_name(name)?;
    let path = root.join(scope).join(package);
    if !path.exists() {
        return Err(Error::PackageNotFound {
            name: name.to_string(),
            index: index_url.to_string(),
        });
    }

    let file: Value = toml::from_str(&std::fs::read_to_string(&path)?)?;
    // Entries live either at the top level or under an "entries" table,
    // keyed by "<version> <target>" (target optional in lpm indices).
    let entries = match file.get("entries") {
        Some(Value::Table(entries)) => entries,
        _ => match file.as_table() {
            Some(table) => table,
            None => {
                return Err(Error::PackageNotFound {
                    name: name.to_string(),
                    index: index_url.to_string(),
                });
            }
        },
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    for (key, entry) in entries {
        let (version_part, target) = match key.split_once(' ') {
            Some((version, target)) => (version, Some(target.to_string())),
            None => (key.as_str(), None),
        };
        let Ok(version) = semver::Version::parse(version_part) else {
            continue; // not a version entry (e.g. metadata keys)
        };
        if !req.matches(&version) {
            continue;
        }

        let environment = entry
            .get("target")
            .and_then(|target| target.get("environment"))
            .and_then(Value::as_str)
            .or(target.as_deref())
            .map(parse_environment)
            .transpose()?;

        candidates.push(Candidate {
            version,
            target,
            environment,
            entry: entry.clone(),
        });
    }

    let Some(best_version) = candidates.iter().map(|c| c.version.clone()).max() else {
        return Err(Error::NoMatchingVersion {
            name: name.to_string(),
            req: req.to_string(),
        });
    };

    // Among the targets published for the best version, prefer the one
    // matching the project's environment; otherwise take the first.
    let mut same_version: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| c.version == best_version)
        .collect();
    let picked = prefer_environment
        .and_then(|preferred| {
            same_version
                .iter()
                .position(|c| c.environment == Some(preferred))
        })
        .unwrap_or(0);
    let candidate = same_version.swap_remove(picked);

    let dependencies = parse_dependencies(&candidate.entry, index_url)?;
    let source = download_source(config, index_url, name, &candidate)?;

    Ok(ResolvedPackage {
        version: candidate.version,
        environment: candidate.environment,
        dependencies,
        source,
    })
}

/// Accepts both lpm environment names and pesde's ("roblox" -> shared, ...).
fn parse_environment(environment: &str) -> Result<Environment, Error> {
    Environment::from_lpm(environment).or_else(|_| Environment::from_pesde(environment))
}

fn download_source(
    config: &Config,
    index_url: &str,
    name: &str,
    candidate: &Candidate,
) -> Result<DownloadSource, Error> {
    // lpm index entries carry a direct URL.
    if let Some(url) = candidate.entry.get("download").and_then(Value::as_str) {
        return Ok(DownloadSource::TarGz {
            url: url.to_string(),
        });
    }

    let (Some(api), Some(target)) = (config.api.as_deref(), candidate.target.as_deref()) else {
        return Err(Error::IndexFetch {
            url: index_url.to_string(),
            reason: format!("entry for {name} has no download url and the index config has no api"),
        });
    };

    let template = config
        .download
        .as_deref()
        .unwrap_or(DEFAULT_DOWNLOAD_TEMPLATE);
    let url = template
        .replace("{API_URL}", api.trim_end_matches('/'))
        .replace("{PACKAGE}", &name.replace('/', "%2F"))
        .replace("{PACKAGE_VERSION}", &candidate.version.to_string())
        .replace("{PACKAGE_TARGET}", target);
    Ok(DownloadSource::TarGz { url })
}

fn parse_dependencies(entry: &Value, index_url: &str) -> Result<Vec<TransitiveDependency>, Error> {
    let Some(Value::Table(dependencies)) = entry.get("dependencies") else {
        return Ok(Vec::new());
    };

    let mut parsed = Vec::new();
    for spec in dependencies.values() {
        // Specs are either the specifier table itself or [specifier, kind].
        let spec = match spec {
            Value::Array(pair) => match pair.first() {
                Some(first) => first,
                None => continue,
            },
            other => other,
        };

        // Dev dependencies of upstream packages are not ours to install.
        if spec.get("workspace").is_some() || spec.get("repo").is_some() {
            continue; // workspace/git specifiers can't be resolved from here
        }

        let is_wally = spec.get("wally").is_some();
        let Some(name) = spec
            .get("name")
            .or_else(|| spec.get("wally"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let version_req = spec
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("*")
            .to_string();
        let index = spec.get("index").and_then(Value::as_str);

        // In published index entries the dep's index is normally a URL.
        let index_url = match index {
            Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
                Some(url.to_string())
            }
            None | Some("default") => {
                if is_wally {
                    // A wally dep can't live in this pesde index.
                    return Err(Error::UnknownIndex(format!(
                        "wally dependency {name} (from an entry in {index_url}) names no index url"
                    )));
                }
                None
            }
            Some(alias) => return Err(Error::UnknownIndex(alias.to_string())),
        };

        // Wally names may contain uppercase in old entries; normalize.
        parsed.push(TransitiveDependency {
            name: name.to_lowercase(),
            version_req,
            index_url,
        });
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_entries(toml_text: &str) -> Value {
        toml::from_str(toml_text).unwrap()
    }

    #[test]
    fn parses_environments_from_both_namings() {
        assert_eq!(parse_environment("shared").unwrap(), Environment::Shared);
        assert_eq!(parse_environment("roblox").unwrap(), Environment::Shared);
        assert_eq!(
            parse_environment("roblox_server").unwrap(),
            Environment::Server
        );
        assert_eq!(parse_environment("lute").unwrap(), Environment::Lute);
        assert!(parse_environment("cobol").is_err());
    }

    #[test]
    fn parses_pesde_style_dependencies() {
        let entry = parse_entries(
            r#"
            [dependencies]
            foo = { name = "acme/foo", version = "^1.0.0" }
            bar = [{ name = "acme/bar", version = "2.0.0", index = "https://github.com/acme/index" }, "standard"]
            "#,
        );

        let deps = parse_dependencies(&entry, "https://example.com/index").unwrap();
        assert_eq!(deps.len(), 2);
        let foo = deps.iter().find(|d| d.name == "acme/foo").unwrap();
        assert_eq!(foo.version_req, "^1.0.0");
        assert_eq!(foo.index_url, None);
        let bar = deps.iter().find(|d| d.name == "acme/bar").unwrap();
        assert_eq!(
            bar.index_url.as_deref(),
            Some("https://github.com/acme/index")
        );
    }

    #[test]
    fn wally_dependency_requires_index_url() {
        let entry = parse_entries(
            r#"
            [dependencies]
            promise = { wally = "evaera/promise", version = "^4.0.0", index = "https://github.com/UpliftGames/wally-index" }
            "#,
        );
        let deps = parse_dependencies(&entry, "https://example.com/index").unwrap();
        assert_eq!(deps[0].name, "evaera/promise");
        assert_eq!(
            deps[0].index_url.as_deref(),
            Some("https://github.com/UpliftGames/wally-index")
        );

        let bad = parse_entries(
            r#"
            [dependencies]
            promise = { wally = "evaera/promise", version = "^4.0.0" }
            "#,
        );
        assert!(parse_dependencies(&bad, "https://example.com/index").is_err());
    }

    #[test]
    fn builds_download_url_from_default_template() {
        let config = Config {
            api: Some("https://registry.example.com/".to_string()),
            download: None,
            github_oauth_id: None,
        };
        let candidate = Candidate {
            version: semver::Version::new(1, 0, 2),
            target: Some("luau".to_string()),
            environment: Some(Environment::Luau),
            entry: parse_entries(""),
        };

        let source = download_source(
            &config,
            "https://example.com/index",
            "pesde/hello",
            &candidate,
        )
        .unwrap();
        let DownloadSource::TarGz { url } = source else {
            panic!("expected tarball source");
        };
        assert_eq!(
            url,
            "https://registry.example.com/v1/packages/pesde%2Fhello/1.0.2/luau/archive"
        );
    }

    #[test]
    fn prefers_direct_download_urls() {
        let config = Config {
            api: None,
            download: None,
            github_oauth_id: None,
        };
        let candidate = Candidate {
            version: semver::Version::new(0, 1, 0),
            target: None,
            environment: Some(Environment::Luau),
            entry: parse_entries(r#"download = "https://example.com/pkg.tar.gz""#),
        };

        let source = download_source(
            &config,
            "https://example.com/index",
            "scope/pkg",
            &candidate,
        )
        .unwrap();
        let DownloadSource::TarGz { url } = source else {
            panic!("expected tarball source");
        };
        assert_eq!(url, "https://example.com/pkg.tar.gz");
    }
}
