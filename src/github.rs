const BASE_URL: &str = "https://api.github.com";
const JSON_HEADER_TYPE: &str = "application/vnd.github.v3+json";

// TODO:
// Add logic for checking tools on github via author/name
// Implement a function to install package release versions and return them so the tool command can link and do whatever else
// Implement version checking and updating functionality

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

    // pub fn get_release(&self, repo: &str, version: Version) -> Result<responses::Release, Error> {
    //     let url = format!("{BASE_URL}/repos/{repo}/release/");

    //     http::get_json::<responses::Release>(&url, &self.headers()).map_err(|error| match error {
    //         HttpError::NotFound => Error::NoSuchRelease(repo.to_string(), "".to_string()),
    //         other => other.into(),
    //     })
    // }

    pub fn get_latest_release(&self, repo: &str) -> Result<responses::Release, Error> {
        let url = format!("{BASE_URL}/repos/{repo}/releases/latest");

        http::get_json::<responses::Release>(&url, &self.headers()).map_err(|error| match error {
            HttpError::NotFound => Error::NoReleases(repo.to_string()),
            other => other.into(),
        })
    }
}
