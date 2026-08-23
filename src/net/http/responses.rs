//! Shapes of the GitHub responses embr deserializes.

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
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
    /// Minimum seconds between polls, going faster earns a `slow_down` error.
    pub interval: u64,
}

/** Device-flow poll result. GitHub reports "not yet" states like
`authorization_pending` and `slow_down` as HTTP 200 with an `error`
field, not an error status. */
#[derive(Deserialize)]
pub struct AccessToken {
    pub access_token: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct User {
    pub login: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_github_release() {
        let json = r#"{
            "url": "https://api.github.com/repos/embr/embr/releases/213371337213",
            "html_url": "https://github.com/luaupm/embr/releases/tag/v0.1.0",
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
                    "url": "https://api.github.com/repos/embr/embr/releases/assets/1",
                    "id": 1,
                    "name": "embr-windows-x86_64.exe",
                    "content_type": "application/octet-stream",
                    "size": 4200000,
                    "browser_download_url": "https://github.com/luaupm/embr/releases/download/v0.1.0/embr-windows-x86_64.exe"
                },
                {
                    "name": "embr-linux-x86_64",
                    "browser_download_url": "https://github.com/luaupm/embr/releases/download/v0.1.0/embr-linux-x86_64"
                }
            ]
        }"#;

        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v0.1.0");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "embr-windows-x86_64.exe");
        assert_eq!(
            release.assets[0].browser_download_url,
            "https://github.com/luaupm/embr/releases/download/v0.1.0/embr-windows-x86_64.exe"
        );
        assert_eq!(release.assets[1].name, "embr-linux-x86_64");
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
    }
}
