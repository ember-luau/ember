use serde::Deserialize;

#[derive(Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
    // Deserialized for future use (e.g. publishing); nothing reads them yet.
    #[allow(dead_code)]
    pub url: String,
    #[allow(dead_code)]
    pub id: u64,
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
}
