//! Shapes of the GitHub responses lpm deserializes.
//!
//! TODO(api): the fork/branch/file/pull shapes below are only still here
//! because github.rs is (see its note); they were the publish flow's.
#![allow(dead_code)]

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
    // Deserialized for future use (e.g. publishing); nothing reads them yet.
    pub url: String,
    pub id: u64,
}

#[derive(Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    /// Minimum seconds between polls; going faster earns a `slow_down` error.
    pub interval: u64,
}

/// Device-flow poll result. GitHub reports "not yet" states
/// (`authorization_pending`, `slow_down`, ...) as HTTP 200 with an `error`
/// field rather than an error status.
#[derive(Deserialize)]
pub struct AccessToken {
    pub access_token: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct User {
    pub login: String,
    // Deserialized for future use (stable identity across renames).
    pub id: u64,
}

#[derive(Deserialize)]
pub struct Repo {
    pub full_name: String,
    pub default_branch: String,
}

#[derive(Deserialize)]
pub struct GitRef {
    pub object: GitRefObject,
}

#[derive(Deserialize)]
pub struct GitRefObject {
    pub sha: String,
}

#[derive(Deserialize)]
pub struct ContentFile {
    pub sha: String,
}

#[derive(Deserialize)]
pub struct PullRequest {
    pub html_url: String,
    pub number: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_github_release() {
        let json = r#"{
            "url": "https://api.github.com/repos/luaupm/cli/releases/213371337213",
            "html_url": "https://github.com/luaupm/cli/releases/tag/v0.1.0",
            "id": 213371337213,
            "node_id": "RE_kwDOLxAmM84MK9bd",
            "tag_name": "v0.1.0",
            "target_commitish": "master",
            "name": "v0.1.0",
            "draft": false,
            "prerelease": false,
            "created_at": "2026-07-20T12:00:00Z",
            "published_at": "2026-07-20T12:05:00Z",
            "body": "Release notes here",
            "assets": [
                {
                    "url": "https://api.github.com/repos/luaupm/cli/releases/assets/1",
                    "id": 1,
                    "name": "lpm-windows-x86_64.exe",
                    "content_type": "application/octet-stream",
                    "size": 4200000,
                    "browser_download_url": "https://github.com/luaupm/cli/releases/download/v0.1.0/lpm-windows-x86_64.exe"
                },
                {
                    "name": "lpm-linux-x86_64",
                    "browser_download_url": "https://github.com/luaupm/cli/releases/download/v0.1.0/lpm-linux-x86_64"
                }
            ]
        }"#;

        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v0.1.0");
        assert_eq!(
            release.url,
            "https://api.github.com/repos/luaupm/cli/releases/213371337213"
        );
        assert_eq!(release.id, 213371337213); // larger than i32::MAX
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "lpm-windows-x86_64.exe");
        assert_eq!(
            release.assets[0].browser_download_url,
            "https://github.com/luaupm/cli/releases/download/v0.1.0/lpm-windows-x86_64.exe"
        );
        assert_eq!(release.assets[1].name, "lpm-linux-x86_64");
    }

    #[test]
    fn deserializes_device_code() {
        let json = r#"{
            "device_code": "3584d83530557fdd1f46af8289938c8ef79f9dc5",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 899,
            "interval": 5
        }"#;

        let code: DeviceCode = serde_json::from_str(json).unwrap();
        assert_eq!(code.device_code, "3584d83530557fdd1f46af8289938c8ef79f9dc5");
        assert_eq!(code.user_code, "WDJB-MJHT");
        assert_eq!(code.verification_uri, "https://github.com/login/device");
        assert_eq!(code.expires_in, 899);
        assert_eq!(code.interval, 5);
    }

    #[test]
    fn deserializes_access_token_pending() {
        let json = r#"{
            "error": "authorization_pending",
            "error_description": "The authorization request is still pending.",
            "error_uri": "https://docs.github.com/developers/apps/authorizing-oauth-apps#error-codes-for-the-device-flow"
        }"#;

        let token: AccessToken = serde_json::from_str(json).unwrap();
        assert!(token.access_token.is_none());
        assert_eq!(token.error.as_deref(), Some("authorization_pending"));
    }

    #[test]
    fn deserializes_access_token_granted() {
        let json = r#"{
            "access_token": "gho_16C7e42F292c6912E7710c838347Ae178B4a",
            "token_type": "bearer",
            "scope": "repo"
        }"#;

        let token: AccessToken = serde_json::from_str(json).unwrap();
        assert_eq!(
            token.access_token.as_deref(),
            Some("gho_16C7e42F292c6912E7710c838347Ae178B4a")
        );
        assert!(token.error.is_none());
    }

    #[test]
    fn deserializes_user() {
        let json = r#"{
            "login": "octocat",
            "id": 583231,
            "node_id": "MDQ6VXNlcjU4MzIzMQ==",
            "avatar_url": "https://avatars.githubusercontent.com/u/583231?v=4",
            "html_url": "https://github.com/octocat",
            "type": "User",
            "site_admin": false
        }"#;

        let user: User = serde_json::from_str(json).unwrap();
        assert_eq!(user.login, "octocat");
        assert_eq!(user.id, 583231);
    }

    #[test]
    fn deserializes_git_ref() {
        let json = r#"{
            "ref": "refs/heads/main",
            "node_id": "MDM6UmVmcmVmcy9oZWFkcy9tYWlu",
            "url": "https://api.github.com/repos/octocat/Hello-World/git/refs/heads/main",
            "object": {
                "type": "commit",
                "sha": "aa218f56b14c9653891f9e74264a383fa43fefbd",
                "url": "https://api.github.com/repos/octocat/Hello-World/git/commits/aa218f56b14c9653891f9e74264a383fa43fefbd"
            }
        }"#;

        let git_ref: GitRef = serde_json::from_str(json).unwrap();
        assert_eq!(
            git_ref.object.sha,
            "aa218f56b14c9653891f9e74264a383fa43fefbd"
        );
    }
}
