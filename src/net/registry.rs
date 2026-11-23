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

/// the header of a publish that names the channel to set.
const CHANNEL_HEADER: &str = "X-Ember-Channel";

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

/** the package answer of `name` as JSON, with the token and the link key
that embr holds for it. beside `versions` it has the keys that no index
file has, such as `trust` and `notes`. */
pub fn package_answer(name: &str) -> Result<serde_json::Value, Error> {
    let body = package(&api_url(), name, &Access::of(name)?)?;
    Ok(serde_json::from_str(&body).map_err(HttpError::from)?)
}

/** each version of `name` that the registry holds, yanked ones included.
none for a package that the registry does not have, or does not show to
this caller: the publish that follows then gets the refusal. */
pub fn published_versions(name: &str) -> Result<Vec<semver::Version>, Error> {
    let answer = match package_answer(name) {
        Err(Error::HiddenNotFound { .. }) => return Ok(Vec::new()),
        answer => answer?,
    };
    // a key is "<version> <target>", as in the package file
    Ok(answer["versions"]
        .as_object()
        .into_iter()
        .flat_map(|versions| versions.keys())
        .filter_map(|key| semver::Version::parse(key.split(' ').next()?).ok())
        .collect())
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
Actions OIDC token when there is one. It rides along in its own header.
next to an Ember `token` it is provenance only. with no token it is the
credential, and the API takes it when the package names this workflow as
a trusted publisher. `channel` goes in `X-Ember-Channel`: the API then
sets that channel to the version of the archive, in the same index commit. */
pub fn publish(
    token: Option<&str>,
    oidc_token: Option<&str>,
    archive: &[u8],
    channel: Option<&str>,
) -> Result<(), Error> {
    publish_at(&api_url(), token, oidc_token, archive, channel)
}

fn publish_at(
    api: &str,
    token: Option<&str>,
    oidc_token: Option<&str>,
    archive: &[u8],
    channel: Option<&str>,
) -> Result<(), Error> {
    let url = format!("{api}/v1/publish");
    /* no overall deadline, a package archive on a slow uplink shouldn't die
    at the minute mark. no redirect: the OIDC token is a custom header, and
    ureq sends those on to another host */
    let mut request = http::manual_redirect_agent()
        .post(&url)
        .set("User-Agent", http::USER_AGENT)
        .set("Content-Type", "application/gzip");
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    if let Some(oidc_token) = oidc_token {
        request = request.set(crate::net::provenance::HEADER, oidc_token);
    }
    if let Some(channel) = channel {
        request = request.set(CHANNEL_HEADER, channel);
    }
    let response = http::no_redirect(request.send_bytes(archive));

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
    let method = if undo { "DELETE" } else { "PUT" };
    owner_call(&api_url(), token, "yank", method, &path, target, None).map(drop)
}

/** sets or clears a deprecation through `PUT`/`DELETE` on
`/v1/packages/{scope}/{name}/deprecate`, or on `.../{version}/deprecate` for
one version. `reason` None clears it. the package's reason is pesde's
`[meta] deprecated`, a version's is `deprecated` on its entries.
`replacement` is the package to use instead, `[meta] replacement`, for the
whole package only. */
pub fn deprecate(
    token: &str,
    name: &str,
    version: Option<&semver::Version>,
    target: Option<&str>,
    reason: Option<&str>,
    replacement: Option<&str>,
) -> Result<(), Error> {
    let path = match version {
        Some(version) => format!("/v1/packages/{name}/{version}/deprecate"),
        None => format!("/v1/packages/{name}/deprecate"),
    };
    let api = api_url();
    let call = |method, body| owner_call(&api, token, "deprecation", method, &path, target, body);
    match reason {
        Some(reason) => call(
            "PUT",
            Some(serde_json::json!({ "reason": reason, "replacement": replacement })),
        ),
        None => call("DELETE", None),
    }
    .map(drop)
}

/** moves `channel` of `name` to `version`, a published version that is not
yanked, through `PUT /v1/packages/{scope}/{name}/channels/{channel}`.
`version` None removes the channel through `DELETE`. owners only, as for
a deprecation. */
pub fn channel(
    token: &str,
    name: &str,
    channel: &str,
    version: Option<&semver::Version>,
) -> Result<(), Error> {
    let path = format!("/v1/packages/{name}/channels/{channel}");
    let api = api_url();
    let call = |method, body| owner_call(&api, token, "channel change", method, &path, None, body);
    match version {
        Some(version) => call(
            "PUT",
            Some(serde_json::json!({ "version": version.to_string() })),
        ),
        None => call("DELETE", None),
    }
    .map(drop)
}

/** sets the release notes of a version through `PUT
/v1/packages/{scope}/{name}/{version}/notes`, with the Markdown as the
body. `body` None removes them through `DELETE`. `oidc_token` is the
credential of a trusted publisher that has no Ember token, in the header
of a publish. */
pub fn notes(
    token: Option<&str>,
    oidc_token: Option<&str>,
    name: &str,
    version: &semver::Version,
    body: Option<&str>,
) -> Result<(), Error> {
    let url = format!("{}/v1/packages/{name}/{version}/notes", api_url());
    let method = if body.is_some() { "PUT" } else { "DELETE" };
    let mut request = http::manual_redirect_agent()
        .request(method, &url)
        .set("User-Agent", http::USER_AGENT);
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    } else if let Some(oidc_token) = oidc_token {
        request = request.set(crate::net::provenance::HEADER, oidc_token);
    }
    let response = match body {
        Some(body) => request
            .set("Content-Type", "text/markdown")
            .send_string(body),
        None => request.call(),
    };
    owner_answer("release notes", response).map(drop)
}

/** posts a notice on `name`, or on the versions that the semver range
`versions` takes, through `POST /v1/packages/{scope}/{name}/notices`.
`level` is `notice` or `warning`. returns the notice's id. */
pub fn post_notice(
    token: &str,
    name: &str,
    level: &str,
    message: &str,
    versions: Option<&str>,
) -> Result<String, Error> {
    post_notice_at(&api_url(), token, name, level, message, versions)
}

fn post_notice_at(
    api: &str,
    token: &str,
    name: &str,
    level: &str,
    message: &str,
    versions: Option<&str>,
) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Answer {
        notice: Posted,
    }
    #[derive(Deserialize)]
    struct Posted {
        id: String,
    }
    let body = serde_json::json!({ "level": level, "message": message, "versions": versions });
    let path = format!("/v1/packages/{name}/notices");
    let answer = owner_call(api, token, "notice", "POST", &path, None, Some(body))?;
    let answer: Answer = serde_json::from_str(&answer).map_err(HttpError::from)?;
    Ok(answer.notice.id)
}

/// removes notice `id` from `name`, `DELETE /v1/packages/{scope}/{name}/notices/{id}`.
pub fn remove_notice(token: &str, name: &str, id: &str) -> Result<(), Error> {
    let path = format!("/v1/packages/{name}/notices/{id}");
    owner_call(&api_url(), token, "removal", "DELETE", &path, None, None).map(drop)
}

/** one call an owner makes, with `body` as JSON or with no body. returns
the answer's body. */
pub(crate) fn owner_call(
    api: &str,
    token: &str,
    action: &'static str,
    method: &str,
    path: &str,
    target: Option<&str>,
    body: Option<serde_json::Value>,
) -> Result<String, Error> {
    let mut url = format!("{api}{path}");
    if let Some(target) = target {
        url.push_str(&format!("?target={target}"));
    }
    let request = http::manual_redirect_agent()
        .request(method, &url)
        .set("User-Agent", http::USER_AGENT)
        .set("Authorization", &format!("Bearer {token}"));
    let response = match body {
        Some(json) => request.send_json(json),
        None => request.call(),
    };
    owner_answer(action, response)
}

/** PUTs `bytes` at `path` as an owner, with their SHA-256 in
`x-ember-sha256` so the API can refuse a body that changed on the way. the
bulk agent, since a tool asset may be 100 MB on a slow uplink. */
pub(crate) fn owner_upload(
    api: &str,
    token: &str,
    action: &'static str,
    path: &str,
    bytes: &[u8],
) -> Result<(), Error> {
    let response = http::manual_redirect_agent()
        .put(&format!("{api}{path}"))
        .set("User-Agent", http::USER_AGENT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/octet-stream")
        .set("x-ember-sha256", &crate::sys::hash::sha256_hex(bytes))
        .send_bytes(bytes);
    owner_answer(action, response).map(drop)
}

/// the body of an owner's call, or its refusal with the API's message.
fn owner_answer(
    action: &'static str,
    response: Result<ureq::Response, ureq::Error>,
) -> Result<String, Error> {
    match http::no_redirect(response) {
        Ok(response) => Ok(response.into_string()?),
        Err(ureq::Error::Status(status, response)) => Err(Error::OwnerActionFailed {
            action,
            status,
            message: error_message(status, response.into_string().ok().as_deref()),
        }),
        Err(other) => Err(HttpError::from(other).into()),
    }
}

fn error_message(status: u16, body: Option<&str>) -> String {
    if (300..400).contains(&status) {
        return format!(
            "the registry answered HTTP {status}, a redirect, and embr sends no token and no upload to another address; check {API_VAR}"
        );
    }
    body.and_then(|body| serde_json::from_str::<ErrorBody>(body).ok())
        .map(|body| body.error)
        .unwrap_or_else(|| format!("the registry answered HTTP {status}"))
}

/** `url` without the `user:password@` of its host. EMBER_API_URL can hold
one, and embr prints the URL in `embr doctor` and in a publish. */
pub fn without_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

/** whether embr may show `uri` as the page of a login and open it: an
https address on github.com, on the host of the API `api`, or on the site
that the API host is `api.` of. the answer of `POST /v1/cli/login` is text
from the network, and the system opener starts a program for a `file:` or
a custom scheme. an API on plain http, a local backend, may name its own
host. */
pub fn is_login_page(uri: &str, api: &str) -> bool {
    let host_of = |url: &str| {
        origin(url).and_then(|origin| {
            let (scheme, authority) = origin.split_once("://")?;
            let host = authority.rsplit('@').next()?;
            let host = host.split(':').next()?.to_string();
            Some((scheme.to_string(), host))
        })
    };
    let (Some((scheme, host)), Some((api_scheme, api_host))) = (host_of(uri), host_of(api)) else {
        return false;
    };
    if uri.chars().any(|c| c.is_control() || c.is_whitespace()) || host.is_empty() {
        return false;
    }
    let site = api_host.strip_prefix("api.").unwrap_or(&api_host);
    let ours = host == api_host || host == site;
    match scheme.as_str() {
        "https" => ours || host == "github.com",
        "http" => ours && api_scheme == "http",
        _ => false,
    }
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

    /** a write that the registry answers with a redirect is no success, and
    the address of the redirect gets no request: no token, no OIDC token
    and no upload. ureq follows a 302 as a GET with no body, and keeps a
    custom header on the way. */
    #[test]
    fn a_redirected_write_fails_and_sends_nothing_on() {
        for status in [
            "301 Moved Permanently",
            "302 Found",
            "303 See Other",
            "307 Temporary Redirect",
            "308 Permanent Redirect",
        ] {
            let (other, other_log) = mock::serve(|_| mock::reply("200 OK", "{}"));
            let redirect = format!(
                "HTTP/1.1 {status}\r\nLocation: {other}/collect\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let (api, api_log) = mock::serve(move |_| redirect.clone());

            let published = publish_at(&api, None, Some("oidc-secret"), b"archive", None);
            assert!(
                matches!(&published, Err(Error::PublishFailed { message, .. }) if message.contains("redirect")),
                "{status}: {published:?}"
            );
            let with_token = publish_at(
                &api,
                Some("ember_abc"),
                Some("oidc-secret"),
                b"archive",
                None,
            );
            assert!(with_token.is_err(), "{status}");
            let called = owner_call(&api, "ember_abc", "yank", "PUT", "/v1/x", None, None);
            assert!(
                matches!(&called, Err(Error::OwnerActionFailed { message, .. }) if message.contains("redirect")),
                "{status}: {called:?}"
            );
            let uploaded = owner_upload(&api, "ember_abc", "upload", "/v1/x", b"bytes");
            assert!(uploaded.is_err(), "{status}");

            assert_eq!(api_log.lock().unwrap().len(), 4, "{status}");
            assert!(api_log.lock().unwrap()[0].contains("x-github-oidc-token: oidc-secret"));
            let followed = other_log.lock().unwrap();
            assert!(followed.is_empty(), "{status}: {followed:?}");
        }
    }

    #[test]
    fn the_api_url_shows_without_its_password() {
        for (url, shown) in [
            ("https://api.emberpm.dev", "https://api.emberpm.dev"),
            (
                "https://user:secret@host.example/v1",
                "https://host.example/v1",
            ),
            ("http://token@127.0.0.1:8787", "http://127.0.0.1:8787"),
            ("https://a:b@c@host.example", "https://host.example"),
            ("https://host.example/path@x", "https://host.example/path@x"),
            ("not a url", "not a url"),
        ] {
            assert_eq!(without_userinfo(url), shown);
        }
    }

    #[test]
    fn a_login_page_is_https_on_the_registry_or_on_github() {
        let api = "https://api.emberpm.dev";
        for page in [
            "https://emberpm.dev/cli/connect?code=ABCD-EFGH",
            "https://api.emberpm.dev/cli",
            "https://github.com/login/device",
            "HTTPS://GitHub.com/login/device",
        ] {
            assert!(is_login_page(page, api), "{page}");
        }
        for page in [
            "http://emberpm.dev/cli/connect",
            "https://evil.example/cli/connect",
            "https://emberpm.dev.evil.example/",
            "https://github.com@evil.example/",
            "https://evil.example/https://github.com/",
            "file:///home/user/.ssh/id_rsa",
            "javascript:alert(1)",
            "vscode://extension/run?x",
            "https://emberpm.dev/\u{1b}[2K",
            "https://emberpm.dev/ --flag",
            "emberpm.dev/cli",
            "",
        ] {
            assert!(!is_login_page(page, api), "{page}");
        }
        // a local backend on plain http names its own host
        assert!(is_login_page(
            "http://127.0.0.1:8787/cli",
            "http://127.0.0.1:8787"
        ));
        assert!(!is_login_page(
            "http://127.0.0.1:9999/cli",
            "https://api.emberpm.dev"
        ));
        assert!(!is_login_page(
            "http://evil.example/cli",
            "http://127.0.0.1:8787"
        ));
    }

    #[test]
    fn link_keys_have_one_shape() {
        assert!(is_link_key("0123456789abcdef0123456789abcdef"));
        assert!(!is_link_key("0123456789abcdef"));
        assert!(!is_link_key("0123456789abcdef0123456789abcdeg"));
        assert!(!is_link_key("0123456789abcdef0123456789abcde\n"));
    }

    /** with no Ember token the OIDC token is the only credential, and a
    refusal carries the registry's own message */
    #[test]
    fn a_trusted_publish_sends_the_oidc_token_and_no_authorization() {
        let (api, log) = mock::serve(|request| {
            if request.contains("authorization:") {
                mock::reply("201 Created", "{}")
            } else {
                mock::reply(
                    "403 Forbidden",
                    r#"{"error":"no trusted publisher of acme/rocket matches repository acme/fork"}"#,
                )
            }
        });

        let refused = publish_at(
            &api,
            None,
            Some("header.claims.signature"),
            b"archive",
            None,
        );
        let Err(Error::PublishFailed { status, message }) = refused else {
            panic!("a refusal is a publish failure: {refused:?}");
        };
        assert_eq!(status, 403);
        assert_eq!(
            message,
            "no trusted publisher of acme/rocket matches repository acme/fork"
        );
        let sent = log.lock().unwrap()[0].clone();
        assert!(sent.starts_with("post /v1/publish "), "{sent}");
        assert!(
            sent.contains("x-github-oidc-token: header.claims.signature"),
            "{sent}"
        );
        assert!(!sent.contains("authorization"), "{sent}");

        // next to a token, the token is the credential, as before
        publish_at(
            &api,
            Some("ember_abc"),
            Some("oidc"),
            b"archive",
            Some("beta"),
        )
        .unwrap();
        let sent = log.lock().unwrap()[1].clone();
        assert!(sent.contains("authorization: bearer ember_abc"), "{sent}");
        assert!(sent.contains("x-github-oidc-token: oidc"), "{sent}");
    }

    #[test]
    fn a_notice_posts_its_level_message_and_range_and_gives_back_the_id() {
        let (api, log) = mock::serve(|request| {
            if request.contains("too long") {
                mock::reply(
                    "400 Bad Request",
                    r#"{"error":"message must be 1 to 500 characters"}"#,
                )
            } else {
                mock::reply(
                    "201 Created",
                    r#"{"notice":{"id":"3f9c0a1b2c4d"},"notices":[]}"#,
                )
            }
        });
        let id = post_notice_at(
            &api,
            "ember_abc",
            "acme/rocket",
            "warning",
            "leaks",
            Some("^1"),
        )
        .unwrap();
        assert_eq!(id, "3f9c0a1b2c4d");
        let sent = log.lock().unwrap()[0].clone();
        assert!(
            sent.starts_with("post /v1/packages/acme/rocket/notices "),
            "{sent}"
        );
        assert!(sent.contains("authorization: bearer ember_abc"), "{sent}");
        assert!(
            sent.contains(r#"{"level":"warning","message":"leaks","versions":"^1"}"#),
            "{sent}"
        );

        let refused = post_notice_at(&api, "ember_abc", "acme/rocket", "notice", "too long", None);
        let Err(Error::OwnerActionFailed {
            status, message, ..
        }) = refused
        else {
            panic!("a refusal is an owner action failure: {refused:?}");
        };
        assert_eq!(
            (status, message.as_str()),
            (400, "message must be 1 to 500 characters")
        );
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
