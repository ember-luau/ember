/*! A tool that installs more than one binary, and a tool with one binary
beside it. The tools sit in the tool store of a scratch home, as an install
leaves them, so the test runs offline.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/** a scratch folder with a `game` that pins `acme/multi` and `acme/solo`.
the store of `home` holds the two tools: `multi` with the binary `multi-lsp`
after its main binary, and `solo` with one binary. */
fn fixture(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("embr-test-tool-bins-{name}"));
    let _ = fs::remove_dir_all(&base);
    let write = |path: &str, text: &str| {
        let path = base.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    write(
        "game/ember.toml",
        "[target]\nenvironment = \"luau\"\n\n[tools]\n\
         multi = \"acme/multi@1.0.0\"\nsolo = \"acme/solo@1.0.0\"\n",
    );
    let multi = "home/.ember/tools/acme_multi/1.0.0";
    write(&format!("{multi}/multi"), "#!/bin/sh\necho multi \"$@\"\n");
    write(
        &format!("{multi}/multi-lsp"),
        "#!/bin/sh\necho multi-lsp \"$@\"\n",
    );
    write(&format!("{multi}/.embr-bins"), "multi-lsp\n");
    write(
        &format!("{multi}/.embr-asset"),
        &format!("{SHA}  tool.toml\n{SHA}  multi-linux-x64.zip\n{SHA}  multi-lsp-linux-x64.zip\n"),
    );
    let solo = "home/.ember/tools/acme_solo/1.0.0";
    write(&format!("{solo}/solo"), "#!/bin/sh\necho solo \"$@\"\n");
    write(
        &format!("{solo}/.embr-asset"),
        &format!("{SHA}  solo-linux-x64.zip\n"),
    );
    base
}

/// runs `program` in `dir` with the home folder of `base`.
fn run(program: &Path, dir: &Path, base: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .env_remove("EMBER_TOKEN")
        .env_remove("EMBER_INDEX_TTL_SECS")
        .output()
        .expect("the program runs")
}

/// [`run`] of embr, and fails the test when embr fails.
fn embr(dir: &Path, base: &Path, args: &[&str]) -> String {
    let output = run(Path::new(env!("CARGO_BIN_EXE_embr")), dir, base, args);
    assert!(output.status.success(), "embr {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// the names of the shims in the bin folder of `base`, in order.
fn shims(base: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(base.join("home/.ember/bin"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        // `.swap` is the folder that embr replaces a shim through
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

#[test]
fn an_install_puts_each_binary_of_a_tool_on_the_path() {
    let base = fixture("install");
    let game = base.join("game");
    let stdout = embr(&game, &base, &["install", "--offline"]);
    assert!(stdout.contains("→ multi, multi-lsp"), "{stdout}");
    // a tool with one binary gets one shim, as before
    assert!(stdout.contains("acme/solo@1.0.0 → solo ("), "{stdout}");
    assert_eq!(shims(&base), ["multi", "multi-lsp", "solo"]);

    // each shim runs its own binary at the version of the one [tools] entry
    let bin = base.join("home/.ember/bin");
    for (shim, says) in [
        ("multi", "multi --help\n"),
        ("multi-lsp", "multi-lsp --help\n"),
        ("solo", "solo --help\n"),
    ] {
        let output = run(&bin.join(shim), &game, &base, &["--help"]);
        assert_eq!(String::from_utf8_lossy(&output.stdout), says, "{output:?}");
    }

    // ember.lock pins each asset that the tool came from
    let lock = fs::read_to_string(game.join("ember.lock")).unwrap();
    for asset in [
        "tool.toml",
        "multi-linux-x64.zip",
        "multi-lsp-linux-x64.zip",
        "solo-linux-x64.zip",
    ] {
        assert!(lock.contains(&format!("asset = \"{asset}\"")), "{lock}");
    }

    // a second install has nothing to do, and a lost shim comes back
    let stdout = embr(&game, &base, &["install", "--offline"]);
    assert!(stdout.contains("up to date"), "{stdout}");
    fs::remove_file(bin.join("multi-lsp")).unwrap();
    embr(&game, &base, &["install", "--offline"]);
    assert_eq!(shims(&base), ["multi", "multi-lsp", "solo"]);
}

#[test]
fn the_second_binary_follows_the_entry_of_its_tool() {
    let base = fixture("scope");
    let game = base.join("game");
    embr(&game, &base, &["install", "--offline"]);
    let lsp = base.join("home/.ember/bin/multi-lsp");

    // outside the project no manifest pins the tool
    let output = run(&lsp, &base, &base, &[]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not managed"), "{stderr}");

    // an alias of [tools] wins over the binary of another tool
    let manifest = fs::read_to_string(game.join("ember.toml")).unwrap();
    fs::write(
        game.join("ember.toml"),
        format!("{manifest}multi-lsp = \"acme/solo@1.0.0\"\n"),
    )
    .unwrap();
    let output = run(&lsp, &game, &base, &[]);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "solo\n");
    fs::write(game.join("ember.toml"), manifest).unwrap();

    // `tool remove` takes the entry, and the two commands go with it
    embr(&game, &base, &["tool", "remove", "multi"]);
    assert!(!run(&lsp, &game, &base, &[]).status.success());

    // `tool delete` takes the stored tool and each of its shims
    embr(&game, &base, &["tool", "delete", "acme/multi"]);
    assert_eq!(shims(&base), ["solo"]);
}
