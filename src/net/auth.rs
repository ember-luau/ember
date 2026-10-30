/*!
GitHub's OAuth device flow and the token store it writes. The device flow
gives a GitHub token, and `POST /v1/auth/token` trades it for an Ember
token. embr stores and sends only the Ember token.
*/

use crate::error::Error;
use crate::net::http;
use crate::net::http::responses;
use crate::net::registry;
use crate::sys::paths;
use crate::ui;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// every Ember token starts with this. embr 0.9 stored a GitHub token instead.
pub const TOKEN_PREFIX: &str = "ember_";

/// an Ember token here wins over the stored login. CI sets it.
pub const TOKEN_VAR: &str = "EMBER_TOKEN";

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

pub struct Credentials {
    pub token: String,
    pub login: String,
}

/// On-disk shape of credentials.toml. Keeps the public struct serde-free.
#[derive(Serialize, Deserialize)]
struct CredentialsFile {
    token: String,
    login: String,
}

pub fn load() -> Result<Option<Credentials>, Error> {
    let path = paths::credentials_file()?;

    /* Corrupt or unreadable file reads as "not logged in". Worst case is
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

/** Deletes the stored credentials, missing file is fine. Called when the
registry rejects the token, revoked or expired, so the next attempt runs
the device flow instead of resending a dead token forever. */
pub fn clear() -> Result<(), Error> {
    match fs::remove_file(paths::credentials_file()?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/** Runs the device flow regardless of stored state, trades its GitHub
token for an Ember token, and saves that. `client_id` comes from the
index's config, not the binary. */
pub fn login(client_id: &str) -> Result<Credentials, Error> {
    let credentials = exchange(&registry::api_url(), &device_flow(client_id)?)?;
    save(&credentials)?;
    ui::print_success(&format!("Logged in as {}", credentials.login));
    Ok(credentials)
}

/// EMBER_TOKEN, when it is set and not empty.
pub fn env_token() -> Option<String> {
    std::env::var(TOKEN_VAR)
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/** the Ember token to send: EMBER_TOKEN, else the stored login. a GitHub
token that embr 0.9 stored is traded for an Ember token on first use. a
GitHub token that the API refuses is deleted, as a dead login. */
pub fn token() -> Result<Option<String>, Error> {
    if let Some(token) = env_token() {
        return Ok(Some(token));
    }
    /* one trade at a time. without the lock, each download worker of an
    install trades the same GitHub token for a new Ember token */
    static TRADE: Mutex<()> = Mutex::new(());
    let _trade = TRADE.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(stored) = load()? else {
        return Ok(None);
    };
    if stored.token.starts_with(TOKEN_PREFIX) {
        return Ok(Some(stored.token));
    }
    match upgrade(&registry::api_url(), &stored.token)? {
        Some(credentials) => {
            save(&credentials)?;
            Ok(Some(credentials.token))
        }
        None => {
            clear()?;
            Ok(None)
        }
    }
}

/// the Ember token for a GitHub token from embr 0.9. None when the API refuses it.
fn upgrade(api: &str, github_token: &str) -> Result<Option<Credentials>, Error> {
    match exchange(api, github_token) {
        Err(Error::TokenRejected) => Ok(None),
        other => other.map(Some),
    }
}

/// trades a GitHub token for an Ember token at `POST {api}/v1/auth/token`.
pub fn exchange(api: &str, github_token: &str) -> Result<Credentials, Error> {
    #[derive(Deserialize)]
    struct Exchanged {
        token: String,
        login: String,
    }
    let url = format!("{api}/v1/auth/token");
    let answer: Exchanged = call(http::agent().post(&url), github_token)?;
    Ok(Credentials {
        token: answer.token,
        login: answer.login,
    })
}

/// checks an Ember token at `GET {api}/api/me`, which also gives its login.
pub fn check(api: &str, token: &str) -> Result<Credentials, Error> {
    #[derive(Deserialize)]
    struct Me {
        login: String,
    }
    let me: Me = call(http::agent().get(&format!("{api}/api/me")), token)?;
    Ok(Credentials {
        token: token.to_string(),
        login: me.login,
    })
}

/// sends `request` with `token` and reads the JSON answer.
fn call<T: DeserializeOwned>(request: ureq::Request, token: &str) -> Result<T, Error> {
    request
        .set("User-Agent", http::USER_AGENT)
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(registry::api_error)?
        .into_json()
        .map_err(|error| http::error::HttpError::from(error).into())
}

/// the GitHub token that the device flow ends with.
fn device_flow(client_id: &str) -> Result<String, Error> {
    /* No OAuth scopes requested. The registry only maps the token to a
    GitHub login, and an unscoped token can already read the public
    profile. */
    let device: responses::DeviceCode = http::post_form(
        DEVICE_CODE_URL,
        &[("Accept", "application/json")],
        &[("client_id", client_id)],
    )?;

    print_instructions(&device);
    poll_for_token(client_id, &device)
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

    /// embr 0.9 stored a GitHub token. it goes to the API once, and an Ember token comes back.
    #[test]
    fn a_github_token_from_0_9_is_traded_for_an_ember_token() {
        use crate::net::http::mock;
        let (api, log) = mock::serve(|request| {
            if request.contains("authorization: bearer gho_dead") {
                mock::reply(
                    "401 Unauthorized",
                    r#"{"error":"GitHub rejected the token"}"#,
                )
            } else {
                let body = r#"{"token":"ember_abc","login":"octocat","expires_at":1}"#;
                mock::reply("201 Created", body)
            }
        });

        let traded = upgrade(&api, "gho_live")
            .unwrap()
            .expect("the API takes it");
        assert_eq!(traded.token, "ember_abc");
        assert_eq!(traded.login, "octocat");
        let first = log.lock().unwrap()[0].clone();
        assert!(first.starts_with("post /v1/auth/token "), "{first}");
        assert!(first.contains("authorization: bearer gho_live"), "{first}");

        // a GitHub token that the API refuses is a dead login, not an error
        assert!(upgrade(&api, "gho_dead").unwrap().is_none());
    }

    #[test]
    fn an_ember_token_is_checked_at_api_me() {
        use crate::net::http::mock;
        let (api, log) = mock::serve(|request| {
            if request.contains("bearer ember_good") {
                mock::reply("200 OK", r#"{"id":1,"login":"octocat"}"#)
            } else {
                mock::reply("401 Unauthorized", r#"{"error":"sign in first"}"#)
            }
        });

        assert_eq!(check(&api, "ember_good").unwrap().login, "octocat");
        assert!(log.lock().unwrap()[0].starts_with("get /api/me "));
        assert!(matches!(
            check(&api, "ember_bad"),
            Err(Error::TokenRejected)
        ));
    }

    #[test]
    fn corrupt_file_reads_as_not_logged_in() {
        assert!(from_toml("not = valid = toml").is_none());
        assert!(from_toml(r#"token = "half-a-file""#).is_none());
    }
}
