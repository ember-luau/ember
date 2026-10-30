/*!
embr's own registry API at api.emberpm.dev.

Public packages resolve and install through the git index clone and the
CDN URLs in its entries, so they work even when the API is down. The API is
the index repo's only writer: `POST /v1/publish` checks the tarball, stores
it, and writes the index entry. Yank and deprecate also go here.

Private and unlisted packages are not in the git index. They resolve and
download here, with the Ember token and the package's link key. Only the API
host gets those, see [`on_api_host`]. `embr mcp` also reads here, for its
package search.
*/

use crate::error::Error;
use crate::net::auth;
use crate::net::http;
use crate::net::http::error::HttpError;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

pub const API_URL: &str = "https://api.emberpm.dev";

/** the hosts before emberpm.dev, each with the host that replaced it. old
index entries and lock files name the old hosts, which serve the same files. */
const MOVED: [(&str, &str); 2] = [
    ("https://api.luaupm.com", API_URL),
    ("https://cdn.luaupm.com", "https://cdn.emberpm.dev"),
];

/// points embr at another API, such as a local backend. unset means [`API_URL`].
pub const API_VAR: &str = "EMBER_API_URL";

/// the header that carries an unlisted package's link key.
const LINK_KEY_HEADER: &str = "Ember-Link-Key";

/// the API base, from EMBER_API_URL when it is set, with no trailing slash.
pub fn api_url() -> String {
    std::env::var(API_VAR)
        .ok()
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| API_URL.to_string())
}

/// scheme, host and port of `url`, in lowercase.
fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    Some(format!("{scheme}://{host}").to_ascii_lowercase())
}

/// `url` on the host that replaced its old host. other URLs stay as they are.
pub fn moved(url: &str) -> String {
    let Some(host) = origin(url) else {
        return url.to_string();
    };
    match MOVED.iter().find(|(old, _)| *old == host) {
        // the origin is `url`'s first bytes, in lowercase, so the length holds
        Some((_, new)) => format!("{new}{}", &url[host.len()..]),
        None => url.to_string(),
    }
}

/** whether `url` is on the API host `api`. the scheme, host and port must
be equal. only this host gets the Ember token and link keys, so an index
entry's URL cannot collect them. */
pub fn on_api_host(url: &str, api: &str) -> bool {
    origin(url).is_some_and(|host| origin(api) == Some(host))
}

/// link keys from the manifests that embr read in this run, by package name.
static KEYS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// whether `key` has the shape of a link key: 32 hex characters.
pub fn is_link_key(key: &str) -> bool {
    key.len() == 32 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// keeps `key` for the API calls about `name` in this run.
pub fn remember_key(name: &str, key: &str) {
    KEYS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(name.to_lowercase(), key.to_lowercase());
}

/// what embr sends the API about one package. each part is there when embr has it.
#[derive(Default)]
pub struct Access {
    pub token: Option<String>,
    pub key: Option<String>,
}

impl Access {
    /// the token from EMBER_TOKEN or the stored login, and the link key for `name`.
    pub fn of(name: &str) -> Result<Access, Error> {
        let key = KEYS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&name.to_lowercase())
            .cloned();
        Ok(Access {
            token: auth::token()?,
            key,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.token.is_none() && self.key.is_none()
    }

    /** GETs `url`, a URL about the package `name`, with the token and the
    key. the agent follows no redirect, so neither header can go on to
    another host. */
    fn get(&self, url: &str, name: &str) -> Result<ureq::Response, Error> {
        let mut request = http::manual_redirect_agent()
            .get(url)
            .set("User-Agent", http::USER_AGENT);
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        if let Some(key) = &self.key {
            request = request.set(LINK_KEY_HEADER, key);
        }
        request.call().map_err(|error| refused(error, name, self))
    }
}

/** `GET {api}/v1/packages/{name}` as `access`, the JSON body. it holds the
git index entries under `versions`. */
pub fn package(api: &str, name: &str, access: &Access) -> Result<String, Error> {
    let response = access.get(&format!("{api}/v1/packages/{name}"), name)?;
    Ok(response.into_string()?)
}

/** downloads `url`, a file on the API host, with what embr holds for its
package. */
pub fn download(url: &str, on_bytes: &mut dyn FnMut(u64, Option<u64>)) -> Result<Vec<u8>, Error> {
    // a files URL is {api}/v1/packages/{scope}/{name}/files/...
    let name = url
        .split_once("/v1/packages/")
        .map(|(_, path)| path.split('/').take(2).collect::<Vec<_>>().join("/"))
        .unwrap_or_default();
    download_as(url, &name, &Access::of(&name)?, on_bytes)
}

/** [`download`] as `access`. a public package's files answer with a
redirect to the CDN. embr follows it without the token and the key. */
fn download_as(
    url: &str,
    name: &str,
    access: &Access,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Vec<u8>, Error> {
    let response = access.get(url, name)?;
    if !(300..400).contains(&response.status()) {
        return Ok(http::read_body(response, on_bytes)?);
    }
    let Some(location) = response.header("location") else {
        return Err(HttpError::RedirectMissingLocation {
            url: url.to_string(),
        }
        .into());
    };
    let next = match location.strip_prefix('/') {
        Some(path) => format!("{}/{path}", origin(url).unwrap_or_default()),
        None => location.to_string(),
    };
    Ok(http::get_bytes_with(&next, &[], on_bytes)?)
}

/** the error for an API answer that is not a success. a 401 asks for a new
sign-in. a 404 looks the same for a missing package and for one that the
caller may not see. */
fn refused(error: ureq::Error, name: &str, access: &Access) -> Error {
    match error {
        ureq::Error::Status(404, _) => Error::HiddenNotFound {
            name: name.to_string(),
            signed_in: access.token.is_some(),
        },
        other => api_error(other),
    }
}

/// the error for any failed API call. a 403 is a refusal, not a rate limit.
pub fn api_error(error: ureq::Error) -> Error {
    match error {
        ureq::Error::Status(401, _) => Error::TokenRejected,
        ureq::Error::Status(status, response) => Error::RegistryRefused {
            status,
            message: error_message(status, response.into_string().ok().as_deref()),
        },
        other => HttpError::from(other).into(),
    }
}

/** The API's hard cap on publish bodies, 413 above it. Checked client-side
so an oversized archive fails before uploading. */
pub const MAX_ARCHIVE_BYTES: usize = 10 * 1024 * 1024;

/// The API's cap on a `[package] logo` file, 400 above it.
pub const MAX_LOGO_BYTES: u64 = 512 * 1024;

/// The API rejects [package] descriptions longer than this with a 400.
pub const MAX_DESCRIPTION_CHARS: usize = 200;

/// Every API error is `{"error": "<human readable message>"}`.
#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

/** Uploads a packed `.tar.gz` to `POST /v1/publish` as the given user.
The API answers 401 bad token, 400 malformed tarball or ember.toml, 403
scope owned by someone else since first publish claims a scope, 409
version already exists, 413 over the size cap. `oidc_token` is the
Actions provenance token when there is one. It rides along in its own
header, auth stays the Ember token. */
pub fn publish(token: &str, oidc_token: Option<&str>, archive: &[u8]) -> Result<(), Error> {
    let url = format!("{}/v1/publish", api_url());
    // bulk agent, a package archive on a slow uplink shouldn't die at the minute mark
    let mut request = http::bulk_agent()
        .post(&url)
        .set("User-Agent", http::USER_AGENT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/gzip");
    if let Some(oidc_token) = oidc_token {
        request = request.set(crate::net::provenance::HEADER, oidc_token);
    }
    let response = request.send_bytes(archive);

    match response {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(status, response)) => Err(Error::PublishFailed {
            status,
            message: error_message(status, response.into_string().ok().as_deref()),
        }),
        // Transport errors like DNS or TLS have no response body to read.
        Err(other) => Err(HttpError::from(other).into()),
    }
}

/** sets or clears `yanked` on a version's index entries through
`PUT`/`DELETE /v1/packages/{scope}/{name}/{version}/yank`. without a
`target` ID every target of the version changes. owners only, the same
auth as publish. both calls are idempotent. */
pub fn yank(
    token: &str,
    name: &str,
    version: &semver::Version,
    target: Option<&str>,
    undo: bool,
) -> Result<(), Error> {
    let path = format!("/v1/packages/{name}/{version}/yank");
    owner_call(token, "yank", &path, target, (!undo).then_some(None))
}

/** sets or clears a deprecation through `PUT`/`DELETE` on
`/v1/packages/{scope}/{name}/deprecate`, or on `.../{version}/deprecate` for
one version. `reason` None clears it. the package's reason is pesde's
`[meta] deprecated`, a version's is `deprecated` on its entries. */
pub fn deprecate(
    token: &str,
    name: &str,
    version: Option<&semver::Version>,
    target: Option<&str>,
    reason: Option<&str>,
) -> Result<(), Error> {
    let path = match version {
        Some(version) => format!("/v1/packages/{name}/{version}/deprecate"),
        None => format!("/v1/packages/{name}/deprecate"),
    };
    let body = reason.map(|reason| Some(serde_json::json!({ "reason": reason })));
    owner_call(token, "deprecation", &path, target, body)
}

/** one call an owner makes. `body` None sends a DELETE, Some a PUT with
that JSON, or with no body at all. */
fn owner_call(
    token: &str,
    action: &'static str,
    path: &str,
    target: Option<&str>,
    body: Option<Option<serde_json::Value>>,
) -> Result<(), Error> {
    let mut url = format!("{}{path}", api_url());
    if let Some(target) = target {
        url.push_str(&format!("?target={target}"));
    }
    let method = if body.is_some() { "PUT" } else { "DELETE" };
    let request = http::agent()
        .request(method, &url)
        .set("User-Agent", http::USER_AGENT)
        .set("Authorization", &format!("Bearer {token}"));
    let response = match body.flatten() {
        Some(json) => request.send_json(json),
        None => request.call(),
    };

    match response {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(status, response)) => Err(Error::OwnerActionFailed {
            action,
            status,
            message: error_message(status, response.into_string().ok().as_deref()),
        }),
        Err(other) => Err(HttpError::from(other).into()),
    }
}

fn error_message(status: u16, body: Option<&str>) -> String {
    body.and_then(|body| serde_json::from_str::<ErrorBody>(body).ok())
        .map(|body| body.error)
        .unwrap_or_else(|| format!("the registry answered HTTP {status}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::http::mock;

    #[test]
    fn only_the_api_host_gets_credentials() {
        let api = API_URL;
        assert!(on_api_host(
            "https://api.emberpm.dev/v1/packages/a/b/files/x",
            api
        ));
        assert!(on_api_host("https://API.emberpm.dev/v1/x", api));
        assert!(on_api_host("https://api.emberpm.dev", api));
        assert!(on_api_host("https://api.emberpm.dev?x", api));
        /* the CDN, the old host, plain HTTP, another port, and look-alike
        hosts get nothing. [`moved`] sends an old host's URL here first */
        for host in ["api.emberpm.dev", "api.luaupm.com"] {
            for other in [
                "https://cdn.emberpm.dev/acme/foo/1.0.0.tar.gz".to_string(),
                "https://cdn.luaupm.com/acme/foo/1.0.0.tar.gz".to_string(),
                format!("http://{host}/v1/x"),
                format!("https://{host}:8443/v1/x"),
                format!("https://{host}.example.com/v1/x"),
                format!("https://{host}@example.com/v1/x"),
                format!("https://example.com/https://{host}/v1/x"),
                format!("{host}/v1/x"),
                "https://api.luaupm.com/v1/packages/a/b/files/x".to_string(),
            ] {
                assert!(!on_api_host(&other, api), "{other}");
            }
        }
        // an API base override is the host then, for local tests
        assert!(on_api_host(
            "http://127.0.0.1:8787/v1/x",
            "http://127.0.0.1:8787"
        ));
        for host in ["https://api.emberpm.dev", "https://api.luaupm.com"] {
            assert!(!on_api_host(
                &format!("{host}/v1/x"),
                "http://127.0.0.1:8787"
            ));
        }
    }

    #[test]
    fn old_hosts_move_to_emberpm_dev() {
        for (old, new) in [
            (
                "https://cdn.luaupm.com/acme/foo/1.0.0.tar.gz",
                "https://cdn.emberpm.dev/acme/foo/1.0.0.tar.gz",
            ),
            (
                "https://CDN.luaupm.com/acme/Foo/1.0.0.tar.gz",
                "https://cdn.emberpm.dev/acme/Foo/1.0.0.tar.gz",
            ),
            (
                "https://api.luaupm.com/v1/packages/a/b/files/x?k=1",
                "https://api.emberpm.dev/v1/packages/a/b/files/x?k=1",
            ),
            ("https://cdn.luaupm.com", "https://cdn.emberpm.dev"),
        ] {
            assert_eq!(moved(old), new);
        }
        // other hosts and look-alikes stay as they are
        for url in [
            "https://cdn.emberpm.dev/acme/foo/1.0.0.tar.gz",
            "https://cdn.luaupm.com.example.com/acme/foo/1.0.0.tar.gz",
            "https://cdn.luaupm.com:8443/acme/foo/1.0.0.tar.gz",
            "http://cdn.luaupm.com/acme/foo/1.0.0.tar.gz",
            "https://example.com/https://cdn.luaupm.com/x",
            "cdn.luaupm.com/x",
        ] {
            assert_eq!(moved(url), url);
        }
    }

    /** the files route answers a public package with a redirect to the CDN.
    the CDN gets neither the token nor the key. */
    #[test]
    fn a_redirect_leaves_the_credentials_behind() {
        let (cdn, cdn_log) = mock::serve(|_| mock::reply("200 OK", "tarball"));
        let to_cdn = format!(
            "HTTP/1.1 301 Moved Permanently\r\nLocation: {cdn}/acme/foo/1.0.0.tar.gz\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (api, api_log) = mock::serve(move |_| to_cdn.clone());
        let access = Access {
            token: Some("ember_abc".to_string()),
            key: Some("0123456789abcdef0123456789abcdef".to_string()),
        };

        let url = format!("{api}/v1/packages/acme/foo/files/1.0.0.tar.gz");
        let bytes = download_as(&url, "acme/foo", &access, &mut |_, _| {}).unwrap();
        assert_eq!(bytes, b"tarball");

        let sent = api_log.lock().unwrap()[0].clone();
        assert!(sent.contains("authorization: bearer ember_abc"), "{sent}");
        assert!(sent.contains("ember-link-key: 0123456789abcdef"), "{sent}");
        let followed = cdn_log.lock().unwrap()[0].clone();
        assert!(
            followed.starts_with("get /acme/foo/1.0.0.tar.gz "),
            "{followed}"
        );
        assert!(!followed.contains("authorization"), "{followed}");
        assert!(!followed.contains("ember-link-key"), "{followed}");
        assert!(!followed.contains("ember_abc"), "{followed}");
    }

    #[test]
    fn link_keys_have_one_shape() {
        assert!(is_link_key("0123456789abcdef0123456789abcdef"));
        assert!(!is_link_key("0123456789abcdef"));
        assert!(!is_link_key("0123456789abcdef0123456789abcdeg"));
        assert!(!is_link_key("0123456789abcdef0123456789abcde\n"));
    }

    #[test]
    fn error_messages_come_from_the_body_when_parsable() {
        assert_eq!(
            error_message(409, Some(r#"{"error":"version 1.0.0 already exists"}"#)),
            "version 1.0.0 already exists"
        );
        // Non-JSON bodies, a proxy error page say, fall back to the status.
        assert_eq!(
            error_message(502, Some("<html>Bad Gateway</html>")),
            "the registry answered HTTP 502"
        );
        assert_eq!(error_message(500, None), "the registry answered HTTP 500");
    }
}
