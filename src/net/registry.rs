/*!
lpm's own registry API (api.luaupm.com): the write side of publishing.

Reads never come here. Resolution and installs go through the git index
clone and the CDN URLs baked into its entries, so they work even when the
API is down. The one thing the CLI needs the API for is `POST /v1/publish`,
which validates the tarball, stores it in the CDN bucket, and writes the
index entry (the API is the index repo's only writer).
*/

use crate::error::Error;
use crate::net::http;
use crate::net::http::error::HttpError;
use serde::Deserialize;

pub const API_URL: &str = "https://api.luaupm.com";

/** The API's hard cap on publish bodies (413 above it); checked client-side
so an oversized archive fails before uploading. */
pub const MAX_ARCHIVE_BYTES: usize = 10 * 1024 * 1024;

/// Every API error is `{"error": "<human readable message>"}`.
#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

/** Uploads a packed `.tar.gz` to `POST /v1/publish` as the given user.
Statuses the API answers with: 401 bad token, 400 malformed tarball or
lpm.toml, 403 scope owned by someone else (first publish claims a scope),
409 version already exists, 413 over the size cap. */
pub fn publish(token: &str, archive: &[u8]) -> Result<(), Error> {
    let url = format!("{API_URL}/v1/publish");
    let response = ureq::post(&url)
        .set("User-Agent", http::USER_AGENT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/gzip")
        .send_bytes(archive);

    match response {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(status, response)) => Err(Error::PublishFailed {
            status,
            message: error_message(status, response.into_string().ok().as_deref()),
        }),
        // Transport errors (DNS, TLS, ...): no response body to read.
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
        // Non-JSON bodies (a proxy error page, say) fall back to the status.
        assert_eq!(
            error_message(502, Some("<html>Bad Gateway</html>")),
            "the registry answered HTTP 502"
        );
        assert_eq!(error_message(500, None), "the registry answered HTTP 500");
    }
}
