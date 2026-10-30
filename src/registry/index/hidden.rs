/*! Private and unlisted packages. The git index does not hold them, so the
ember index asks the API for a name that the git index misses. The call
sends the Ember token and the package's link key, and it happens only when
embr has one of them. forest.rs is the other index that resolves over HTTP. */

use super::{ResolvedPackage, pesde};
use crate::error::Error;
use crate::net::registry::{self, Access};
use crate::project::manifest::{DEFAULT_INDEX_URL, Environment, Language};
use serde::Deserialize;

/// `GET /v1/packages/{scope}/{name}`, the fields embr reads.
#[derive(Deserialize)]
struct Package {
    /// the git index entries, by "<version> <target>" as in the package file.
    versions: toml::Table,
    /// the package's deprecation reason, `[meta] deprecated` in the package file.
    #[serde(default)]
    deprecated: Option<String>,
}

/** `name` from the API, by the rules of the git index. None when embr has
neither a token nor a link key for it, so an anonymous miss makes no call. */
pub fn resolve(
    name: &str,
    req: &semver::VersionReq,
    prefer: Option<(Environment, Language)>,
    pinned: &[semver::Version],
) -> Result<Option<ResolvedPackage>, Error> {
    let access = Access::of(name)?;
    if access.is_empty() {
        return Ok(None);
    }
    resolve_at(&registry::api_url(), &access, name, req, prefer, pinned).map(Some)
}

fn resolve_at(
    api: &str,
    access: &Access,
    name: &str,
    req: &semver::VersionReq,
    prefer: Option<(Environment, Language)>,
    pinned: &[semver::Version],
) -> Result<ResolvedPackage, Error> {
    let package: Package = serde_json::from_str(&registry::package(api, name, access)?)
        .map_err(crate::net::http::error::HttpError::from)?;
    let mut file = package.versions;
    if let Some(reason) = package.deprecated {
        let meta = toml::Table::from_iter([("deprecated".to_string(), reason.into())]);
        file.insert("meta".to_string(), meta.into());
    }
    /* each entry has its `download` URL, so the config needs no `api`. a
    dependency with no index stays in the ember index, where a hidden
    dependency resolves through here again */
    let config = pesde::Config {
        api: None,
        github_oauth_id: None,
    };
    let found = pesde::entries(&file.into(), DEFAULT_INDEX_URL, name)?;
    pesde::pick(found, DEFAULT_INDEX_URL, &config, name, req, prefer, pinned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::http::mock;
    use crate::registry::index::DownloadSource;

    // the shape of the API's answer: the git entries, plus `language` and `visibility`
    const BODY: &str = r#"{"name":"acme/secret","visibility":"private","versions":{
        "1.0.0 roblox":{"version":"1.0.0","target":"roblox","language":"luau",
            "download":"API/v1/packages/acme/secret/files/1.0.0/roblox.tar.gz",
            "dependencies":{"Other":{"name":"acme/other","version":"^2.0.0"}}},
        "1.1.0 roblox":{"version":"1.1.0","target":"roblox","language":"luau","yanked":true,
            "download":"API/v1/packages/acme/secret/files/1.1.0/roblox.tar.gz"},
        "2.0.0-rc.1 roblox":{"version":"2.0.0-rc.1","target":"roblox","language":"luau",
            "download":"API/v1/packages/acme/secret/files/2.0.0-rc.1/roblox.tar.gz"}},
        "deprecated":"use acme/open"}"#;

    fn api() -> (String, mock::Log) {
        mock::serve(|request| {
            let base = request
                .lines()
                .find_map(|line| line.strip_prefix("host: "))
                .map(|host| format!("http://{host}"))
                .unwrap_or_default();
            match request.split(' ').nth(1) {
                Some("/v1/packages/acme/secret") => {
                    mock::reply("200 OK", &BODY.replace("API", &base))
                }
                _ => mock::reply("404 Not Found", r#"{"error":"package not found"}"#),
            }
        })
    }

    fn any() -> semver::VersionReq {
        semver::VersionReq::STAR
    }

    #[test]
    fn a_hidden_package_resolves_by_the_git_index_rules() {
        let (api, log) = api();
        let access = Access {
            token: Some("ember_abc".to_string()),
            key: None,
        };
        let package = resolve_at(&api, &access, "acme/secret", &any(), None, &[]).unwrap();

        // 1.1.0 is yanked and 2.0.0-rc.1 is a prerelease, so 1.0.0 wins
        assert_eq!(package.version, semver::Version::new(1, 0, 0));
        assert_eq!(package.environment, Some(Environment::Roblox));
        assert_eq!(package.package_deprecated.as_deref(), Some("use acme/open"));
        assert_eq!(package.dependencies[0].name, "acme/other");
        assert_eq!(package.dependencies[0].index_url, None);
        assert_eq!(
            package.source,
            DownloadSource::TarGz {
                url: format!("{api}/v1/packages/acme/secret/files/1.0.0/roblox.tar.gz")
            }
        );
        // a lockfile that pins the yanked version keeps it
        let pinned = [semver::Version::new(1, 1, 0)];
        let package = resolve_at(&api, &access, "acme/secret", &any(), None, &pinned).unwrap();
        assert_eq!(package.version, semver::Version::new(1, 1, 0));

        let first = log.lock().unwrap()[0].clone();
        assert!(first.contains("authorization: bearer ember_abc"), "{first}");
        assert!(!first.contains("ember-link-key"), "{first}");
    }

    #[test]
    fn a_link_key_goes_in_its_header_and_a_404_hints_without_a_token() {
        let (api, log) = api();
        let access = Access {
            token: None,
            key: Some("0123456789abcdef0123456789abcdef".to_string()),
        };
        assert!(resolve_at(&api, &access, "acme/secret", &any(), None, &[]).is_ok());
        let first = log.lock().unwrap()[0].clone();
        assert!(
            first.contains("ember-link-key: 0123456789abcdef0123456789abcdef"),
            "{first}"
        );
        assert!(!first.contains("authorization"), "{first}");

        let missing = resolve_at(&api, &access, "acme/gone", &any(), None, &[])
            .err()
            .expect("an error");
        assert!(
            matches!(
                missing,
                Error::HiddenNotFound {
                    signed_in: false,
                    ..
                }
            ),
            "{missing}"
        );
        assert!(missing.to_string().contains("embr auth login"), "{missing}");

        let signed_in = Access {
            token: Some("ember_abc".to_string()),
            key: None,
        };
        let missing = resolve_at(&api, &signed_in, "acme/gone", &any(), None, &[])
            .err()
            .expect("an error");
        assert_eq!(missing.to_string(), "Package acme/gone was not found");
    }

    #[test]
    fn a_bad_token_asks_for_a_new_sign_in_and_a_403_is_no_rate_limit() {
        let (api, _) = mock::serve(|request| {
            if request.contains("ember_revoked") {
                mock::reply("401 Unauthorized", r#"{"error":"revoked"}"#)
            } else {
                mock::reply("403 Forbidden", r#"{"error":"this token may not read"}"#)
            }
        });
        let access = |token: &str| Access {
            token: Some(token.to_string()),
            key: None,
        };
        let revoked = resolve_at(&api, &access("ember_revoked"), "acme/x", &any(), None, &[]);
        assert!(matches!(revoked, Err(Error::TokenRejected)));
        let refused = resolve_at(&api, &access("ember_other"), "acme/x", &any(), None, &[])
            .err()
            .expect("an error")
            .to_string();
        assert!(refused.contains("this token may not read"), "{refused}");
    }
}
