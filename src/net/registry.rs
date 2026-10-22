/*!
embr's own registry API at api.luaupm.com, the write side of publishing.

Reads never come here. Resolution and installs go through the git index
clone and the CDN URLs baked into its entries, so they work even when the
API is down. The one thing the CLI needs the API for is `POST /v1/publish`,
which validates the tarball, stores it in the CDN bucket, and writes the
index entry. The API is the index repo's only writer.
*/

use crate::error::Error;
use crate::net::http;
use crate::net::http::error::HttpError;
use crate::project::manifest::Environment;
use serde::Deserialize;

pub const API_URL: &str = "https://api.luaupm.com";

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
header, auth stays the GitHub token. */
pub fn publish(token: &str, oidc_token: Option<&str>, archive: &[u8]) -> Result<(), Error> {
    let url = format!("{API_URL}/v1/publish");
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
`PUT`/`DELETE /v1/packages/{scope}/{name}/{version}/yank`. without an
`environment` every target of the version changes. owners only, the same
auth as publish. both calls are idempotent. */
pub fn yank(
    token: &str,
    name: &str,
    version: &semver::Version,
    environment: Option<Environment>,
    undo: bool,
) -> Result<(), Error> {
    let path = format!("/v1/packages/{name}/{version}/yank");
    owner_call(token, "yank", &path, environment, (!undo).then_some(None))
}

/** sets or clears a deprecation through `PUT`/`DELETE` on
`/v1/packages/{scope}/{name}/deprecate`, or on `.../{version}/deprecate` for
one version. `reason` None clears it. the package's reason is pesde's
`[meta] deprecated`, a version's is `deprecated` on its entries. */
pub fn deprecate(
    token: &str,
    name: &str,
    version: Option<&semver::Version>,
    environment: Option<Environment>,
    reason: Option<&str>,
) -> Result<(), Error> {
    let path = match version {
        Some(version) => format!("/v1/packages/{name}/{version}/deprecate"),
        None => format!("/v1/packages/{name}/deprecate"),
    };
    let body = reason.map(|reason| Some(serde_json::json!({ "reason": reason })));
    owner_call(token, "deprecation", &path, environment, body)
}

/** one call an owner makes. `body` None sends a DELETE, Some a PUT with
that JSON, or with no body at all. */
fn owner_call(
    token: &str,
    action: &'static str,
    path: &str,
    environment: Option<Environment>,
    body: Option<Option<serde_json::Value>>,
) -> Result<(), Error> {
    let mut url = format!("{API_URL}{path}");
    if let Some(environment) = environment {
        url.push_str(&format!("?target={environment}"));
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
