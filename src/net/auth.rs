/*!
GitHub's OAuth device flow and the token store it writes. Publishing sends
the token as a Bearer header; the registry API maps it to a GitHub login for
scope ownership.
*/

use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::net::http;
use crate::net::http::responses;
use crate::sys::paths;
use crate::ui;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::Duration;

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

pub struct Credentials {
    pub token: String,
    pub login: String,
}

/// On-disk shape of credentials.toml; keeps the public struct serde-free.
#[derive(Serialize, Deserialize)]
struct CredentialsFile {
    token: String,
    login: String,
}

pub fn load() -> Result<Option<Credentials>, Error> {
    let path = paths::credentials_file()?;

    /* Corrupt or unreadable file reads as "not logged in": worst case is
    re-running the device flow, while a hard failure here would brick
    every command that touches credentials. */
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(None);
    };
    Ok(from_toml(&text))
}

pub fn save(credentials: &Credentials) -> Result<(), Error> {
    let path = paths::credentials_file()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, to_toml(credentials))?;
    restrict_permissions(&path)?;
    Ok(())
}

/** Deletes the stored credentials; missing file is fine. Called when the
registry rejects the token (revoked/expired) so the next attempt runs the
device flow instead of resending a dead token forever. */
pub fn clear() -> Result<(), Error> {
    match fs::remove_file(paths::credentials_file()?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/** Runs the device flow regardless of stored state and saves the result.
`client_id` comes from the index's config, not the binary. */
pub fn login(client_id: &str) -> Result<Credentials, Error> {
    let credentials = device_flow(client_id)?;
    save(&credentials)?;
    ui::print_success(&format!("Logged in as {}", credentials.login));
    Ok(credentials)
}

fn device_flow(client_id: &str) -> Result<Credentials, Error> {
    /* No OAuth scopes requested: the registry only maps the token to a
    GitHub login, and an unscoped token can already read the public
    profile. */
    let device: responses::DeviceCode = http::post_form(
        DEVICE_CODE_URL,
        &[("Accept", "application/json")],
        &[("client_id", client_id)],
    )?;

    print_instructions(&device);

    let token = poll_for_token(client_id, &device)?;
    let user = GithubAPI::with_token(&token).get_user()?;

    Ok(Credentials {
        token,
        login: user.login,
    })
}

fn print_instructions(device: &responses::DeviceCode) {
    use crossterm::style::Stylize;

    let (r, g, b) = ui::ACCENT;
    let accent = crossterm::style::Color::Rgb { r, g, b };
    println!(
        "Open {} and enter the code {}",
        device.verification_uri.as_str().with(accent),
        device.user_code.as_str().with(accent).bold(),
    );
}

fn poll_for_token(client_id: &str, device: &responses::DeviceCode) -> Result<String, Error> {
    let spinner = ui::spinner("Waiting for authorization");
    let result = poll_loop(client_id, device);
    spinner.finish_and_clear();
    result
}

fn poll_loop(client_id: &str, device: &responses::DeviceCode) -> Result<String, Error> {
    let mut interval = device.interval.max(1);
    let mut remaining = device.expires_in;

    loop {
        if remaining < interval {
            return Err(Error::AuthFailed(
                "the device code expired before it was authorized".to_string(),
            ));
        }
        std::thread::sleep(Duration::from_secs(interval));
        remaining -= interval;

        let response: responses::AccessToken = http::post_form(
            ACCESS_TOKEN_URL,
            &[("Accept", "application/json")],
            &[
                ("client_id", client_id),
                ("device_code", &device.device_code),
                ("grant_type", GRANT_TYPE),
            ],
        )?;

        if let Some(token) = response.access_token {
            return Ok(token);
        }

        match response.error.as_deref() {
            Some("authorization_pending") => {}
            // GitHub asks clients that poll too fast to back off by 5s.
            Some("slow_down") => interval += 5,
            Some(other) => return Err(Error::AuthFailed(other.to_string())),
            None => {
                return Err(Error::AuthFailed(
                    "GitHub returned neither a token nor an error".to_string(),
                ));
            }
        }
    }
}

fn to_toml(credentials: &Credentials) -> String {
    let file = CredentialsFile {
        token: credentials.token.clone(),
        login: credentials.login.clone(),
    };
    toml::to_string(&file).expect("credentials serialize")
}

fn from_toml(text: &str) -> Option<Credentials> {
    let file: CredentialsFile = toml::from_str(text).ok()?;
    Some(Credentials {
        token: file.token,
        login: file.login,
    })
}

/** Keep the token file owner-only. No-op on Windows, where the profile
directory's ACL already restricts it. */
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_round_trips() {
        let credentials = Credentials {
            token: "gho_abc123".to_string(),
            login: "octocat".to_string(),
        };

        let text = to_toml(&credentials);
        assert!(text.contains(r#"token = "gho_abc123""#));
        assert!(text.contains(r#"login = "octocat""#));

        let parsed = from_toml(&text).expect("round-trips");
        assert_eq!(parsed.token, "gho_abc123");
        assert_eq!(parsed.login, "octocat");
    }

    #[test]
    fn corrupt_file_reads_as_not_logged_in() {
        assert!(from_toml("not = valid = toml").is_none());
        assert!(from_toml(r#"token = "half-a-file""#).is_none());
    }
}
