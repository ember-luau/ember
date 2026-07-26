use crate::net::http::error::HttpError;
use serde::de::DeserializeOwned;
use std::io::Read;

pub mod error;
pub mod responses;

pub const USER_AGENT: &str = concat!("lpm/", env!("CARGO_PKG_VERSION"));

/// Sets our User-Agent first so callers can still override it.
fn with_headers(mut request: ureq::Request, headers: &[(&str, &str)]) -> ureq::Request {
    request = request.set("User-Agent", USER_AGENT);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    request
}

/** Some GitHub endpoints answer 201/204 with an empty body, which serde
won't parse; treat that as JSON `null` so `T = serde_json::Value` (or an
Option) still deserializes. */
fn parse_json<T: DeserializeOwned>(response: ureq::Response) -> Result<T, HttpError> {
    let body = response.into_string()?;
    if body.trim().is_empty() {
        return Ok(serde_json::from_str("null")?);
    }

    Ok(serde_json::from_str(&body)?)
}

pub fn get_json<T: DeserializeOwned>(url: &str, headers: &[(&str, &str)]) -> Result<T, HttpError> {
    let response = with_headers(ureq::get(url), headers).call()?;
    Ok(response.into_json::<T>()?)
}

pub fn post_form<T: DeserializeOwned>(
    url: &str,
    headers: &[(&str, &str)],
    form: &[(&str, &str)],
) -> Result<T, HttpError> {
    let response = with_headers(ureq::post(url), headers).send_form(form)?;
    parse_json(response)
}

/** GETs `url`, following redirects by hand: ureq 2 only auto-follows
301/302/303, and some hosts (pesde's registry, GitHub asset redirects)
answer 307. */
pub fn get_bytes(url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
    let mut url = url.to_string();
    for _ in 0..5 {
        let response = with_headers(ureq::get(&url), headers).call()?;
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
