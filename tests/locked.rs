/*! `embr install --locked` and `--offline` as CI runs them, and what an
install does with an archive that nobody packed with embr. A package goes
to a local registry in a scratch home, so the test runs offline.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use embr::project::lockfile::Lockfile;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// a scratch folder with `lib`, a package in the local registry of `home`, and `game`, which depends on it.
fn fixture(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("embr-test-locked-{name}"));
    let _ = fs::remove_dir_all(&base);
    let write = |path: &str, text: &str| {
        let path = base.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "lib/ember.toml",
        "[package]\nname = \"acme/lib\"\nversion = \"1.0.0\"\nlicense = \"MIT\"\n\
         description = \"a library\"\n\n[target]\nenvironment = \"luau\"\nmain = \"src/init.luau\"\n",
    );
    write("lib/src/init.luau", "return 1\n");
    write(
        "game/ember.toml",
        "[target]\nenvironment = \"luau\"\n\n[dependencies]\n\
         lib = { name = \"acme/lib\", version = \"^1\", index = \"local\" }\n",
    );
    embr(&base.join("lib"), &base, &["publish", "--local"]);
    base
}

/// runs embr in `dir` with the home folder of `base`.
fn run(dir: &Path, base: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(args)
        .current_dir(dir)
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .env_remove("EMBER_TOKEN")
        .env_remove("EMBER_INDEX_TTL_SECS")
        .output()
        .expect("embr runs")
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

fn append(path: &Path, text: &str) {
    let mut whole = fs::read_to_string(path).unwrap();
    whole.push_str(text);
    fs::write(path, whole).unwrap();
}

#[test]
fn locked_refuses_a_lock_that_another_manifest_wrote() {
    let base = fixture("drift");
    let game = base.join("game");
    embr(&game, &base, &["install"]);
    let lock_text = fs::read_to_string(game.join("ember.lock")).unwrap();
    assert!(lock_text.contains("manifest-hash = \""), "{lock_text}");

    // the lock of this ember.toml installs, and --locked leaves it as it is
    embr(&game, &base, &["install", "--locked"]);
    assert_eq!(
        fs::read_to_string(game.join("ember.lock")).unwrap(),
        lock_text
    );

    // a table that resolves nothing is no change
    append(&game.join("ember.toml"), "\n[scripts]\nhi = \"echo hi\"\n");
    embr(&game, &base, &["install", "--locked"]);

    // a dependency that ember.toml gained, and that no install has seen
    let manifest = fs::read_to_string(game.join("ember.toml")).unwrap();
    fs::write(
        game.join("ember.toml"),
        manifest.replace(
            "[dependencies]\n",
            "[dependencies]\nmore = { name = \"acme/lib\", version = \"^1\", index = \"local\" }\n",
        ),
    )
    .unwrap();
    let refused = run(&game, &base, &["install", "--locked"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(3), "{refused:?}");
    assert!(
        stderr.contains("ember.toml asks for other dependencies"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(game.join("ember.lock")).unwrap(),
        lock_text
    );

    // an install without --locked writes the lock of the new ember.toml
    embr(&game, &base, &["install"]);
    embr(&game, &base, &["install", "--locked"]);

    // a lock from before the hash: a warning, and the install goes on
    let old: String = fs::read_to_string(game.join("ember.lock"))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("manifest-hash"))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(game.join("ember.lock"), old).unwrap();
    let warned = run(&game, &base, &["install", "--locked"]);
    assert!(warned.status.success(), "{warned:?}");
    assert!(
        String::from_utf8_lossy(&warned.stderr)
            .contains("did not record what ember.toml asked for"),
        "{warned:?}"
    );
    // and the next plain install gives the lock its hash, fast path or not
    embr(&game, &base, &["install"]);
    assert!(
        Lockfile::parse(&fs::read_to_string(game.join("ember.lock")).unwrap())
            .unwrap()
            .manifest_hash
            .is_some()
    );
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn offline_installs_from_what_is_on_this_computer() {
    let base = fixture("offline");
    let game = base.join("game");
    embr(&game, &base, &["install"]);
    fs::remove_dir_all(game.join("packages")).unwrap();
    embr(&game, &base, &["install", "--offline", "--locked"]);
    assert!(game.join("packages/luau/lib.luau").is_file());

    // an index that was never cloned is not something an offline run can get
    append(
        &game.join("ember.toml"),
        "\n[indices]\nelsewhere = \"https://example.invalid/index\"\n\n\
         [dev-dependencies]\nother = { name = \"acme/other\", version = \"^1\", index = \"elsewhere\" }\n",
    );
    let refused = run(&game, &base, &["install", "--offline"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(4), "{refused:?}");
    assert!(
        stderr.contains("the index https://example.invalid/index") && stderr.contains("--offline"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(&base);
}

/** `embr publish` packs no `.git`, but an archive is a tar that anyone can
write. one with a `.git/config` that names an fsmonitor program used to run
that program at `embr patch`, which runs git in the extracted package. */
#[test]
fn a_git_folder_in_an_archive_never_reaches_git() {
    let base = fixture("git-folder");
    let game = base.join("game");
    let archive = base.join("home/.ember/local/archives/acme/lib/1.0.0/luau.tar.gz");
    let marker = base.join("ran");

    // the published archive again, with a hostile `.git` beside its files
    let unpacked = base.join("unpacked");
    fs::create_dir_all(unpacked.join(".git")).unwrap();
    fs::create_dir_all(unpacked.join("src/.git")).unwrap();
    let tar = |args: &[&str]| {
        let status = Command::new("tar")
            .args(args)
            .current_dir(&unpacked)
            .status();
        assert!(status.is_ok_and(|status| status.success()), "tar {args:?}");
    };
    tar(&["xzf", archive.to_str().unwrap()]);
    let config = format!(
        "[core]\n\tfsmonitor = \"touch '{}'; false\"\n",
        marker.display()
    );
    fs::write(unpacked.join(".git/config"), &config).unwrap();
    fs::write(unpacked.join("src/.git/config"), &config).unwrap();
    tar(&["czf", archive.to_str().unwrap(), "."]);

    embr(&game, &base, &["install"]);
    let stored = game.join("packages/luau/.ember/acme_lib");
    assert!(stored.join("src/init.luau").is_file());
    assert!(!stored.join(".git").exists());
    assert!(!stored.join("src/.git").exists());

    // git runs in the working copy with its own fresh repository
    embr(&game, &base, &["patch", "acme/lib"]);
    let copy = game.join(".ember-patch/acme_lib@1.0.0");
    assert!(copy.join(".git").is_dir());
    assert!(!copy.join("src/.git").exists());
    assert!(!marker.exists(), "the package's fsmonitor program ran");
    let _ = fs::remove_dir_all(&base);
}
