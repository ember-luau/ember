/*!
publish provenance: inside a GitHub Actions job that can mint OIDC tokens,
grab one and send it along with the publish so the registry can verify
"built from repo X at commit Y by workflow Z". local publishes simply have
no token, which is normal, not suspicious. best effort all the way down:
nothing here may fail or noticeably delay a publish.
*/

use crate::net::http;
use serde::Deserialize;
use std::time::Duration;

/// the audience the registry verifies; anything else gets rejected server side
const AUDIENCE: &str = "luaupm.com";

/// header the publish request carries the token in
pub const HEADER: &str = "X-GitHub-OIDC-Token";

/* the token endpoint answers fast when it answers at all; don't let a
wedged runner hold the publish hostage for the agent's usual 60s */
const TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

/// what the Actions token endpoint answers with
#[derive(Deserialize)]
struct TokenResponse {
    value: String,
}

/** the workflow's token endpoint, present only when the job has
`permissions: id-token: write` (that's what puts the request url + bearer
in the env). detecting Actions itself keeps us from ever calling out on a
developer machine that happens to have the vars set. */
struct TokenEndpoint {
    url: String,
    bearer: String,
}

impl TokenEndpoint {
    fn from_env() -> Option<Self> {
        Self::from_vars(
            std::env::var("GITHUB_ACTIONS").ok(),
            std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok(),
            std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok(),
        )
    }

    fn from_vars(
        actions: Option<String>,
        url: Option<String>,
        bearer: Option<String>,
    ) -> Option<Self> {
        if actions.as_deref() != Some("true") {
            return None;
        }
        match (url, bearer) {
            (Some(url), Some(bearer)) if !url.is_empty() && !bearer.is_empty() => {
                Some(TokenEndpoint { url, bearer })
            }
            _ => None,
        }
    }

    /// the request url already carries a query string, so audience appends with &
    fn request_url(&self) -> String {
        format!("{}&audience={AUDIENCE}", self.url)
    }
}

/** an OIDC token for this publish, or None when we're not in Actions or
anything at all goes wrong (missing permission, non-200, timeout, bad
JSON). None means "publish without provenance", never an error. */
pub fn actions_token() -> Option<String> {
    let endpoint = TokenEndpoint::from_env()?;
    let response = http::agent()
        .get(&endpoint.request_url())
        .set("User-Agent", http::USER_AGENT)
        .set("Authorization", &format!("Bearer {}", endpoint.bearer))
        .timeout(TOKEN_TIMEOUT)
        .call()
        .ok()?;
    let token: TokenResponse = response.into_json().ok()?;
    (!token.value.is_empty()).then_some(token.value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    #[test]
    fn endpoint_needs_all_three_vars() {
        assert!(TokenEndpoint::from_vars(some("true"), some("https://x?a=1"), some("t")).is_some());

        // not in Actions at all, or in Actions without id-token: write
        assert!(TokenEndpoint::from_vars(None, some("https://x"), some("t")).is_none());
        assert!(TokenEndpoint::from_vars(some("1"), some("https://x"), some("t")).is_none());
        assert!(TokenEndpoint::from_vars(some("true"), None, some("t")).is_none());
        assert!(TokenEndpoint::from_vars(some("true"), some("https://x"), None).is_none());
        assert!(TokenEndpoint::from_vars(some("true"), some(""), some("t")).is_none());
    }

    #[test]
    fn audience_is_appended_to_the_existing_query() {
        let endpoint = TokenEndpoint::from_vars(
            some("true"),
            some("https://host/token?api-version=2.0"),
            some("t"),
        )
        .unwrap();
        assert_eq!(
            endpoint.request_url(),
            "https://host/token?api-version=2.0&audience=luaupm.com"
        );
    }
}
