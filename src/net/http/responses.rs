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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_github_release() {
        let json = r#"{
            "url": "https://api.github.com/repos/ember-luau/ember/releases/213371337213",
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
                    "url": "https://api.github.com/repos/ember-luau/ember/releases/assets/1",
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
}
