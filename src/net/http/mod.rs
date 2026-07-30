use crate::net::http::error::HttpError;
use serde::de::DeserializeOwned;
use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

pub mod error;
pub mod responses;

pub const USER_AGENT: &str = concat!("lpm/", env!("CARGO_PKG_VERSION"));

/** the client for small exchanges like index metadata, auth, and JSON
APIs. an agent pools connections, bare `ureq::get` opens a fresh TCP+TLS
connection per request, and carries timeouts, without them a wedged
registry hangs an install forever. the 60s overall cap is right for
requests whose responses are small, bulk transfers go through
[`bulk_agent`]. the idle pool is sized for the installer's worker count,
ureq's default keeps a single idle connection per host, which would make
concurrent fetches from one registry all pay their own handshake. */
pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .max_idle_connections_per_host(8)
            .build()
    })
}

/** the client for archives and release binaries. stall-bounded, 30s with
no bytes moving fails the request, but with no overall deadline, so a
20 MB tool download on a slow connection takes as long as it takes instead
of dying at the minute mark. same pool as `agent`, different limits. */
pub fn bulk_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .timeout_write(Duration::from_secs(30))
            .max_idle_connections_per_host(8)
            .build()
    })
}

/// Sets our User-Agent first so callers can still override it.
fn with_headers(mut request: ureq::Request, headers: &[(&str, &str)]) -> ureq::Request {
    request = request.set("User-Agent", USER_AGENT);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    request
}

/** Some GitHub endpoints answer 201/204 with an empty body, which serde
won't parse. Treat that as JSON `null` so `T = serde_json::Value` or an
Option still deserializes. */
fn parse_json<T: DeserializeOwned>(response: ureq::Response) -> Result<T, HttpError> {
    let body = response.into_string()?;
    if body.trim().is_empty() {
        return Ok(serde_json::from_str("null")?);
    }

    Ok(serde_json::from_str(&body)?)
}

pub fn get_json<T: DeserializeOwned>(url: &str, headers: &[(&str, &str)]) -> Result<T, HttpError> {
    let response = with_headers(agent().get(url), headers).call()?;
    Ok(response.into_json::<T>()?)
}

pub fn post_form<T: DeserializeOwned>(
    url: &str,
    headers: &[(&str, &str)],
    form: &[(&str, &str)],
) -> Result<T, HttpError> {
    let response = with_headers(agent().post(url), headers).send_form(form)?;
    parse_json(response)
}

/** GETs `url`, following redirects by hand. ureq 2 only auto-follows
301/302/303, and some hosts like pesde's registry and GitHub asset
redirects answer 307. */
pub fn get_bytes(url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
    let mut url = url.to_string();
    for _ in 0..5 {
        let response = with_headers(bulk_agent().get(&url), headers).call()?;
        if (300..400).contains(&response.status()) {
            match response.header("location") {
                Some(location) => {
                    url = location.to_string();
                    continue;
                }
                None => return Err(HttpError::RedirectMissingLocation { url }),
            }
        }

        let mut bytes = Vec::new();
        response.into_reader().read_to_end(&mut bytes)?;
        return Ok(bytes);
    }

    Err(HttpError::TooManyRedirects { url })
}
