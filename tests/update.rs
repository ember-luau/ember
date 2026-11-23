/*! `embr update`, `embr check`, the `--json` output of the commands that
write, the exit codes, and what an install leaves when a download fails.
The packages go to a local registry in a scratch home, so the test runs
offline.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// publishes `name` at `version` to the local registry of `base`, with the lines `dependencies`.
fn publish(base: &Path, name: &str, version: &str, dependencies: &str) {
    let dir = base.join("work").join(name);
    let _ = fs::remove_dir_all(&dir);
    write(
        &dir.join("ember.toml"),
        &format!(
            "[package]\nname = \"acme/{name}\"\nversion = \"{version}\"\nlicense = \"MIT\"\n\
             description = \"a library\"\n\n[target]\nenvironment = \"luau\"\n\
             main = \"src/init.luau\"\n\n[dependencies]\n{dependencies}"
        ),
    );
    write(
        &dir.join("src/init.luau"),
        &format!("return \"{version}\"\n"),
    );
    embr(&dir, base, &["publish", "--local"]);
}

/** a scratch folder with acme/fuel 1.0.0 and acme/rocket 1.0.0, which needs
fuel, in the local registry of `home`, and `game`, which depends on rocket
and has it installed. */
fn fixture(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("embr-test-update-{name}"));
    let _ = fs::remove_dir_all(&base);
    publish(&base, "fuel", "1.0.0", "");
    publish(
        &base,
        "rocket",
        "1.0.0",
        "fuel = { name = \"acme/fuel\", version = \"^1\", index = \"local\" }\n",
    );
    write(
        &base.join("game/ember.toml"),
        "[target]\nenvironment = \"luau\"\n\n[dependencies]\n\
         rocket = { name = \"acme/rocket\", version = \"^1\", index = \"local\" }\n",
    );
    embr(&base.join("game"), &base, &["install"]);
    base
}

/// runs embr in `dir` with the home folder of `base` and the variables `env`.
fn run_with(dir: &Path, base: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(args)
        .current_dir(dir)
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .env_remove("EMBER_TOKEN")
        .env_remove("EMBER_INDEX_TTL_SECS")
        .env_remove("EMBER_OFFLINE")
        .env_remove("CI")
        .envs(env.iter().copied())
        .output()
        .expect("embr runs")
}

fn run(dir: &Path, base: &Path, args: &[&str]) -> Output {
    run_with(dir, base, args, &[])
}

/// [`run`], and fails the test when embr fails.
fn embr(dir: &Path, base: &Path, args: &[&str]) -> String {
    let output = run(dir, base, args);
    assert!(
        output.status.success(),
        "embr {args:?} in {}: {output:?}",
        dir.display()
    );
    String::from_utf8(output.stdout).unwrap()
}

/// [`embr`], and stdout must be one JSON document.
fn json(dir: &Path, base: &Path, args: &[&str]) -> Value {
    let stdout = embr(dir, base, args);
    serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{args:?}: {error}\n{stdout}"))
}

/// each file and folder under `dir`: its bytes, and the time of its last write.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Option<Vec<u8>>, SystemTime)> {
    let mut found = BTreeMap::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let modified = fs::symlink_metadata(&path).unwrap().modified().unwrap();
            let bytes = if path.is_dir() {
                folders.push(path.clone());
                None
            } else {
                Some(fs::read(&path).unwrap())
            };
            found.insert(path, (bytes, modified));
        }
    }
    found
}

#[test]
fn a_dry_run_writes_nothing_and_lists_what_the_update_then_does() {
    let base = fixture("dry-run");
    let game = base.join("game");
    publish(&base, "fuel", "1.2.0", "");
    publish(
        &base,
        "rocket",
        "1.0.1",
        "fuel = { name = \"acme/fuel\", version = \"^1\", index = \"local\" }\n",
    );
    // a version that `^1` does not take
    publish(&base, "rocket", "2.0.0", "");

    let before = (snapshot(&game), snapshot(&base.join("home")));
    let preview = json(&game, &base, &["update", "--dry-run", "--json"]);
    let text = embr(&game, &base, &["update", "--dry-run"]);
    assert_eq!(
        before,
        (snapshot(&game), snapshot(&base.join("home"))),
        "a dry run changed a file"
    );

    assert_eq!(preview["version"], 1);
    assert_eq!(preview["dry_run"], true);
    assert_eq!(
        preview["changes"],
        serde_json::json!([
            {"name": "acme/fuel", "alias": null, "from": "1.0.0", "to": "1.2.0",
             "change": "minor", "direct": false, "environment": "luau"},
            {"name": "acme/rocket", "alias": "rocket", "from": "1.0.0", "to": "1.0.1",
             "change": "patch", "direct": true, "environment": "luau"},
        ])
    );
    assert_eq!(preview["unchanged"], 0);
    assert!(
        text.contains("acme/fuel 1.0.0 → 1.2.0  minor, transitive")
            && text.contains("acme/rocket 1.0.0 → 1.0.1  patch, direct")
            && text.contains("2 lockfile entries would change"),
        "{text}"
    );

    // the update makes the change that the dry run listed
    let done = json(&game, &base, &["update", "--json"]);
    assert_eq!(done["dry_run"], false);
    assert_eq!(done["changes"], preview["changes"]);
    assert_eq!(done["unchanged"], preview["unchanged"]);
    let lock = fs::read_to_string(game.join("ember.lock")).unwrap();
    assert!(
        lock.contains("version = \"1.2.0\"") && lock.contains("version = \"1.0.1\""),
        "{lock}"
    );
    assert_eq!(
        fs::read_to_string(game.join("packages/luau/.ember/acme_fuel/src/init.luau")).unwrap(),
        "return \"1.2.0\"\n"
    );

    // and then nothing is left to move
    let again = json(&game, &base, &["update", "--dry-run", "--json"]);
    assert_eq!(again["changes"], serde_json::json!([]));
    assert_eq!(again["unchanged"], 2);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn update_moves_only_the_package_that_it_names() {
    let base = fixture("named");
    let game = base.join("game");
    publish(&base, "fuel", "1.2.0", "");
    publish(
        &base,
        "rocket",
        "1.0.1",
        "fuel = { name = \"acme/fuel\", version = \"^1\", index = \"local\" }\n",
    );

    // by its alias in ember.toml, and the other package stays where the lock has it
    let preview = json(&game, &base, &["update", "rocket", "--dry-run", "--json"]);
    assert_eq!(preview["changes"].as_array().unwrap().len(), 1, "{preview}");
    assert_eq!(preview["changes"][0]["name"], "acme/rocket");
    assert_eq!(preview["unchanged"], 1);
    let done = json(&game, &base, &["update", "acme/rocket", "--json"]);
    assert_eq!(done["changes"], preview["changes"]);

    let unknown = run(&game, &base, &["update", "nobody"]);
    assert_eq!(unknown.status.code(), Some(1), "{unknown:?}");
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("ember.lock has no package 'nobody'"),
        "{unknown:?}"
    );
    let _ = fs::remove_dir_all(&base);
}

/// ember.lock of `game` with one more package, whose archive is at `url`.
fn lock_with(game: &Path, name: &str, url: &str) {
    let mut lock = fs::read_to_string(game.join("ember.lock")).unwrap();
    lock.push_str(&format!(
        "\n[[package]]\nname = \"{name}\"\nversion = \"1.0.0\"\nenvironment = \"luau\"\n\
         index = \"https://example.invalid/index\"\ntype = \"tar-gz\"\nurl = \"{url}\"\n"
    ));
    fs::write(game.join("ember.lock"), lock).unwrap();
}

#[test]
fn a_download_that_fails_leaves_the_last_install() {
    let base = fixture("keeps");
    let game = base.join("game");
    // nothing listens on port 1, so the download fails at once
    lock_with(&game, "acme/far", "http://127.0.0.1:1/far.tar.gz");
    let before = snapshot(&game);

    let failed = run(&game, &base, &["install", "--locked"]);
    assert_eq!(failed.status.code(), Some(4), "{failed:?}");
    assert_eq!(before, snapshot(&game), "{failed:?}");
    assert!(game.join("packages/luau/rocket.luau").is_file());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn an_offline_install_lists_each_download_before_it_changes_a_file() {
    let base = fixture("offline-list");
    let game = base.join("game");
    lock_with(&game, "acme/far", "https://example.invalid/far.tar.gz");
    lock_with(&game, "acme/near", "https://example.invalid/near.tar.gz");
    let before = snapshot(&game);

    // the flag in front of the command, behind it, and the variable
    for (args, env) in [
        (&["--offline", "install", "--locked"][..], &[][..]),
        (&["install", "--locked", "--offline"], &[]),
        (&["install", "--locked"], &[("EMBER_OFFLINE", "1")]),
    ] {
        let refused = run_with(&game, &base, args, env);
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert_eq!(refused.status.code(), Some(4), "{refused:?}");
        assert!(
            stderr.contains("the archive of acme/far@1.0.0")
                && stderr.contains("the archive of acme/near@1.0.0"),
            "{stderr}"
        );
        assert_eq!(before, snapshot(&game), "{refused:?}");
    }

    // EMBER_OFFLINE=0 is off: the install goes for the download, and that fails
    let online = run_with(
        &game,
        &base,
        &["install", "--locked"],
        &[("EMBER_OFFLINE", "0")],
    );
    assert!(
        !String::from_utf8_lossy(&online.stderr).contains("embr is offline"),
        "{online:?}"
    );

    // a command that only works with the network stops before it starts
    let search = run(&game, &base, &["search", "rocket", "--offline"]);
    assert_eq!(search.status.code(), Some(4), "{search:?}");
    assert!(
        String::from_utf8_lossy(&search.stderr).contains("`embr search` reads the registry"),
        "{search:?}"
    );
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn check_reads_the_two_files_and_exits_3_on_a_lock_that_does_not_match() {
    let base = fixture("check");
    let game = base.join("game");
    let before = snapshot(&game);
    assert!(embr(&game, &base, &["check"]).contains("ember.lock matches it"));
    assert_eq!(
        json(&game, &base, &["check", "--json"]),
        serde_json::json!({"version": 1, "ok": true, "problems": []})
    );
    assert_eq!(before, snapshot(&game));

    // a dependency that no install has seen
    let manifest = fs::read_to_string(game.join("ember.toml")).unwrap();
    write(
        &game.join("ember.toml"),
        &format!(
            "{manifest}fuel = {{ name = \"acme/fuel\", version = \"^1\", index = \"local\" }}\n"
        ),
    );
    let drift = run(&game, &base, &["check"]);
    assert_eq!(drift.status.code(), Some(3), "{drift:?}");
    assert!(
        String::from_utf8_lossy(&drift.stderr).contains("ember.toml asks for other dependencies"),
        "{drift:?}"
    );
    let drift = run(&game, &base, &["check", "--json"]);
    assert_eq!(drift.status.code(), Some(3), "{drift:?}");
    let document: Value = serde_json::from_slice(&drift.stdout).unwrap();
    assert_eq!(document["ok"], false);
    assert_eq!(document["problems"].as_array().unwrap().len(), 1);

    fs::remove_file(game.join("ember.lock")).unwrap();
    let missing = run(&game, &base, &["check"]);
    assert_eq!(missing.status.code(), Some(3), "{missing:?}");

    // an ember.toml that does not load is a plain failure
    write(&game.join("ember.toml"), "[target\n");
    assert_eq!(run(&game, &base, &["check"]).status.code(), Some(1));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn the_commands_that_write_print_one_json_document() {
    let base = fixture("json");
    let game = base.join("game");

    let added = json(
        &game,
        &base,
        &["add", "acme/fuel", "--index", "local", "--json"],
    );
    assert_eq!(
        added,
        serde_json::json!({"version": 1, "added": [{
            "name": "acme/fuel", "alias": "fuel", "version": "1.0.0",
            "requirement": "^", "table": "dependencies",
        }]})
    );

    // fuel was in the lock, and now ember.toml names it
    let installed = json(&game, &base, &["install", "--json"]);
    assert_eq!(installed["version"], 1);
    assert_eq!(installed["changes"], serde_json::json!([]));
    assert_eq!(installed["unchanged"], 2);
    assert!(game.join("packages/luau/fuel.luau").is_file());

    let removed = json(&game, &base, &["remove", "rocket", "--json"]);
    assert_eq!(
        removed,
        serde_json::json!({"version": 1, "removed": [{
            "name": "acme/rocket", "alias": "rocket", "table": "dependencies",
        }]})
    );
    let installed = json(&game, &base, &["install", "--json"]);
    assert_eq!(
        installed["changes"],
        serde_json::json!([{
            "name": "acme/rocket", "alias": "rocket", "from": "1.0.0", "to": null,
            "change": "removed", "direct": true, "environment": "luau",
        }])
    );

    let published = json(
        &base.join("work/fuel"),
        &base,
        &["publish", "--local", "--json"],
    );
    assert_eq!(published["local"], true);
    assert_eq!(published["dry_run"], false);
    assert_eq!(published["published"][0]["name"], "acme/fuel");
    assert_eq!(published["published"][0]["version"], "1.0.0");
    assert_eq!(published["published"][0]["target"], "luau");
    assert!(published["published"][0]["bytes"].as_u64().unwrap() > 0);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn ci_answers_no_question() {
    let base = fixture("ci");
    let game = base.join("game");
    for args in [
        &["index", "add"][..],
        &["index", "add", "mine"],
        &["studio", "init"],
    ] {
        let refused = run_with(&game, &base, args, &[("CI", "true")]);
        assert_eq!(refused.status.code(), Some(1), "{refused:?}");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("no terminal can answer"),
            "{refused:?}"
        );
    }
    // with each answer on the command line, no question is left
    embr(
        &game,
        &base,
        &["index", "add", "mine", "https://example.invalid/index"],
    );
    let _ = fs::remove_dir_all(&base);
}
