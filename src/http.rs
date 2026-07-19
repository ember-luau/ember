use crate::error::Error;
use std::io::Read;

const USER_AGENT: &str = concat!("lpm/", env!("CARGO_PKG_VERSION"));

/// GETs `url`, following redirects (ureq only auto-follows 301/302/303;
/// some hosts, like pesde's registry or GitHub's asset redirects, use 307).
pub fn get_bytes(url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, Error> {
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

    Err(Error::IndexFetch {
        url,
        reason: "too many redirects".to_string(),
    })
}