/*! Channels and release notes: `embr publish --channel` and `--notes`,
`embr channel`, `embr notes`, and the `channel` key of a dependency. The
install tests use the local registry of a scratch home, and the registry
calls go to a server in this file, so the test runs offline.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// an API URL that answers nothing, for the runs that must make no call.
const NO_REGISTRY: &str = "http://127.0.0.1:9";

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn run_at(api: &str, dir: &Path, base: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(args)
        .current_dir(dir)
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .env("EMBER_API_URL", api)
        .env("EMBER_TOKEN", "ember_test")
        .env_remove("EMBER_INDEX_TTL_SECS")
        .env_remove("EMBER_OFFLINE")
        .env_remove("CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("embr runs")
}

fn run(dir: &Path, base: &Path, args: &[&str]) -> Output {
    run_at(NO_REGISTRY, dir, base, args)
}

/// stdout of a run that must work.
fn ok(output: Output, args: &[&str]) -> String {
    assert!(output.status.success(), "embr {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn embr(dir: &Path, base: &Path, args: &[&str]) -> String {
    ok(run(dir, base, args), args)
}

/// stderr of a run that must fail.
fn fails(output: Output, args: &[&str]) -> String {
    assert!(!output.status.success(), "embr {args:?}: {output:?}");
    String::from_utf8(output.stderr).unwrap()
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}\n{text}"))
}

/// the folder of acme/`name` at `version`, with a module that returns `build`.
fn package(base: &Path, name: &str, version: &str, build: &str, tables: &str) -> PathBuf {
    let dir = base.join("work").join(name);
    let _ = fs::remove_dir_all(&dir);
    write(
        &dir.join("ember.toml"),
        &format!(
            "[package]\nname = \"acme/{name}\"\nversion = \"{version}\"\nlicense = \"MIT\"\n\
             description = \"a library\"\n\n[target]\nenvironment = \"luau\"\n\
             main = \"src/init.luau\"\n\n{tables}"
        ),
    );
    write(&dir.join("src/init.luau"), &format!("return \"{build}\"\n"));
    dir
}

/// the `ember.lock` entry of `name` in `dir`.
fn locked(dir: &Path, name: &str) -> toml::Table {
    let lock: toml::Table = fs::read_to_string(dir.join("ember.lock"))
        .unwrap()
        .parse()
        .unwrap();
    lock["package"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|package| package.as_table())
        .find(|package| package["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the lock"))
        .clone()
}

/// what the installed acme/fuel returns: the build that the install took.
fn installed(game: &Path) -> String {
    let mut found = Vec::new();
    let mut folders = vec![game.join("packages")];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else if path.ends_with("src/init.luau") && path.to_string_lossy().contains("fuel") {
                found.push(fs::read_to_string(&path).unwrap());
            }
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

/// the ember.toml inside a packed archive.
fn packed_manifest(archive: &[u8]) -> String {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().ends_with("ember.toml") {
            let mut text = String::new();
            entry.read_to_string(&mut text).unwrap();
            return text;
        }
    }
    panic!("the archive has no ember.toml");
}

#[test]
fn a_channel_dependency_locks_one_version_and_only_an_update_moves_it() {
    let base = std::env::temp_dir().join("embr-test-channels-lock");
    let _ = fs::remove_dir_all(&base);
    let fuel = package(&base, "fuel", "1.2.0", "stable", "");
    embr(&fuel, &base, &["publish", "--local"]);

    // the first publish to a channel is <version>-<channel>.1
    let fuel = package(&base, "fuel", "1.3.0", "beta one", "");
    let manifest = fs::read_to_string(fuel.join("ember.toml")).unwrap();
    let out = embr(&fuel, &base, &["publish", "--local", "--channel", "beta"]);
    assert!(out.contains("acme/fuel@1.3.0-beta.1"), "{out}");
    assert!(
        out.contains("The channel beta now names 1.3.0-beta.1"),
        "{out}"
    );
    assert!(out.contains("embr add fuel --channel beta"), "{out}");
    assert_eq!(
        fs::read_to_string(fuel.join("ember.toml")).unwrap(),
        manifest
    );
    let archive =
        fs::read(base.join("home/.ember/local/archives/acme/fuel/1.3.0-beta.1/luau.tar.gz"))
            .unwrap();
    assert!(packed_manifest(&archive).contains("version = \"1.3.0-beta.1\""));

    // `embr add --channel` writes the channel and no requirement
    let game = base.join("game");
    write(
        &game.join("ember.toml"),
        "[target]\nenvironment = \"luau\"\n",
    );
    let added = json(&embr(
        &game,
        &base,
        &["add", "fuel", "--channel", "beta", "--json"],
    ));
    assert_eq!(added["added"][0]["version"], "1.3.0-beta.1");
    assert_eq!(added["added"][0]["channel"], "beta");
    assert_eq!(added["added"][0]["requirement"], Value::Null);
    let text = fs::read_to_string(game.join("ember.toml")).unwrap();
    assert!(
        text.contains("fuel = { name = \"acme/fuel\", channel = \"beta\", index = \"local\" }"),
        "{text}"
    );

    embr(&game, &base, &["install"]);
    let entry = locked(&game, "acme/fuel");
    assert_eq!(entry["version"].as_str(), Some("1.3.0-beta.1"));
    assert_eq!(entry["channel"].as_str(), Some("beta"));
    assert_eq!(installed(&game), "return \"beta one\"\n");

    // the channel moves: the next publish is one more
    let fuel = package(&base, "fuel", "1.3.0", "beta two", "");
    let dry = json(&embr(
        &fuel,
        &base,
        &[
            "publish",
            "--local",
            "--channel",
            "beta",
            "--dry-run",
            "--json",
        ],
    ));
    assert_eq!(dry["published"][0]["version"], "1.3.0-beta.2");
    assert_eq!(dry["published"][0]["channel"], "beta");
    let out = embr(&fuel, &base, &["publish", "--local", "--channel", "beta"]);
    assert!(out.contains("acme/fuel@1.3.0-beta.2"), "{out}");
    let index = fs::read_to_string(base.join("home/.ember/local/index/acme/fuel")).unwrap();
    let index: toml::Table = index.parse().unwrap();
    assert_eq!(
        index["meta"]["channels"]["beta"].as_str(),
        Some("1.3.0-beta.2")
    );

    // a locked install reads no channel, and it takes the locked version
    let lock = fs::read_to_string(game.join("ember.lock")).unwrap();
    embr(&game, &base, &["install", "--locked"]);
    assert_eq!(installed(&game), "return \"beta one\"\n");
    assert_eq!(fs::read_to_string(game.join("ember.lock")).unwrap(), lock);

    /* a plain install keeps it too. the new dependency makes the install
    resolve again, so this is the resolver and not the up-to-date check */
    let spark = package(&base, "spark", "1.0.0", "spark", "");
    embr(&spark, &base, &["publish", "--local"]);
    embr(&game, &base, &["add", "spark"]);
    embr(&game, &base, &["install"]);
    assert_eq!(
        locked(&game, "acme/fuel")["version"].as_str(),
        Some("1.3.0-beta.1")
    );
    assert_eq!(locked(&game, "acme/fuel")["channel"].as_str(), Some("beta"));
    embr(&game, &base, &["check"]);

    // a dry run of the update shows the move, and the update makes it
    let preview = json(&embr(&game, &base, &["update", "--dry-run", "--json"]));
    let changes = preview["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{preview}");
    assert_eq!(changes[0]["name"], "acme/fuel");
    assert_eq!(changes[0]["from"], "1.3.0-beta.1");
    assert_eq!(changes[0]["to"], "1.3.0-beta.2");
    assert_eq!(changes[0]["channel"], "beta");
    let text = embr(&game, &base, &["update", "--dry-run"]);
    assert!(
        text.contains("acme/fuel 1.3.0-beta.1 → 1.3.0-beta.2") && text.contains("channel beta"),
        "{text}"
    );
    assert_eq!(
        locked(&game, "acme/fuel")["version"].as_str(),
        Some("1.3.0-beta.1")
    );
    embr(&game, &base, &["update"]);
    assert_eq!(
        locked(&game, "acme/fuel")["version"].as_str(),
        Some("1.3.0-beta.2")
    );
    assert_eq!(installed(&game), "return \"beta two\"\n");

    // `embr update fuel` reads the channel again too
    let fuel = package(&base, "fuel", "1.3.0", "beta three", "");
    embr(&fuel, &base, &["publish", "--local", "--channel", "beta"]);
    embr(&game, &base, &["update", "fuel"]);
    assert_eq!(
        locked(&game, "acme/fuel")["version"].as_str(),
        Some("1.3.0-beta.3")
    );

    // a manifest version with a prerelease part goes out as it is
    let fuel = package(&base, "fuel", "2.0.0-rc.7", "rc", "");
    let out = embr(&fuel, &base, &["publish", "--local", "--channel", "rc"]);
    assert!(out.contains("acme/fuel@2.0.0-rc.7"), "{out}");
}

#[test]
fn a_channel_dependency_that_cannot_resolve_says_why() {
    let base = std::env::temp_dir().join("embr-test-channels-errors");
    let _ = fs::remove_dir_all(&base);
    let fuel = package(&base, "fuel", "1.3.0", "beta", "");
    embr(&fuel, &base, &["publish", "--local", "--channel", "beta"]);
    let game = base.join("game");
    let install = |entry: &str| {
        write(
            &game.join("ember.toml"),
            &format!(
                "[target]\nenvironment = \"luau\"\n\n\
                 [indices]\nwally = \"https://github.com/UpliftGames/wally-index\"\n\n\
                 [dependencies]\nfuel = {entry}\n"
            ),
        );
        run(&game, &base, &["install"])
    };

    // a channel that the package does not have lists the ones it has
    let error = fails(
        install(r#"{ name = "acme/fuel", channel = "nightly", index = "local" }"#),
        &["install"],
    );
    assert!(
        error.contains("acme/fuel has no channel 'nightly'; its channels are beta"),
        "{error}"
    );

    // a requirement that does not take the channel version names both
    let error = fails(
        install(r#"{ name = "acme/fuel", version = "^2", channel = "beta", index = "local" }"#),
        &["install"],
    );
    assert!(
        error.contains("'beta'") && error.contains("1.3.0-beta.1") && error.contains("'^2'"),
        "{error}"
    );
    // one that takes the release of the prerelease is good
    ok(
        install(r#"{ name = "acme/fuel", version = "^1.3", channel = "beta", index = "local" }"#),
        &["install"],
    );
    assert_eq!(
        locked(&game, "acme/fuel")["version"].as_str(),
        Some("1.3.0-beta.1")
    );

    // only the ember index and the local registry have channels
    let error = fails(
        install(r#"{ name = "acme/fuel", channel = "beta", index = "wally" }"#),
        &["install"],
    );
    assert!(error.contains("has no channels"), "{error}");

    let error = fails(
        install(r#"{ name = "acme/fuel", channel = "latest", index = "local" }"#),
        &["install"],
    );
    assert!(error.contains("'latest' is not a channel name"), "{error}");
    let error = fails(
        install(r#"{ name = "acme/fuel", index = "local" }"#),
        &["install"],
    );
    assert!(error.contains("has no `version`"), "{error}");
}

#[test]
fn a_published_package_cannot_depend_on_a_channel() {
    let base = std::env::temp_dir().join("embr-test-channels-publish");
    let _ = fs::remove_dir_all(&base);
    let fuel = package(&base, "fuel", "1.3.0", "beta", "");
    embr(&fuel, &base, &["publish", "--local", "--channel", "beta"]);

    for table in ["dependencies", "peer-dependencies"] {
        let rocket = package(
            &base,
            "rocket",
            "1.0.0",
            "rocket",
            &format!(
                "[{table}]\nfuel = {{ name = \"acme/fuel\", channel = \"beta\", index = \"local\" }}\n"
            ),
        );
        let args = ["publish", "--local"];
        let error = fails(run(&rocket, &base, &args), &args);
        assert!(
            error.contains("Dependency 'fuel' uses the channel 'beta'")
                && error.contains(
                    "a published package cannot depend on a channel; give a version requirement"
                ),
            "{table}: {error}"
        );
    }

    // a dev dependency may, and the archive's manifest has no channel
    let rocket = package(
        &base,
        "rocket",
        "1.0.0",
        "rocket",
        "[dev-dependencies]\n\
         fuel = { name = \"acme/fuel\", channel = \"beta\", index = \"local\" }\n\
         spark = { name = \"acme/fuel\", version = \"^1.3\", channel = \"beta\", index = \"local\" }\n",
    );
    embr(&rocket, &base, &["publish", "--local"]);
    let archive =
        fs::read(base.join("home/.ember/local/archives/acme/rocket/1.0.0/luau.tar.gz")).unwrap();
    let packed = packed_manifest(&archive);
    assert!(!packed.contains("channel"), "{packed}");
    assert!(!packed.contains("fuel = "), "{packed}");
    assert!(
        packed.contains("spark") && packed.contains("^1.3"),
        "{packed}"
    );
}

/// one request that the registry of this file got.
struct Request {
    /// the request line and the headers, in lowercase.
    head: String,
    body: Vec<u8>,
}

/** a registry with the routes of the contract for acme/fuel. it has the
versions `versions` and the channel beta, and it refuses a `PUT` of notes
with 500 when `notes_fail` is set. */
fn registry(versions: &[&str], notes_fail: bool) -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let log: Arc<Mutex<Vec<Request>>> = Arc::default();
    let shared = log.clone();
    let versions: Vec<String> = versions
        .iter()
        .map(|version| format!("\"{version} luau\":{{}}"))
        .collect();
    let package = format!(
        r#"{{"name":"acme/fuel","versions":{{{}}},"channels":{{"beta":"1.3.0-beta.4"}}}}"#,
        versions.join(",")
    );
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = Vec::new();
            let mut buffer = [0u8; 65536];
            let (head, body) = loop {
                let read = stream.read(&mut buffer).unwrap_or(0);
                request.extend_from_slice(&buffer[..read]);
                let end = request.windows(4).position(|window| window == b"\r\n\r\n");
                if let Some(end) = end {
                    let head = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length: usize = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map_or(0, |length| length.parse().unwrap());
                    if request.len() >= end + 4 + length {
                        break (head, request[end + 4..].to_vec());
                    }
                }
                if read == 0 {
                    break (String::new(), Vec::new());
                }
            };
            let mut words = head.split(' ');
            let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
            let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
            let (status, answer) = match (method, parts.as_slice()) {
                ("get", ["v1", "packages", "acme", "fuel"]) => ("200 OK", package.clone()),
                ("post", ["v1", "publish"]) => ("200 OK", "{}".to_string()),
                ("put", ["v1", "packages", "acme", "fuel", _, "notes"]) if notes_fail => (
                    "500 Internal Server Error",
                    r#"{"error":"the notes table is away"}"#.to_string(),
                ),
                ("put" | "delete", ["v1", "packages", "acme", "fuel", _, "notes"]) => {
                    ("200 OK", "{}".to_string())
                }
                ("put" | "delete", ["v1", "packages", "acme", "fuel", "channels", _]) => {
                    ("200 OK", "{}".to_string())
                }
                _ => ("404 Not Found", r#"{"error":"not found"}"#.to_string()),
            };
            shared.lock().unwrap().push(Request { head, body });
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                answer.len()
            );
            let _ = stream.write_all(&[head.as_bytes(), answer.as_bytes()].concat());
        }
    });
    (api, log)
}

/// the request lines that the registry got, such as "put /v1/publish".
fn lines(log: &Arc<Mutex<Vec<Request>>>) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .map(|request| {
            let line = request.head.lines().next().unwrap_or("");
            line.trim_end_matches(" http/1.1").to_string()
        })
        .collect()
}

#[test]
fn a_publish_to_a_channel_gets_the_next_version_and_sends_the_notes() {
    let base = std::env::temp_dir().join("embr-test-channels-registry");
    let _ = fs::remove_dir_all(&base);
    let fuel = package(&base, "fuel", "1.3.0", "beta", "");
    let manifest = fs::read_to_string(fuel.join("ember.toml")).unwrap();
    write(&fuel.join("NOTES.md"), "# Beta 5\n\nFaster fuel.\n");
    let published = [
        "1.2.0",
        "1.2.0-beta.9",
        "1.3.0-beta.1",
        "1.3.0-beta.4",
        "1.3.0-rc.7",
    ];

    // a bad channel name or a bad notes file stops before any call
    let (api, log) = registry(&published, false);
    write(&fuel.join("EMPTY.md"), "");
    for (args, says) in [
        (
            vec!["publish", "--channel", "Stable"],
            "'Stable' is not a channel name",
        ),
        (
            vec!["publish", "--channel", "stable"],
            "'stable' is not a channel name",
        ),
        (vec!["publish", "--notes", "EMPTY.md"], "has 0 bytes"),
        (vec!["publish", "--notes", "MISSING.md"], "did not open"),
    ] {
        let error = fails(run_at(&api, &fuel, &base, &args), &args);
        assert!(error.contains(says), "{error}");
    }
    assert!(lines(&log).is_empty(), "{:?}", lines(&log));

    // a dry run reads the versions and shows the one that it would publish
    let args = ["publish", "--channel", "beta", "--dry-run", "--json"];
    let dry = json(&ok(run_at(&api, &fuel, &base, &args), &args));
    assert_eq!(dry["published"][0]["version"], "1.3.0-beta.5");
    assert_eq!(lines(&log), ["get /v1/packages/acme/fuel"]);
    log.lock().unwrap().clear();

    let args = ["publish", "--channel", "beta", "--notes", "NOTES.md"];
    let out = ok(run_at(&api, &fuel, &base, &args), &args);
    assert!(out.contains("Published acme/fuel@1.3.0-beta.5"), "{out}");
    assert!(
        out.contains("The channel beta now names 1.3.0-beta.5"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(fuel.join("ember.toml")).unwrap(),
        manifest
    );
    assert_eq!(
        lines(&log),
        [
            "get /v1/packages/acme/fuel",
            "post /v1/publish",
            "put /v1/packages/acme/fuel/1.3.0-beta.5/notes"
        ]
    );
    {
        let log = log.lock().unwrap();
        assert!(
            log[1].head.contains("x-ember-channel: beta"),
            "{}",
            log[1].head
        );
        assert!(packed_manifest(&log[1].body).contains("version = \"1.3.0-beta.5\""));
        assert!(log[2].head.contains("content-type: text/markdown"));
        assert!(log[2].head.contains("authorization: bearer ember_test"));
        assert_eq!(log[2].body, b"# Beta 5\n\nFaster fuel.\n");
    }

    // a publish with no channel sends no channel header
    log.lock().unwrap().clear();
    ok(run_at(&api, &fuel, &base, &["publish"]), &["publish"]);
    assert_eq!(lines(&log), ["post /v1/publish"]);
    assert!(!log.lock().unwrap()[0].head.contains("x-ember-channel"));

    // notes that fail after the publish say that the version is out
    let (api, log) = registry(&[], true);
    let error = fails(run_at(&api, &fuel, &base, &args), &args);
    assert!(
        error.contains("acme/fuel@1.3.0-beta.1 is published, but its release notes did not upload")
            && error.contains("the notes table is away")
            && error.contains("embr notes acme/fuel@1.3.0-beta.1 NOTES.md"),
        "{error}"
    );
    assert_eq!(lines(&log).len(), 3);
}

#[test]
fn channel_and_notes_commands_call_the_registry() {
    let base = std::env::temp_dir().join("embr-test-channels-commands");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    write(&base.join("NOTES.md"), "Fixes the pump.\n");
    let (api, log) = registry(&["1.3.0-beta.4"], false);
    let embr = |args: &[&str]| ok(run_at(&api, &base, &base, args), args);

    let set = json(&embr(&[
        "channel",
        "set",
        "acme/fuel",
        "beta",
        "1.3.0-beta.4",
        "--json",
    ]));
    assert_eq!(set["name"], "acme/fuel");
    assert_eq!(set["channel"], "beta");
    assert_eq!(set["channel_version"], "1.3.0-beta.4");
    let removed = json(&embr(&["channel", "remove", "acme/fuel", "beta", "--json"]));
    assert_eq!(removed["channel_version"], Value::Null);
    let list = embr(&["channel", "list", "acme/fuel"]);
    assert!(list.starts_with("beta  1.3.0-beta.4\n"), "{list}");
    let list = json(&embr(&["channel", "list", "acme/fuel", "--json"]));
    assert_eq!(list["channels"]["beta"], "1.3.0-beta.4");

    let notes = json(&embr(&[
        "notes",
        "acme/fuel@1.3.0-beta.4",
        "NOTES.md",
        "--json",
    ]));
    assert_eq!(notes["package_version"], "1.3.0-beta.4");
    assert_eq!(notes["removed"], false);
    assert_eq!(notes["bytes"], 16);
    embr(&["notes", "acme/fuel@1.3.0-beta.4", "--remove"]);

    assert_eq!(
        lines(&log),
        [
            "put /v1/packages/acme/fuel/channels/beta",
            "delete /v1/packages/acme/fuel/channels/beta",
            "get /v1/packages/acme/fuel",
            "get /v1/packages/acme/fuel",
            "put /v1/packages/acme/fuel/1.3.0-beta.4/notes",
            "delete /v1/packages/acme/fuel/1.3.0-beta.4/notes",
        ]
    );
    {
        let log = log.lock().unwrap();
        let body: Value = serde_json::from_slice(&log[0].body).unwrap();
        assert_eq!(body, serde_json::json!({ "version": "1.3.0-beta.4" }));
        assert!(log[0].head.contains("authorization: bearer ember_test"));
        assert_eq!(log[4].body, b"Fixes the pump.\n");
    }

    // a bad name or a bad file makes no call
    log.lock().unwrap().clear();
    for args in [
        &["channel", "set", "acme/fuel", "latest", "1.0.0"][..],
        &["channel", "set", "acme/fuel", "beta", "one"],
        &["notes", "acme/fuel", "NOTES.md"],
        &["notes", "acme/fuel@1.0.0", "MISSING.md"],
        &["notes", "acme/fuel@1.0.0"],
    ] {
        fails(run_at(&api, &base, &base, args), args);
    }
    assert!(lines(&log).is_empty(), "{:?}", lines(&log));
}
