const BASE_URL: &str = "https://api.github.com";
const JSON_HEADER_TYPE: &str = "application/vnd.github.v3+json";

use crate::error::Error;
use crate::http;
use crate::http::error::HttpError;
use http::responses;

pub struct GithubAPI {
    /// Pre-formatted `Bearer <token>` value, present when GITHUB_TOKEN is set.
    auth_header: Option<String>,
}

impl GithubAPI {
    pub fn new() -> Self {
        let auth_header = std::env::var("GITHUB_TOKEN")
            .ok()
            .filter(|token| !token.trim().is_empty())
            .map(|token| format!("Bearer {token}"));

        Self { auth_header }
    }

    fn headers(&self) -> Vec<(&str, &str)> {
        let mut headers = vec![
            ("User-Agent", http::USER_AGENT),
            ("Accept", JSON_HEADER_TYPE),
        ];

        if let Some(auth) = &self.auth_header {
            headers.push(("Authorization", auth));
        }

        headers
    }

    /// Fetches the release tagged `v{version}`, falling back to the bare
    /// `{version}` tag for repos that don't prefix their tags.
    /// `version` must not include a leading 'v'.
    pub fn get_release(&self, repo: &str, version: &str) -> Result<responses::Release, Error> {
        for tag in [format!("v{version}"), version.to_string()] {
            let url = format!("{BASE_URL}/repos/{repo}/releases/tags/{tag}");

            match http::get_json::<responses::Release>(&url, &self.headers()) {
                Ok(release) => return Ok(release),
                Err(HttpError::NotFound) => continue,
                Err(other) => return Err(other.into()),
            }
        }

        Err(Error::NoSuchRelease(repo.to_string(), version.to_string()))
    }

    pub fn get_latest_release(&self, repo: &str) -> Result<responses::Release, Error> {
        let url = format!("{BASE_URL}/repos/{repo}/releases/latest");

        http::get_json::<responses::Release>(&url, &self.headers()).map_err(|error| match error {
            HttpError::NotFound => Error::NoReleases(repo.to_string()),
            other => other.into(),
        })
    }
}

impl Default for GithubAPI {
    fn default() -> Self {
        Self::new()
    }
}
