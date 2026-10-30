/*!
The browser sign-in and the token store it writes. `embr auth login` starts
a login at `POST /v1/cli/login` and shows a link and a code. The user
approves the code on emberpm.dev, and `POST /v1/cli/login/token` then gives
an Ember token. The link works on any device, so the login also works over
SSH. embr stores and sends only the Ember token.
*/

use crate::error::Error;
use crate::net::http;
use crate::net::http::error::HttpError;
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

/// RFC 8628 asks a client that polls too fast to wait this much longer.
const SLOW_DOWN_SECS: u64 = 5;

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
    a new login, while a hard failure here would brick every command that
    touches credentials. */
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
registry rejects the token, revoked or expired, so the next attempt logs
in again instead of resending a dead token forever. */
pub fn clear() -> Result<(), Error> {
    match fs::remove_file(paths::credentials_file()?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/** Logs in through the browser regardless of stored state and saves the
Ember token. The API makes the token when the user approves the code. */
pub fn login() -> Result<Credentials, Error> {
    let api = registry::api_url();
    let started = start(&api, &login_name())?;
    print_instructions(&started);
    open_browser(&started.verification_uri);
    let spinner = ui::spinner("Waiting for the approval in the browser");
    let result = poll(&api, &started, &mut std::thread::sleep);
    spinner.finish_and_clear();
    let credentials = result?;
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

/// the answer of `POST /v1/cli/login`, the device authorization of RFC 8628.
#[derive(Deserialize)]
struct Started {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    /// the seconds between polls. a faster poll gets `slow_down`.
    interval: u64,
}

/// starts a login at `POST {api}/v1/cli/login`. `name` becomes the token's name.
fn start(api: &str, name: &str) -> Result<Started, Error> {
    http::agent()
        .post(&format!("{api}/v1/cli/login"))
        .set("User-Agent", http::USER_AGENT)
        .send_json(serde_json::json!({ "name": name }))
        .map_err(registry::api_error)?
        .into_json()
        .map_err(|error| HttpError::from(error).into())
}

/// the token name that the site shows: "embr on <hostname>".
fn login_name() -> String {
    match hostname() {
        Some(host) => format!("embr on {host}"),
        None => "embr CLI".to_string(),
    }
}

#[cfg(unix)]
fn hostname() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: gethostname writes at most buffer.len() bytes into the buffer.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    let host = String::from_utf8_lossy(&buffer[..end]).trim().to_string();
    (!host.is_empty()).then_some(host)
}

#[cfg(not(unix))]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
}

fn print_instructions(started: &Started) {
    use crossterm::style::Stylize;

    let (r, g, b) = ui::ACCENT;
    let accent = crossterm::style::Color::Rgb { r, g, b };
    println!(
        "Open {} to connect embr",
        started.verification_uri.as_str().with(accent),
    );
    println!(
        "Approve only if the page shows the code {}",
        started.user_code.as_str().with(accent).bold(),
    );
}

/** opens `url` in the browser, best effort. over SSH, or on a Linux box
with no display, the browser would open on the wrong screen or not at
all, so embr only prints the link. a failure leaves the printed link. */
fn open_browser(url: &str) {
    let remote = ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some());
    let no_display = cfg!(all(unix, not(target_os = "macos")))
        && ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .all(|name| std::env::var_os(name).is_none());
    if !remote && !no_display {
        let _ = crate::commands::studio::os_open(std::ffi::OsStr::new(url));
    }
}

/** polls `POST {api}/v1/cli/login/token` every `interval` seconds until
the user answers or the code expires. `slow_down` adds 5 seconds to the
interval. `sleep` waits; tests pass one that only records. */
fn poll(
    api: &str,
    started: &Started,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Credentials, Error> {
    #[derive(Deserialize)]
    struct Token {
        token: String,
        login: String,
    }
    #[derive(Deserialize)]
    struct Refused {
        error: String,
    }
    let url = format!("{api}/v1/cli/login/token");
    let body = serde_json::json!({ "device_code": started.device_code });
    let mut interval = started.interval.max(1);
    let mut remaining = started.expires_in;
    let expired = || Error::AuthFailed("the code expired; run `embr auth login` again".to_string());
    loop {
        if remaining < interval {
            return Err(expired());
        }
        sleep(Duration::from_secs(interval));
        remaining -= interval;

        let answer = http::agent()
            .post(&url)
            .set("User-Agent", http::USER_AGENT)
            .send_json(&body);
        let refused = match answer {
            Ok(response) => {
                let token: Token = response.into_json().map_err(HttpError::from)?;
                return Ok(Credentials {
                    token: token.token,
                    login: token.login,
                });
            }
            Err(ureq::Error::Status(400, response)) => response
                .into_json::<Refused>()
                .map(|refused| refused.error)
                .unwrap_or_default(),
            Err(error) => return Err(registry::api_error(error)),
        };
        match refused.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += SLOW_DOWN_SECS,
            "access_denied" => {
                return Err(Error::AuthFailed(
                    "it was denied in the browser".to_string(),
                ));
            }
            "expired_token" => return Err(expired()),
            other => {
                return Err(Error::AuthFailed(format!(
                    "the registry answered `{other}`"
                )));
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
    use crate::net::http::mock;

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

    /// a start answer, as the API sends it.
    fn started(expires_in: u64) -> Started {
        Started {
            device_code: "d".repeat(64),
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: "https://emberpm.dev/cli/connect?code=ABCD-EFGH".to_string(),
            expires_in,
            interval: 5,
        }
    }

    /// an API whose polls answer `polls` in turn, and the last one after that.
    fn polls(polls: &'static [(&'static str, &'static str)]) -> (String, mock::Log) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = AtomicUsize::new(0);
        mock::serve(move |_| {
            let at = count.fetch_add(1, Ordering::SeqCst).min(polls.len() - 1);
            mock::reply(polls[at].0, polls[at].1)
        })
    }

    const PENDING: (&str, &str) = ("400 Bad Request", r#"{"error":"authorization_pending"}"#);
    const SLOW_DOWN: (&str, &str) = ("400 Bad Request", r#"{"error":"slow_down"}"#);

    #[test]
    fn a_start_sends_the_name_and_reads_both_codes() {
        let body = r#"{"device_code":"ab12","user_code":"ABCD-EFGH",
            "verification_uri":"http://site/cli/connect?code=ABCD-EFGH",
            "expires_in":600,"interval":5}"#;
        let (api, log) = mock::serve(move |_| mock::reply("200 OK", body));
        let started = start(&api, "embr on box").unwrap();
        assert_eq!(started.device_code, "ab12");
        assert_eq!(started.user_code, "ABCD-EFGH");
        assert_eq!(
            started.verification_uri,
            "http://site/cli/connect?code=ABCD-EFGH"
        );
        assert_eq!((started.expires_in, started.interval), (600, 5));
        let sent = log.lock().unwrap()[0].clone();
        assert!(sent.starts_with("post /v1/cli/login "), "{sent}");
        assert!(sent.ends_with(r#"{"name":"embr on box"}"#), "{sent}");
    }

    /// pending, then slow_down, then the token: the waits are 5, 5 and 10 seconds.
    #[test]
    fn a_login_polls_at_the_interval_until_the_approval() {
        const TOKEN: (&str, &str) = (
            "200 OK",
            r#"{"token":"ember_abc","login":"octocat","expires_at":1}"#,
        );
        let (api, log) = polls(&[PENDING, SLOW_DOWN, TOKEN]);
        let mut waits = Vec::new();
        let credentials = poll(&api, &started(600), &mut |wait| waits.push(wait)).unwrap();
        assert_eq!(credentials.token, "ember_abc");
        assert_eq!(credentials.login, "octocat");
        assert_eq!(waits, [5, 5, 10].map(Duration::from_secs));
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3);
        for sent in log.iter() {
            assert!(sent.starts_with("post /v1/cli/login/token "), "{sent}");
            let body = format!(r#"{{"device_code":"{}"}}"#, "d".repeat(64));
            assert!(sent.ends_with(&body), "{sent}");
        }
    }

    #[test]
    fn a_denied_or_expired_login_fails() {
        for (answer, says) in [
            (r#"{"error":"access_denied"}"#, "denied"),
            (r#"{"error":"expired_token"}"#, "expired"),
            (r#"{"error":"invalid_grant"}"#, "invalid_grant"),
        ] {
            let (api, _) = mock::serve(move |_| mock::reply("400 Bad Request", answer));
            match poll(&api, &started(600), &mut |_| {}) {
                Err(Error::AuthFailed(message)) => assert!(message.contains(says), "{message}"),
                other => panic!("{answer}: {:?}", other.map(|c| c.login)),
            }
        }
    }

    /// the code's own end stops the polls when the API never says it expired.
    #[test]
    fn the_poll_stops_when_the_code_runs_out() {
        let (api, log) = polls(&[PENDING]);
        let mut waits = Vec::new();
        let result = poll(&api, &started(12), &mut |wait| waits.push(wait));
        assert!(matches!(result, Err(Error::AuthFailed(m)) if m.contains("expired")));
        assert_eq!(waits.len(), 2);
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_server_error_is_no_poll_answer() {
        let (api, _) = mock::serve(|_| {
            mock::reply("500 Internal Server Error", r#"{"error":"database error"}"#)
        });
        assert!(matches!(
            poll(&api, &started(600), &mut |_| {}),
            Err(Error::RegistryRefused { status: 500, .. })
        ));
    }

    #[test]
    fn corrupt_file_reads_as_not_logged_in() {
        assert!(from_toml("not = valid = toml").is_none());
        assert!(from_toml(r#"token = "half-a-file""#).is_none());
    }
}
