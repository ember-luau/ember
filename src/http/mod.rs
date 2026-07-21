use crate::http::error::HttpError;
use serde::de::DeserializeOwned;
use std::io::Read;

pub mod error;
pub mod responses;
pub const USER_AGENT: &str = concat!("lpm/", env!("CARGO_PKG_VERSION"));

pub fn get_json<T: DeserializeOwned>(url: &str, headers: &[(&str, &str)]) -> Result<T, HttpError> {
    let mut request = ureq::get(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }

    let response = request.call()?;
    Ok(response.into_json::<T>()?)
}

/// GETs `url`, following redirects (ureq only auto-follows 301/302/303;
/// some hosts, like pesde's registry or GitHub's asset redirects, use 307).
pub fn get_bytes(url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
    let mut url = url.to_string();
    for _ in 0..5 {
        let mut request = ureq::get(&url).set("User-Agent", USER_AGENT);
        for (name, value) in headers {
            request = request.set(name, value);
        }

        let response = request.call()?;
        if (300..400).contains(&response.status()) {
            match response.header("location") {
                Some(location) => {
                    url = location.to_string();
                    continue;
                }
                None => break,
            }
        }

        let mut bytes = Vec::new();
        response.into_reader().read_to_end(&mut bytes)?;
        return Ok(bytes);
    }

    Err(HttpError::TooManyRedirects { url })
}
