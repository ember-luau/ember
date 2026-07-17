use crate::error::Error;
use crate::manifest::Manifest;

/// Publishing scaffold. The eventual flow:
/// 1. Authenticate with GitHub via the device flow, using the `github_oauth_id`
///    from the target index's config.toml (see `Index::github_oauth_id`), and
///    cache the token under ~/.lpm/credentials.toml.
/// 2. Pack the package (respecting the manifest) into a .tar.gz.
/// 3. Upload it to the index's registry API (or open an index PR carrying a
///    direct download URL, matching the lpm index format).
pub fn run() -> Result<(), Error> {
    // Validate the manifest so `lpm publish` at least catches a broken file.
    let manifest = Manifest::load()?;
    if manifest.target.is_none() {
        return Err(Error::ManifestInvalid(
            "publishing requires a [target] section with an environment".to_string(),
        ));
    }

    Err(Error::Unimplemented("Publishing"))
}

/// GitHub OAuth device flow stub: POST https://github.com/login/device/code
/// with the index's client id, direct the user to enter the code, then poll
/// https://github.com/login/oauth/access_token until authorized.
#[allow(dead_code)]
fn github_device_auth(_client_id: &str) -> Result<String, Error> {
    Err(Error::Unimplemented("GitHub authentication"))
}
