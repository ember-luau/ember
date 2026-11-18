//! Shapes of the GitHub responses embr deserializes. a tool on Ember becomes the same shape, see `tools::ember`.

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    /** the lowercase hex SHA-256 of the asset. Ember lists it for an upload
    as `sha256`. GitHub lists it as `digest`, in the form `sha256:<hex>`, for
    each asset uploaded since June 2025, and null for an older one. */
    #[serde(default, alias = "digest", deserialize_with = "sha256_of")]
    pub sha256: Option<String>,
}

/** reads `sha256` or `digest` as lowercase hex. a digest of another
algorithm, or a value that is not 64 hex characters, reads as none: a hash
embr cannot check must not look like one it did. */
fn sha256_of<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let listed = Option::<String>::deserialize(deserializer)?;
    Ok(listed.and_then(|listed| {
        let hex = listed
            .strip_prefix("sha256:")
            .unwrap_or(&listed)
            .to_ascii_lowercase();
        crate::sys::hash::is_sha256_hex(&hex).then_some(hex)
    }))
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

    #[test]
    fn a_github_digest_reads_as_the_sha256() {
        let hex = "2151b604e3429bff440b9fbc03eb3617bc2603cda96c95b9bb05277f9ddba255";
        let asset = |extra: &str| -> Asset {
            serde_json::from_str(&format!(
                r#"{{"name": "a.zip", "browser_download_url": "https://example.com/a.zip"{extra}}}"#
            ))
            .unwrap()
        };
        assert_eq!(
            asset(&format!(r#", "digest": "sha256:{hex}""#))
                .sha256
                .as_deref(),
            Some(hex)
        );
        assert_eq!(
            asset(&format!(r#", "digest": "SHA256:{}""#, hex.to_uppercase())).sha256,
            None,
            "an unknown prefix is no hash"
        );
        assert_eq!(
            asset(&format!(r#", "sha256": "{}""#, hex.to_uppercase()))
                .sha256
                .as_deref(),
            Some(hex)
        );
        // an asset from before GitHub listed digests, and other algorithms
        assert_eq!(asset(r#", "digest": null"#).sha256, None);
        assert_eq!(asset("").sha256, None);
        assert_eq!(asset(r#", "digest": "sha512:abcd""#).sha256, None);
    }
}
