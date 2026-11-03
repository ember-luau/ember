/*! The `--json` output of the commands that only read, run as a person
runs them. stdout must hold one JSON document and nothing else: no colour,
no spinner, no "Done in" line. The exit code is the one of the text mode.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("a file has a folder")).unwrap();
    fs::write(path, text).unwrap();
}

/** a home with a local index and a saved template, and in `game` a
project that installed acme/rocket, which needs acme/fuel. the index has
a security notice on fuel 0.2.0. */
fn fixture(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("embr-json-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let index = base.join("home/.ember/local/index/acme");
    write(
        &index.join("fuel"),
        "[\"0.2.0 luau\"]\ndownload = \"local:acme/fuel/0.2.0/luau.tar.gz\"\n\
         name = \"acme/fuel\"\ntarget = \"luau\"\nversion = \"0.2.0\"\n\n\
         [[meta.notices]]\nid = \"aaaaaaaaaaaa\"\nlevel = \"security\"\n\
         message = \"0.2.0 sends the place file away\"\nversions = \"=0.2.0\"\n\
         posted_by = \"acme\"\nposted_at = \"2026-11-03T12:00:00Z\"\n",
    );
    write(
        &index.join("rocket"),
        "[\"1.0.0 luau\"]\ndownload = \"local:acme/rocket/1.0.0/luau.tar.gz\"\n\
         name = \"acme/rocket\"\ntarget = \"luau\"\nversion = \"1.0.0\"\n",
    );
    let template = base.join("home/.ember/templates/starter");
    write(&template.join("files/src/init.luau"), "return {}\n");
    write(
        &template.join("template.toml"),
        "source = \"/work/starter\"\n",
    );

    let game = base.join("game");
    write(
        &game.join("ember.toml"),
        "[target]\nenvironment = \"luau\"\n\n[dependencies]\n\
         rocket = { name = \"acme/rocket\", version = \"^1\", index = \"local\" }\n",
    );
    write(
        &game.join("ember.lock"),
        "version = 2\n\n\
         [[package]]\nname = \"acme/fuel\"\nversion = \"0.2.0\"\nenvironment = \"luau\"\n\
         index = \"local\"\ntype = \"tar-gz\"\nurl = \"local:acme/fuel/0.2.0/luau.tar.gz\"\n\n\
         [[package]]\nname = \"acme/rocket\"\nversion = \"1.0.0\"\nenvironment = \"luau\"\n\
         link = \"rocket\"\nindex = \"local\"\ntype = \"tar-gz\"\n\
         url = \"local:acme/rocket/1.0.0/luau.tar.gz\"\n",
    );
    let store = game.join("packages/luau/.ember");
    write(
        &store.join("acme_rocket/ember.toml"),
        "[dependencies]\nfuel = { name = \"acme/fuel\", version = \"^0.2\", index = \"local\" }\n",
    );
    write(&store.join("acme_fuel/src/init.luau"), "return {}\n");
    base
}

fn embr(base: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(args)
        .current_dir(base.join("game"))
        .env("HOME", base.join("home"))
        .env_remove("NO_COLOR")
        .env_remove("EMBER_TOKEN")
        .output()
        .expect("embr runs")
}

/// the one JSON document of a run, once stdout is known to hold nothing else.
fn document(output: &Output) -> Value {
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains('\x1b'),
        "an escape reached stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("Done in"),
        "the time line reached stdout:\n{stdout}"
    );
    let document: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("{error}\nstdout:\n{stdout}\nstderr:\n{stderr}"));
    assert_eq!(document["version"], 1, "{document}");
    document
}

#[test]
fn each_read_only_command_prints_one_json_document() {
    let base = fixture("each");
    let run = |args: &[&str]| {
        let output = embr(&base, args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        document(&output)
    };

    let tree = run(&["tree", "--json"]);
    assert_eq!(tree["dependencies"][0]["name"], "acme/rocket");
    assert_eq!(
        tree["dependencies"][0]["dependencies"][0]["name"],
        "acme/fuel"
    );
    let why = run(&["why", "acme/fuel", "--json"]);
    assert_eq!(why["matches"][0]["paths"][0]["path"][1]["alias"], "fuel");
    let notices = run(&["notices", "--json"]);
    assert_eq!(notices["packages"][0]["items"][0]["kind"], "security");
    assert_eq!(notices["packages"][0]["brought_in_by"][0], "rocket");
    let outdated = run(&["outdated", "--json"]);
    assert_eq!(outdated["dependencies"][0]["current"], "1.0.0");
    assert_eq!(outdated["dependencies"][0]["behind"], false);
    let info = run(&["info", "acme/fuel", "--index", "local", "--json"]);
    assert_eq!(info["shown"]["version"], "0.2.0");
    assert_eq!(info["notices"][0]["level"], "security");
    let tools = run(&["tool", "list", "--json"]);
    assert_eq!(tools["tools"], serde_json::json!([]));
    let templates = run(&["template", "list", "--json"]);
    assert_eq!(templates["templates"][0]["name"], "starter");
    assert_eq!(templates["templates"][0]["source"], "/work/starter");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn audit_json_keeps_the_exit_code_of_a_finding() {
    let base = fixture("audit");
    for args in [
        &["audit", "--json"][..],
        &["notices", "--json", "--deny", "security"],
    ] {
        let output = embr(&base, args);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        let found = document(&output);
        assert_eq!(found["packages"][0]["name"], "acme/fuel");
        assert_eq!(found["packages"][0]["items"][0]["kind"], "security");
        // the reason goes to stderr
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("1 security notice applies"), "{stderr}");
    }
    let _ = fs::remove_dir_all(&base);
}

/** the checks above would see colour and the time line: the text mode
prints both to stdout, also into a pipe. */
#[test]
fn the_text_mode_paints_stdout() {
    let base = fixture("text");
    let output = embr(&base, &["notices"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains('\x1b'), "{stdout}");
    assert!(stdout.contains("Done in"), "{stdout}");
    let _ = fs::remove_dir_all(&base);
}
