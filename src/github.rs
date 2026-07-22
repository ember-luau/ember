const BASE_URL: &str = "https://api.github.com";
const UPLOADS_URL: &str = "https://uploads.github.com";
const JSON_HEADER_TYPE: &str = "application/vnd.github.v3+json";

use crate::error::Error;
use crate::http;
use crate::http::error::HttpError;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use http::responses;
use serde_json::json;

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

    /// Like `new`, but authenticated with this token; GITHUB_TOKEN is ignored.
    pub fn with_token(token: &str) -> Self {
        Self {
            auth_header: Some(format!("Bearer {token}")),
        }
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

    pub fn get_user(&self) -> Result<responses::User, Error> {
        let url = format!("{BASE_URL}/user");
        Ok(http::get_json(&url, &self.headers())?)
    }

    pub fn get_repo(&self, repo: &str) -> Result<responses::Repo, Error> {
        let url = format!("{BASE_URL}/repos/{repo}");
        Ok(http::get_json(&url, &self.headers())?)
    }

    /// Looks up a release by its exact tag (no `v` fallback, unlike
    /// `get_release`). A missing release is not an error here: publish uses
    /// this to decide whether to create one.
    pub fn release_by_tag(
        &self,
        repo: &str,
        tag: &str,
    ) -> Result<Option<responses::Release>, Error> {
        let url = format!("{BASE_URL}/repos/{repo}/releases/tags/{tag}");

        match http::get_json::<responses::Release>(&url, &self.headers()) {
            Ok(release) => Ok(Some(release)),
            Err(HttpError::NotFound) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    pub fn create_release(&self, repo: &str, tag: &str) -> Result<responses::Release, Error> {
        let url = format!("{BASE_URL}/repos/{repo}/releases");
        let body = json!({ "tag_name": tag, "name": tag });

        Ok(http::post_json(&url, &self.headers(), &body)?)
    }

    pub fn upload_release_asset(
        &self,
        repo: &str,
        release_id: u64,
        file_name: &str,
        bytes: &[u8],
    ) -> Result<responses::Asset, Error> {
        // Asset uploads go to a separate host, with the name as a query param.
        let url = format!(
            "{UPLOADS_URL}/repos/{repo}/releases/{release_id}/assets?name={}",
            urlencode(file_name)
        );

        Ok(http::post_bytes(
            &url,
            &self.headers(),
            "application/gzip",
            bytes,
        )?)
    }

    /// Forks `repo` into the authenticated user's account. GitHub forks
    /// asynchronously (202 with a repo that may not exist yet), so this polls
    /// until the fork actually answers. An already-existing fork is returned
    /// immediately; that's how re-publishing reuses it.
    pub fn create_fork(&self, repo: &str) -> Result<responses::Repo, Error> {
        let url = format!("{BASE_URL}/repos/{repo}/forks");
        let fork: responses::Repo = http::post_json(&url, &self.headers(), &json!({}))?;

        for _ in 0..10 {
            match self.get_repo(&fork.full_name) {
                Ok(repo) => return Ok(repo),
                Err(Error::Http(HttpError::NotFound)) => {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
                Err(other) => return Err(other),
            }
        }

        Err(HttpError::NotFound.into())
    }

    pub fn branch_sha(&self, repo: &str, branch: &str) -> Result<String, Error> {
        let url = format!("{BASE_URL}/repos/{repo}/git/ref/heads/{branch}");
        let git_ref: responses::GitRef = http::get_json(&url, &self.headers())?;

        Ok(git_ref.object.sha)
    }

    pub fn create_branch(&self, repo: &str, branch: &str, sha: &str) -> Result<(), Error> {
        let url = format!("{BASE_URL}/repos/{repo}/git/refs");
        let body = json!({ "ref": format!("refs/heads/{branch}"), "sha": sha });

        http::post_json::<serde_json::Value>(&url, &self.headers(), &body)?;
        Ok(())
    }

    /// Fetches file metadata (we only need the blob sha for updates).
    /// `path` may contain '/' and is passed through unencoded.
    pub fn get_file(
        &self,
        repo: &str,
        path: &str,
        reference: Option<&str>,
    ) -> Result<Option<responses::ContentFile>, Error> {
        let mut url = format!("{BASE_URL}/repos/{repo}/contents/{path}");
        if let Some(reference) = reference {
            url.push_str("?ref=");
            url.push_str(reference);
        }

        match http::get_json::<responses::ContentFile>(&url, &self.headers()) {
            Ok(file) => Ok(Some(file)),
            Err(HttpError::NotFound) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    /// Creates or updates a file on `branch`. Pass the current blob sha
    /// (from `get_file`) when the file already exists, or GitHub rejects
    /// the write with a 422.
    pub fn put_file(
        &self,
        repo: &str,
        path: &str,
        branch: &str,
        message: &str,
        content: &[u8],
        previous_sha: Option<&str>,
    ) -> Result<(), Error> {
        let url = format!("{BASE_URL}/repos/{repo}/contents/{path}");
        let mut body = json!({
            "message": message,
            "content": BASE64.encode(content),
            "branch": branch,
        });
        if let Some(sha) = previous_sha {
            body["sha"] = json!(sha);
        }

        http::put_json::<serde_json::Value>(&url, &self.headers(), &body)?;
        Ok(())
    }

    /// `head` is `owner:branch` when the branch lives on a fork.
    pub fn create_pull(
        &self,
        base_repo: &str,
        base_branch: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<responses::PullRequest, Error> {
        let url = format!("{BASE_URL}/repos/{base_repo}/pulls");
        let payload = json!({
            "title": title,
            "head": head,
            "base": base_branch,
            "body": body,
        });

        Ok(http::post_json(&url, &self.headers(), &payload)?)
    }
}

impl Default for GithubAPI {
    fn default() -> Self {
        Self::new()
    }
}

/// Just enough percent-encoding for a query value; asset file names are the
/// only caller and stay ASCII in practice.
fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            ' ' | '#' | '?' | '%' | '&' | '+' => {
                encoded.push_str(&format!("%{:02X}", c as u32));
            }
            _ => encoded.push(c),
        }
    }

    encoded
}
