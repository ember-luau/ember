/*! `embr add` with several packages and with a whole scope. The packages
go to a local registry in a scratch home, so the test runs offline.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const GAME: &str = "[target]\nenvironment = \"luau\"\n";

/// a scratch folder with four packages in the local registry of `home`, and `game`, which has no dependencies.
fn fixture(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("embr-test-add-{name}"));
    let _ = fs::remove_dir_all(&base);
    let write = |path: &str, text: &str| {
        let path = base.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    for package in ["acme/maid", "acme/signal", "acme/promise", "beta/signal"] {
        let dir = package.replace('/', "-");
        write(
            &format!("{dir}/ember.toml"),
            &format!(
                "[package]\nname = \"{package}\"\nversion = \"1.2.0\"\nlicense = \"MIT\"\n\
                 description = \"a library\"\n\n[target]\nenvironment = \"luau\"\nmain = \"src/init.luau\"\n"
            ),
        );
        write(&format!("{dir}/src/init.luau"), "return 1\n");
        let output = run(&base.join(dir), &base, &["publish", "--local"]);
        assert!(output.status.success(), "publish {package}: {output:?}");
    }
    write("game/ember.toml", GAME);
    base
}

/// runs embr in `dir` with the home folder of `base`. stdin is no terminal.
fn run(dir: &Path, base: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(args)
        .current_dir(dir)
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .env_remove("EMBER_TOKEN")
        .env_remove("EMBER_INDEX_TTL_SECS")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("embr runs")
}

/// `embr add <args> --index local` in the game. gives the output and the manifest after it.
fn add(base: &Path, args: &[&str]) -> (Output, String) {
    let game = base.join("game");
    let args: Vec<&str> = ["add"]
        .into_iter()
        .chain(args.iter().copied())
        .chain(["--index", "local"])
        .collect();
    let output = run(&game, base, &args);
    (output, fs::read_to_string(game.join("ember.toml")).unwrap())
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn several_packages_go_in_with_one_command() {
    let base = fixture("several");
    let (output, manifest) = add(&base, &["acme/maid", "acme/signal@^1.1", "acme/maid"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        manifest.contains(r#"maid = { name = "acme/maid", version = "^", index = "local" }"#),
        "{manifest}"
    );
    assert!(
        manifest
            .contains(r#"signal = { name = "acme/signal", version = "^1.1", index = "local" }"#),
        "{manifest}"
    );
    assert_eq!(manifest.matches("acme/maid").count(), 1, "{manifest}");
}

#[test]
fn one_failure_leaves_the_manifest_alone_and_every_failure_is_listed() {
    let base = fixture("failures");
    let (output, manifest) = add(&base, &["acme/maid", "acme/nope", "acme/signal@^9"]);
    assert!(!output.status.success());
    assert_eq!(manifest, GAME);
    let stderr = stderr(&output);
    assert!(stderr.contains("2 packages failed"), "{stderr}");
    assert!(stderr.contains("acme/nope"), "{stderr}");
    assert!(stderr.contains("acme/signal"), "{stderr}");
}

#[test]
fn a_scope_adds_its_packages_and_skips_the_ones_already_there() {
    let base = fixture("scope");

    // no terminal can answer the question, so the run needs --yes
    let (output, manifest) = add(&base, &["acme/"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--yes"), "{}", stderr(&output));
    assert_eq!(manifest, GAME);

    let (output, _) = add(&base, &["acme/maid@=1.2.0"]);
    assert!(output.status.success(), "{output:?}");

    let (output, manifest) = add(&base, &["acme/", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("has 2 packages to add"), "{stdout}");
    // the scope does not rewrite an entry of the table
    assert!(manifest.contains(r#"version = "=1.2.0""#), "{manifest}");
    for name in ["acme/promise", "acme/signal"] {
        assert!(manifest.contains(name), "{manifest}");
    }
    assert!(!manifest.contains("beta/signal"), "{manifest}");

    let (output, after) = add(&base, &["acme/*", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(after, manifest);

    let (output, _) = add(&base, &["nobody/", "--yes"]);
    assert!(stderr(&output).contains("has no packages"), "{output:?}");
}

#[test]
fn two_packages_with_one_short_name_need_scoped() {
    let base = fixture("clash");
    let (output, manifest) = add(&base, &["acme/signal", "beta/signal"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--scoped"), "{}", stderr(&output));
    assert_eq!(manifest, GAME);

    let (output, manifest) = add(&base, &["acme/signal", "beta/signal", "--scoped"]);
    assert!(output.status.success(), "{output:?}");
    assert!(manifest.contains(r#""acme/signal" = "#), "{manifest}");
    assert!(manifest.contains(r#""beta/signal" = "#), "{manifest}");
}

#[test]
fn alias_and_version_take_one_package() {
    let base = fixture("one-only");
    for flag in [["--alias", "x"], ["--version", "^1"]] {
        let (output, manifest) = add(&base, &["acme/maid", "acme/signal", flag[0], flag[1]]);
        assert!(!output.status.success());
        assert!(stderr(&output).contains(flag[0]), "{}", stderr(&output));
        assert_eq!(manifest, GAME);
    }
    let (output, _) = add(&base, &["acme/maid@^1", "--version", "^1"]);
    assert!(
        stderr(&output).contains("give one of the two"),
        "{output:?}"
    );

    // one package takes both, as before
    let (output, manifest) = add(&base, &["acme/maid", "--alias", "m", "--version", "^1.2"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        manifest.contains(r#"m = { name = "acme/maid", version = "^1.2""#),
        "{manifest}"
    );
}
