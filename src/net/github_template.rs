/*! GitHub calls for project templates: the token, the repository tarball
that `embr init --template owner/repo` unpacks, and the template repository
that `embr template publish` creates. embr reads the token on each run and
never writes it to disk. */

use crate::error::Error;
use crate::net::http::{self, error::HttpError};
use base64::Engine;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const API: &str = "https://api.github.com";
const ACCEPT: &str = "application/vnd.github+json";

/** a GitHub token: `gh auth token` when the gh CLI is installed and logged
in, then GH_TOKEN, then GITHUB_TOKEN. None when no source has one. */
pub fn token() -> Option<String> {
    token_from(gh_token, |name| std::env::var(name).ok())
}

/// [`token`] with its two sources passed in, so tests need no gh and no env.
fn token_from(
    gh: impl FnOnce() -> Option<String>,
    var: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let clean = |token: String| Some(token.trim().to_string()).filter(|token| !token.is_empty());
    gh().and_then(clean).or_else(|| {
        ["GH_TOKEN", "GITHUB_TOKEN"]
            .into_iter()
            .find_map(|name| var(name).and_then(clean))
    })
}

/// the token of the gh CLI. None when gh is missing or logged out.
fn gh_token() -> Option<String> {
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `request` with the headers every authenticated call sends.
fn authed(request: ureq::Request, token: &str) -> ureq::Request {
    request
        .set("User-Agent", http::USER_AGENT)
        .set("Accept", ACCEPT)
        .set("Authorization", &format!("Bearer {token}"))
}

/** the error for a GitHub answer that is not a success, with GitHub's own
message and the detail it gives, such as "name already exists on this
account". */
fn refused(error: ureq::Error) -> Error {
    #[derive(Deserialize)]
    struct Body {
        message: String,
        #[serde(default)]
        errors: Vec<Detail>,
    }
    #[derive(Deserialize)]
    struct Detail {
        message: Option<String>,
    }
    match error {
        ureq::Error::Status(status, response) => {
            let message = match response.into_json::<Body>() {
                Ok(body) => {
                    let details: Vec<String> = body
                        .errors
                        .into_iter()
                        .filter_map(|detail| detail.message)
                        .collect();
                    match details.is_empty() {
                        true => body.message,
                        false => format!("{} ({})", body.message, details.join("; ")),
                    }
                }
                Err(_) => "no message".to_string(),
            };
            Error::GithubRefused { status, message }
        }
        other => HttpError::from(other).into(),
    }
}

/// sends `body` as JSON with the token and reads the JSON answer.
fn send<T: serde::de::DeserializeOwned>(
    request: ureq::Request,
    token: &str,
    body: serde_json::Value,
) -> Result<T, Error> {
    authed(request, token)
        .send_json(body)
        .map_err(refused)?
        .into_json()
        .map_err(|error| HttpError::from(error).into())
}

/// the login of the account that owns `token`, from `GET /user`.
pub fn login(api: &str, token: &str) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    let user: User = authed(http::agent().get(&format!("{api}/user")), token)
        .call()
        .map_err(refused)?
        .into_json()
        .map_err(HttpError::from)?;
    Ok(user.login)
}

/// a repository that [`create`] made.
#[derive(Deserialize, Debug)]
pub struct Repository {
    pub full_name: String,
    pub html_url: String,
    pub default_branch: String,
}

/// the repository that `embr template publish` asks for.
pub struct NewRepository<'a> {
    pub owner: &'a str,
    pub name: &'a str,
    pub private: bool,
    /** a first commit with a README. the git data API refuses an empty
    repository, so [`push`] needs it, and a push with git does not. */
    pub auto_init: bool,
}

/** creates a template repository (`is_template`). `POST /user/repos` when
the owner is `login`, the token's own account, else `POST /orgs/{owner}/repos`. */
pub fn create(
    api: &str,
    token: &str,
    login: &str,
    repository: &NewRepository,
) -> Result<Repository, Error> {
    let url = if repository.owner.eq_ignore_ascii_case(login) {
        format!("{api}/user/repos")
    } else {
        format!("{api}/orgs/{}/repos", repository.owner)
    };
    let body = serde_json::json!({
        "name": repository.name,
        "private": repository.private,
        "is_template": true,
        "auto_init": repository.auto_init,
    });
    send(http::agent().post(&url), token, body)
}

/** makes the files under `root` the only commit on `branch` of `repository`,
with the git data API: a blob per file, one tree, a commit with no parent,
and a forced move of the branch onto it. the forced move drops the README
commit of `auto_init`. */
pub fn push(
    api: &str,
    token: &str,
    repository: &str,
    branch: &str,
    root: &Path,
    files: &[PathBuf],
) -> Result<(), Error> {
    #[derive(Deserialize)]
    struct Sha {
        sha: String,
    }
    let base = format!("{api}/repos/{repository}/git");
    let mut tree = Vec::new();
    for file in files {
        let bytes = std::fs::read(root.join(file))?;
        let content = base64::engine::general_purpose::STANDARD.encode(bytes);
        let blob: Sha = send(
            http::agent().post(&format!("{base}/blobs")),
            token,
            serde_json::json!({ "content": content, "encoding": "base64" }),
        )?;
        tree.push(serde_json::json!({
            "path": crate::registry::pack::tar_path(file),
            "mode": if executable(&root.join(file)) { "100755" } else { "100644" },
            "type": "blob",
            "sha": blob.sha,
        }));
    }
    let tree: Sha = send(
        http::agent().post(&format!("{base}/trees")),
        token,
        serde_json::json!({ "tree": tree }),
    )?;
    let commit: Sha = send(
        http::agent().post(&format!("{base}/commits")),
        token,
        serde_json::json!({ "message": "Initial commit", "tree": tree.sha, "parents": [] }),
    )?;
    let _: serde_json::Value = send(
        http::agent().request("PATCH", &format!("{base}/refs/heads/{branch}")),
        token,
        serde_json::json!({ "sha": commit.sha, "force": true }),
    )?;
    Ok(())
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(_path: &Path) -> bool {
    false
}

/** pushes the files under `work_tree` to `remote` as one commit on
`branch`, with the git CLI. the git folder is a temp folder, so the saved
template stays a plain folder. the token reaches git in GIT_CONFIG_*
variables, for github.com only: not on disk, and not in the argument list
that other users can read. `login` signs the commit when git has no
identity of its own. */
pub fn push_with_git(
    remote: &str,
    token: Option<&str>,
    work_tree: &Path,
    branch: &str,
    login: &str,
) -> Result<(), Error> {
    let git_dir = std::env::temp_dir().join(format!("embr-template-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&git_dir);
    let refspec = format!("HEAD:refs/heads/{branch}");
    let steps: [&[&str]; 4] = [
        &["init", "-q"],
        &["add", "-A"],
        &["commit", "-q", "--no-verify", "-m", "Initial commit"],
        &["push", "-q", "--no-verify", remote, &refspec],
    ];
    let result = steps
        .iter()
        .try_for_each(|args| git(&git_dir, work_tree, token, login, args));
    let _ = std::fs::remove_dir_all(&git_dir);
    result
}

fn git(
    git_dir: &Path,
    work_tree: &Path,
    token: Option<&str>,
    login: &str,
    args: &[&str],
) -> Result<(), Error> {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(git_dir)
        .arg("--work-tree")
        .arg(work_tree)
        // the bytes as saved, and no signing prompt in the middle of a publish
        .args(["-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(token) = token {
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
        command
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
            .env(
                "GIT_CONFIG_VALUE_0",
                format!("Authorization: Basic {basic}"),
            )
            // no credential helper, so no helper stores the token
            .env("GIT_CONFIG_KEY_1", "credential.helper")
            .env("GIT_CONFIG_VALUE_1", "");
    }
    if crate::sys::git::output(&["config", "user.email"]).is_none() {
        let email = format!("{login}@users.noreply.github.com");
        command
            .env("GIT_AUTHOR_NAME", login)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_COMMITTER_NAME", login)
            .env("GIT_COMMITTER_EMAIL", &email);
    }
    let output = command.output().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Error::GitMissing,
        _ => error.into(),
    })?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::Template(format!(
        "git {} failed: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

/** downloads the tarball of `repository` at `reference`, or at its default
branch. the API answers with a redirect, and embr follows it without the
token, so the token goes to the API host only. */
pub fn tarball(
    api: &str,
    repository: &str,
    reference: Option<&str>,
    token: Option<&str>,
) -> Result<Vec<u8>, Error> {
    let url = match reference {
        Some(reference) => format!("{api}/repos/{repository}/tarball/{reference}"),
        None => format!("{api}/repos/{repository}/tarball"),
    };
    let mut request = http::manual_redirect_agent()
        .get(&url)
        .set("User-Agent", http::USER_AGENT)
        .set("Accept", ACCEPT);
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    let response = request.call().map_err(HttpError::from)?;
    if !(300..400).contains(&response.status()) {
        return Ok(http::read_body(response, &mut |_, _| {})?);
    }
    let location = response
        .header("location")
        .ok_or(HttpError::RedirectMissingLocation { url })?
        .to_string();
    Ok(http::get_bytes(&location, &[])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::http::mock;

    #[test]
    fn the_token_comes_from_gh_then_gh_token_then_github_token() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        let both = env(&[("GH_TOKEN", "gh_env"), ("GITHUB_TOKEN", "github_env")]);
        let gh = || Some("gho_cli\n".to_string());
        let no_gh = || None;

        assert_eq!(token_from(gh, both).as_deref(), Some("gho_cli"));
        assert_eq!(token_from(no_gh, both).as_deref(), Some("gh_env"));
        assert_eq!(
            token_from(no_gh, env(&[("GITHUB_TOKEN", "github_env")])).as_deref(),
            Some("github_env")
        );
        // an empty value is no token, and the next source gets its turn
        let empty = env(&[("GH_TOKEN", " "), ("GITHUB_TOKEN", "github_env")]);
        assert_eq!(
            token_from(|| Some(String::new()), empty).as_deref(),
            Some("github_env")
        );
        assert_eq!(token_from(no_gh, env(&[])), None);
    }

    #[test]
    fn a_template_repository_is_created_for_the_user_or_the_org() {
        let body = r#"{"full_name":"octocat/game","html_url":"https://github.com/octocat/game","default_branch":"main"}"#;
        let (api, log) = mock::serve(move |_| mock::reply("201 Created", body));

        let mine = NewRepository {
            owner: "OctoCat",
            name: "game",
            private: true,
            auto_init: false,
        };
        let created = create(&api, "gho_x", "octocat", &mine).unwrap();
        assert_eq!(created.full_name, "octocat/game");
        assert_eq!(created.default_branch, "main");

        let org = NewRepository {
            owner: "ember-luau",
            private: false,
            auto_init: true,
            ..mine
        };
        create(&api, "gho_x", "octocat", &org).unwrap();

        let log = log.lock().unwrap();
        assert!(log[0].starts_with("post /user/repos "), "{}", log[0]);
        assert!(log[0].contains("authorization: bearer gho_x"));
        for field in [
            r#""name":"game""#,
            r#""private":true"#,
            r#""is_template":true"#,
            r#""auto_init":false"#,
        ] {
            assert!(log[0].contains(field), "{field} in {}", log[0]);
        }
        assert!(
            log[1].starts_with("post /orgs/ember-luau/repos "),
            "{}",
            log[1]
        );
        assert!(log[1].contains(r#""private":false"#));
        assert!(log[1].contains(r#""is_template":true"#));
    }

    #[test]
    fn a_refusal_carries_the_message_of_github() {
        let body = r#"{"message":"Repository creation failed.","errors":[{"resource":"Repository","code":"custom","field":"name","message":"name already exists on this account"}]}"#;
        let (api, _) = mock::serve(move |_| mock::reply("422 Unprocessable Entity", body));
        let repository = NewRepository {
            owner: "octocat",
            name: "game",
            private: false,
            auto_init: false,
        };
        match create(&api, "gho_x", "octocat", &repository) {
            Err(Error::GithubRefused { status, message }) => {
                assert_eq!(status, 422);
                assert_eq!(
                    message,
                    "Repository creation failed. (name already exists on this account)"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_login_comes_from_api_user() {
        let (api, log) = mock::serve(|_| mock::reply("200 OK", r#"{"login":"octocat","id":1}"#));
        assert_eq!(login(&api, "gho_x").unwrap(), "octocat");
        assert!(log.lock().unwrap()[0].starts_with("get /user "));
    }

    #[test]
    fn the_api_push_makes_one_commit_with_no_parent() {
        let (api, log) = mock::serve(|request| {
            let sha = if request.starts_with("post /repos/octocat/game/git/blobs") {
                "blob"
            } else if request.starts_with("post /repos/octocat/game/git/trees") {
                "tree"
            } else if request.starts_with("post /repos/octocat/game/git/commits") {
                "commit"
            } else {
                "ref"
            };
            mock::reply("201 Created", &format!(r#"{{"sha":"{sha}"}}"#))
        });
        let root = std::env::temp_dir().join("embr-test-template-api-push");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("ember.toml"), "[target]\n").unwrap();
        std::fs::write(root.join("src/init.luau"), "return {}\n").unwrap();
        let files = [
            PathBuf::from("ember.toml"),
            Path::new("src").join("init.luau"),
        ];

        push(&api, "gho_x", "octocat/game", "main", &root, &files).unwrap();
        let log = log.lock().unwrap();
        let lines: Vec<&str> = log
            .iter()
            .map(|request| request.lines().next().unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                "post /repos/octocat/game/git/blobs http/1.1",
                "post /repos/octocat/game/git/blobs http/1.1",
                "post /repos/octocat/game/git/trees http/1.1",
                "post /repos/octocat/game/git/commits http/1.1",
                "patch /repos/octocat/game/git/refs/heads/main http/1.1",
            ]
        );
        // base64 of "[target]\n", lowercased by the mock log
        assert!(log[0].contains(r#""content":"w3rhcmdldf0k""#), "{}", log[0]);
        assert!(log[2].contains(r#""path":"src/init.luau""#), "{}", log[2]);
        assert!(log[3].contains(r#""parents":[]"#), "{}", log[3]);
        assert!(log[3].contains(r#""tree":"tree""#));
        assert!(log[4].contains(r#""force":true"#));
        assert!(log[4].contains(r#""sha":"commit""#));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tarball_follows_the_redirect_without_the_token() {
        let (cdn, cdn_log) = mock::serve(|_| mock::reply("200 OK", "tarball"));
        let to_cdn = format!(
            "HTTP/1.1 302 Found\r\nLocation: {cdn}/octocat-game-abc.tar.gz\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (api, api_log) = mock::serve(move |_| to_cdn.clone());

        let bytes = tarball(&api, "octocat/game", Some("v1"), Some("gho_x")).unwrap();
        assert_eq!(bytes, b"tarball");
        let sent = api_log.lock().unwrap()[0].clone();
        assert!(
            sent.starts_with("get /repos/octocat/game/tarball/v1 "),
            "{sent}"
        );
        assert!(sent.contains("authorization: bearer gho_x"));
        assert!(!cdn_log.lock().unwrap()[0].contains("authorization"));

        // no token, no header, and no reference means the default branch
        tarball(&api, "octocat/game", None, None).unwrap();
        let sent = api_log.lock().unwrap()[1].clone();
        assert!(
            sent.starts_with("get /repos/octocat/game/tarball "),
            "{sent}"
        );
        assert!(!sent.contains("authorization"));
    }

    #[test]
    fn a_missing_repository_is_not_found() {
        let (api, _) = mock::serve(|_| mock::reply("404 Not Found", r#"{"message":"Not Found"}"#));
        assert!(matches!(
            tarball(&api, "octocat/private", None, None),
            Err(Error::Http(HttpError::NotFound))
        ));
    }

    #[test]
    fn the_git_push_makes_one_commit_on_the_branch() {
        if !crate::sys::git::available() {
            return;
        }
        let root = std::env::temp_dir().join("embr-test-template-git-push");
        let _ = std::fs::remove_dir_all(&root);
        let (work, remote) = (root.join("work"), root.join("remote.git"));
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::write(work.join("ember.toml"), "[target]\n").unwrap();
        std::fs::write(work.join("src/init.luau"), "return {}\n").unwrap();
        let bare = Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&remote)
            .status()
            .unwrap();
        assert!(bare.success());

        push_with_git(&remote.to_string_lossy(), None, &work, "main", "octocat").unwrap();
        let read = |args: &[&str]| {
            let output = Command::new("git")
                .arg("--git-dir")
                .arg(&remote)
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        assert_eq!(read(&["log", "--format=%s", "main"]), "Initial commit");
        assert_eq!(
            read(&["ls-tree", "-r", "--name-only", "main"]),
            "ember.toml\nsrc/init.luau"
        );
        // the template folder stays a plain folder
        assert!(!work.join(".git").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
