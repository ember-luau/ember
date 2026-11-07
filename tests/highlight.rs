/*! `[highlight]` from publish to install, as a person runs them: publish
checks the rules, the archive and the index entry keep them, and a game
installs the package. The packages go to a local registry in a scratch
home, so the test runs offline.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use std::fs;
use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Output};

const RULES: &str = r##"
[[highlight.rules]]
languages = ["alx"]
type = "keyword"
before = '\{'
words = ["#if", ":else", "/if", "#each", "/each"]
"##;

/// runs embr in `dir` with `home`.
fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env_remove("EMBER_TOKEN")
        .env_remove("EMBER_INDEX_TTL_SECS")
        .output()
        .expect("embr runs")
}

/// writes a luau package acme/blocks with `extra` at the end of ember.toml.
fn package(dir: &Path, extra: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/init.luau"), "return { ready = true }\n").unwrap();
    fs::write(
        dir.join("ember.toml"),
        format!(
            "[package]\nname = \"acme/blocks\"\nversion = \"1.0.0\"\n\n\
             [target]\nenvironment = \"luau\"\nmain = \"src/init.luau\"\n{extra}"
        ),
    )
    .unwrap();
}

/// the text of ember.toml in the archive at `path`.
fn packed_manifest(path: &Path) -> String {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]));
    let mut text = String::new();
    archive
        .entries()
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.path().unwrap().to_str() == Some("ember.toml"))
        .expect("the archive holds ember.toml")
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[test]
fn highlight_rules_reach_the_index_and_the_package_installs() {
    let base = std::env::temp_dir().join(format!("embr-highlight-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();

    // publish refuses a rule that it cannot check, before any upload
    let bad = base.join("bad");
    package(&bad, &RULES.replace("words = [", "match = 'a+'\nwords = ["));
    let output = run(&bad, &home, &["publish", "--local", "--dry-run"]);
    assert!(!output.status.success(), "{output:?}");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains(
            "Invalid highlight rules in ember.toml: [highlight] rule 1: has `match` and `words`"
        ),
        "{error}"
    );
    package(
        &bad,
        "[[highlight.rules]]\nlanguages = [\"alx\"]\ntype = \"keyword\"\nmatch = '(?:#if|a+)'\n",
    );
    let error =
        String::from_utf8_lossy(&run(&bad, &home, &["publish", "--local"]).stderr).to_string();
    assert!(
        error.contains("rule 1: `match` has `+`, a repeat with no upper bound"),
        "{error}"
    );

    // a good table goes into the archive and into the index entry
    let blocks = base.join("blocks");
    package(&blocks, RULES);
    let output = run(&blocks, &home, &["publish", "--local"]);
    assert!(output.status.success(), "{output:?}");
    let packed = packed_manifest(&home.join(".ember/local/archives/acme/blocks/1.0.0/luau.tar.gz"));
    let packed: toml::Table = toml::from_str(&packed).unwrap();
    let rules = &packed["highlight"]["rules"][0];
    assert_eq!(rules["before"].as_str(), Some("\\{"));
    assert_eq!(rules["words"].as_array().unwrap().len(), 5);
    let index = fs::read_to_string(home.join(".ember/local/index/acme/blocks")).unwrap();
    let index: toml::Table = toml::from_str(&index).unwrap();
    assert_eq!(
        index["1.0.0 luau"]["highlight"], packed["highlight"],
        "{index:#?}"
    );

    // a game installs it as it installs any package
    let game = base.join("game");
    fs::create_dir_all(&game).unwrap();
    fs::write(
        game.join("ember.toml"),
        "[target]\nenvironment = \"luau\"\n\n[dependencies]\nblocks = { name = \"acme/blocks\", version = \"^1.0.0\", index = \"local\" }\n",
    )
    .unwrap();
    let output = run(&game, &home, &["install"]);
    assert!(output.status.success(), "{output:?}");
    assert!(game.join("packages/luau/blocks.luau").is_file());
    let _ = fs::remove_dir_all(&base);
}
