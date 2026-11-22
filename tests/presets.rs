/*! `embr preset` and `embr template`, run as a person runs them. The saved
presets and templates are in a scratch home, so the local commands run
offline. The registry commands talk to a small server in this file, never
to Ember.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// an address where nothing listens. a local command that asks the registry fails there.
const NO_REGISTRY: &str = "http://127.0.0.1:1";

const GAME: &str = "\
# the game
[package]
name = \"acme/game\"
version = \"1.0.0\"

[target]
environment = \"roblox\" # where it runs

[scripts]
# checks the code
lint = \"selene src\"

[tools]
rojo = \"rojo-rbx/rojo@7.4.4\"
";

const OTHER: &str = "\
# another project
[target]
environment = \"lune\" # not roblox

[scripts]
build = \"lune run build\"
";

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// a scratch folder with the project `game` and the project `other`.
fn fixture(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("embr-test-presets-{name}"));
    let _ = fs::remove_dir_all(&base);
    write(&base.join("game/ember.toml"), GAME);
    write(&base.join("game/src/init.luau"), "return {}\n");
    write(&base.join("other/ember.toml"), OTHER);
    fs::create_dir_all(base.join("home")).unwrap();
    base
}

/// runs embr in `dir` with the home folder of `base` and the registry at `api`. stdin is no terminal.
fn run_at(api: &str, base: &Path, dir: &str, args: &[&str]) -> Output {
    fs::create_dir_all(base.join(dir)).unwrap();
    Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(args)
        .current_dir(base.join(dir))
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .env("EMBER_API_URL", api)
        .env("EMBER_TOKEN", "ember_test")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("embr runs")
}

fn run(base: &Path, dir: &str, args: &[&str]) -> Output {
    run_at(NO_REGISTRY, base, dir, args)
}

fn ok(base: &Path, dir: &str, args: &[&str]) -> String {
    let output = run(base, dir, args);
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn manifest(base: &Path, dir: &str) -> String {
    fs::read_to_string(base.join(dir).join("ember.toml")).unwrap()
}

#[test]
fn a_local_preset_goes_from_one_project_to_another() {
    let base = fixture("round-trip");
    let saved = ok(&base, "game", &["preset", "save", "strict"]);
    assert!(saved.contains("Saved preset strict"), "{saved}");
    assert!(saved.contains("Left out: package"), "{saved}");
    let file = fs::read_to_string(base.join("home/.ember/presets/strict.toml")).unwrap();
    // `save` strips the name and the version, and the empty [package] with them
    assert!(saved.contains("package.name, package.version"), "{saved}");
    assert!(!file.contains("[package]"), "{file}");
    assert!(
        file.contains("# checks the code\nlint = \"selene src\""),
        "{file}"
    );

    // a second save needs --force
    let again = run(&base, "game", &["preset", "save", "strict"]);
    assert!(!again.status.success());
    assert!(stderr(&again).contains("--force"), "{again:?}");
    ok(&base, "game", &["preset", "save", "strict", "--force"]);

    let list: serde_json::Value =
        serde_json::from_str(&ok(&base, "game", &["preset", "list", "--json"])).unwrap();
    assert_eq!(list["version"], 1);
    assert_eq!(list["presets"][0]["name"], "strict");
    assert_eq!(
        list["presets"][0]["keys"],
        serde_json::json!(["target", "scripts", "tools"])
    );
    assert!(ok(&base, "game", &["preset", "list"]).contains("strict"));
    assert!(ok(&base, "game", &["preset", "show", "strict"]).contains("rojo-rbx/rojo@7.4.4"));

    let applied = ok(&base, "other", &["preset", "apply", "strict", "--yes"]);
    assert!(applied.contains("add      scripts.lint"), "{applied}");
    assert!(applied.contains("add      tools"), "{applied}");
    assert!(applied.contains("skip     target.environment"), "{applied}");
    assert!(
        applied.contains("2 added, 0 replaced, 1 skipped"),
        "{applied}"
    );
    // the comments and the key order of the project stay, and new keys go last
    assert_eq!(
        manifest(&base, "other"),
        "\
# another project
[target]
environment = \"lune\" # not roblox

[scripts]
build = \"lune run build\"
# checks the code
lint = \"selene src\"

[tools]
rojo = \"rojo-rbx/rojo@7.4.4\"
"
    );

    // --force takes the value of the preset and keeps the comment beside it
    let forced = ok(
        &base,
        "other",
        &["preset", "apply", "strict", "--force", "--yes"],
    );
    assert!(forced.contains("replace  target.environment"), "{forced}");
    assert!(
        manifest(&base, "other").contains("environment = \"roblox\" # not roblox"),
        "{}",
        manifest(&base, "other")
    );
    // nothing is left to change, so a third apply needs no --yes
    let nothing = ok(&base, "other", &["preset", "apply", "strict"]);
    assert!(nothing.contains("Nothing to change"), "{nothing}");

    ok(&base, "game", &["preset", "remove", "strict"]);
    assert!(!base.join("home/.ember/presets/strict.toml").exists());
    let gone = run(&base, "other", &["preset", "apply", "strict", "--yes"]);
    assert!(!gone.status.success());
    assert!(stderr(&gone).contains("No preset is named"), "{gone:?}");
}

#[test]
fn an_apply_writes_nothing_without_a_yes_or_with_a_dry_run() {
    let base = fixture("no-terminal");
    ok(&base, "game", &["preset", "save", "strict"]);

    // no terminal and no --yes: the changes print, the command fails, the file stays
    let refused = run(&base, "other", &["preset", "apply", "strict"]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(stderr(&refused).contains("--yes"), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains("add      scripts.lint"),
        "{refused:?}"
    );
    assert_eq!(manifest(&base, "other"), OTHER);

    for args in [
        &["preset", "apply", "strict", "--dry-run"][..],
        &["preset", "apply", "strict", "--dry-run", "--force", "--yes"],
    ] {
        let dry = ok(&base, "other", args);
        assert!(dry.contains("add      scripts.lint"), "{dry}");
        assert!(dry.contains("Dry run"), "{dry}");
        assert_eq!(manifest(&base, "other"), OTHER);
    }

    // a folder with no ember.toml has nothing to merge into
    let nowhere = run(&base, "empty", &["preset", "apply", "strict", "--yes"]);
    assert!(!nowhere.status.success());
    assert!(!base.join("empty/ember.toml").exists());
}

#[test]
fn the_local_template_flow_is_as_it_was() {
    let base = fixture("template-flow");
    let saved = ok(&base, "game", &["template", "save", "starter"]);
    assert!(
        saved.contains("Saved template starter with 2 files"),
        "{saved}"
    );
    let list: serde_json::Value =
        serde_json::from_str(&ok(&base, "game", &["template", "list", "--json"])).unwrap();
    assert_eq!(list["templates"][0]["name"], "starter");
    let shown = ok(&base, "game", &["template", "show", "starter"]);
    assert!(shown.contains("src/init.luau"), "{shown}");

    // a bare saved name asks no registry
    let created = ok(
        &base,
        "new",
        &["init", "--template", "starter", "--name", "acme/new"],
    );
    assert!(
        created.contains("Created 2 files from template starter"),
        "{created}"
    );
    assert!(base.join("new/src/init.luau").is_file());
    let new = manifest(&base, "new");
    assert!(new.contains("name = \"acme/new\""), "{new}");
    assert!(new.contains("version = \"0.1.0\""), "{new}");
    assert!(new.contains("# checks the code"), "{new}");

    // the GitHub command has the flags that `publish` had. `publish` refuses
    // them and names the new command
    let help = ok(&base, "game", &["template", "github", "--help"]);
    for flag in ["--private", "--repo", "--yes"] {
        assert!(help.contains(flag), "{flag}: {help}");
    }
    let old = run(
        &base,
        "game",
        &["template", "publish", "starter", "--private"],
    );
    assert!(!old.status.success());
    assert!(stderr(&old).contains("embr template github"), "{old:?}");

    ok(&base, "game", &["template", "remove", "starter"]);
    assert!(!base.join("home/.ember/templates/starter").exists());
}

#[test]
fn init_takes_a_template_and_a_preset_together() {
    let base = fixture("init-both");
    ok(&base, "game", &["preset", "save", "strict"]);
    write(
        &base.join("tree/ember.toml"),
        "# from the tree\n[target]\nenvironment = \"lune\"\n\n[scripts]\nbuild = \"lune run build\"\n",
    );
    write(&base.join("tree/src/main.luau"), "print(1)\n");
    ok(&base, "tree", &["template", "save", "tree"]);

    let created = ok(
        &base,
        "new",
        &["init", "--template", "tree", "--preset", "strict"],
    );
    assert!(
        created.contains("Created 2 files from template tree"),
        "{created}"
    );
    assert!(created.contains("Applied preset strict"), "{created}");
    // the tree first, then the preset, whose values win in a new manifest
    assert!(base.join("new/src/main.luau").is_file());
    assert_eq!(
        manifest(&base, "new"),
        "\
# from the tree
[target]
environment = \"roblox\"

[scripts]
build = \"lune run build\"
# checks the code
lint = \"selene src\"

[tools]
rojo = \"rojo-rbx/rojo@7.4.4\"
"
    );

    // a preset alone goes into the manifest that init writes
    ok(&base, "plain", &["init", "--yes", "--preset", "strict"]);
    let plain = manifest(&base, "plain");
    assert!(plain.contains("lint = \"selene src\""), "{plain}");
    assert!(plain.contains("[dependencies]"), "{plain}");

    // a preset that does not exist leaves the folder empty
    let missing = run(
        &base,
        "none",
        &["init", "--template", "tree", "--preset", "nope"],
    );
    assert!(!missing.status.success());
    assert_eq!(fs::read_dir(base.join("none")).unwrap().count(), 0);
}

/// what the registry of this file got, and what it holds.
#[derive(Default)]
struct Registry {
    /// each request: its first line and its headers, in lowercase.
    log: Vec<String>,
    /// the content of each finished version, by "templates/ada/starter", with its sha256 header.
    stored: std::collections::BTreeMap<String, (String, Vec<u8>)>,
}

/** a registry with the routes of the contract, for the name `ada/<any>`
and the version 1.0.0. a `PUT` of the content finishes the version. */
fn registry() -> (String, Arc<Mutex<Registry>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let state: Arc<Mutex<Registry>> = Arc::default();
    let shared = state.clone();
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
            let mut state = shared.lock().unwrap();
            state.log.push(head.clone());
            let mut words = head.split(' ');
            let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
            // "/v1/{kind}/{scope}/{name}[/versions[/1.0.0/{content|download}]]"
            let parts: Vec<&str> = path.split('/').skip(2).collect();
            let key = parts.iter().take(3).copied().collect::<Vec<_>>().join("/");
            let (status, answer): (&str, Vec<u8>) = match (method, parts.as_slice()) {
                ("get", [_, scope, name]) => match state.stored.get(&key) {
                    Some((sha256, content)) => (
                        "200 OK",
                        format!(
                            r#"{{"scope":"{scope}","name":"{name}","description":"d","tags":[],
                            "latest":"1.0.0","downloads":0,"updated_at":1,
                            "versions":[{{"version":"1.0.0","prerelease":false,
                            "size":{},"sha256":"{sha256}"}}]}}"#,
                            content.len()
                        )
                        .into_bytes(),
                    ),
                    None => ("404 Not Found", br#"{"error":"not found"}"#.to_vec()),
                },
                ("post", [_, _, _, "versions"]) => {
                    ("201 Created", br#"{"version":"1.0.0"}"#.to_vec())
                }
                ("put", [_, _, _, "versions", "1.0.0", "content"]) => {
                    let sha256 = head
                        .lines()
                        .find_map(|line| line.strip_prefix("x-ember-sha256: "))
                        .unwrap_or_default()
                        .to_string();
                    state.stored.insert(key, (sha256, body));
                    ("201 Created", b"{}".to_vec())
                }
                ("get", [_, _, _, "versions", "1.0.0", "download"]) => {
                    ("200 OK", state.stored[&key].1.clone())
                }
                _ => ("404 Not Found", br#"{"error":"not found"}"#.to_vec()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                answer.len()
            );
            let _ = stream.write_all(&[head.as_bytes(), &answer].concat());
        }
    });
    (api, state)
}

#[test]
fn a_template_and_a_preset_go_to_a_registry_and_come_back() {
    let base = fixture("registry");
    let (api, state) = registry();
    let embr = |dir: &str, args: &[&str]| {
        let output = run_at(&api, &base, dir, args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    embr("game", &["template", "save", "starter"]);
    embr("game", &["preset", "save", "strict"]);

    // a dry run sends nothing
    embr(
        "game",
        &[
            "template",
            "publish",
            "starter",
            "--as",
            "ada/starter",
            "--description",
            "A start",
            "--dry-run",
        ],
    );
    assert!(state.lock().unwrap().stored.is_empty());

    let published = embr(
        "game",
        &[
            "template",
            "publish",
            "starter",
            "--as",
            "ada/starter",
            "--description",
            "A start",
            "--tag",
            "game",
        ],
    );
    assert!(
        published.contains("Published template ada/starter@1.0.0"),
        "{published}"
    );
    assert!(
        published.contains("embr init --template ember:ada/starter"),
        "{published}"
    );
    let published = embr(
        "game",
        &[
            "preset",
            "publish",
            "strict",
            "--as",
            "ada/strict",
            "--description",
            "Strict",
        ],
    );
    assert!(
        published.contains("Published preset ada/strict@1.0.0"),
        "{published}"
    );
    {
        let state = state.lock().unwrap();
        let sent = |start: &str| {
            let found = state.log.iter().find(|sent| sent.starts_with(start));
            found
                .unwrap_or_else(|| panic!("{start}: {:?}", state.log))
                .clone()
        };
        let draft = sent("post /v1/templates/ada/starter/versions ");
        assert!(
            draft.contains("authorization: bearer ember_test"),
            "{draft}"
        );
        let content = sent("put /v1/templates/ada/starter/versions/1.0.0/content ");
        assert!(
            content.contains("authorization: bearer ember_test"),
            "{content}"
        );
        assert!(content.contains("x-ember-sha256: "), "{content}");
        sent("put /v1/presets/ada/strict/versions/1.0.0/content ");
        // a preset is its TOML text, with no [package]
        let preset = String::from_utf8(state.stored["presets/ada/strict"].1.clone()).unwrap();
        assert!(preset.contains("lint = \"selene src\""), "{preset}");
        assert!(!preset.contains("[package]"), "{preset}");
    }

    /* a saved preset that someone edited by hand: `publish` refuses a
    [workspace] table and the [package] keys of one project, and sends
    nothing */
    for (name, text) in [
        ("named", "[package]\nname = \"acme/game\"\n"),
        (
            "versioned",
            "[package]\nprivate = true\nversion = \"1.0.0\"\n",
        ),
        ("workspace", "[workspace.package]\nversion = \"1.0.0\"\n"),
    ] {
        write(&base.join(format!("home/.ember/presets/{name}.toml")), text);
        let before = state.lock().unwrap().log.len();
        let as_name = format!("ada/{name}");
        let refused = run_at(
            &api,
            &base,
            "game",
            &[
                "preset",
                "publish",
                name,
                "--as",
                &as_name,
                "--description",
                "d",
            ],
        );
        assert!(!refused.status.success(), "{name}: {refused:?}");
        assert!(
            stderr(&refused).contains("cannot hold"),
            "{name}: {refused:?}"
        );
        assert_eq!(state.lock().unwrap().log.len(), before, "{name}");
    }

    // the two of them make a new project, each from the registry
    let created = embr(
        "new",
        &[
            "init",
            "--template",
            "ember:ada/starter",
            "--preset",
            "ember:ada/strict",
            "--name",
            "acme/new",
        ],
    );
    assert!(created.contains("Created 2 files"), "{created}");
    assert!(base.join("new/src/init.luau").is_file());
    assert!(manifest(&base, "new").contains("name = \"acme/new\""));

    // a preset on Ember merges as a saved one does
    embr("other", &["preset", "apply", "ada/strict@1.0.0", "--yes"]);
    assert!(manifest(&base, "other").contains("lint = \"selene src\""));
    assert!(manifest(&base, "other").starts_with("# another project\n"));

    let info: serde_json::Value = serde_json::from_str(&embr(
        "game",
        &["template", "info", "ada/starter", "--json"],
    ))
    .unwrap();
    assert_eq!(info["version"], 1);
    assert_eq!(info["template"]["name"], "starter");
    assert_eq!(info["template"]["versions"][0]["version"], "1.0.0");
    assert!(
        embr("game", &["preset", "info", "ada/strict"])
            .contains("embr preset apply ember:ada/strict")
    );

    // content that changed after the publish is refused, and nothing is written
    state
        .lock()
        .unwrap()
        .stored
        .get_mut("templates/ada/starter")
        .unwrap()
        .1
        .push(0);
    let changed = run_at(
        &api,
        &base,
        "bad",
        &["init", "--template", "ember:ada/starter"],
    );
    assert!(!changed.status.success());
    assert!(stderr(&changed).contains("sha256"), "{changed:?}");
    assert_eq!(fs::read_dir(base.join("bad")).unwrap().count(), 0);
}
