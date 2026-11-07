/*! File features from publish to install, as a person runs them. The
fixtures in `tests/fixtures/file-features` are acme/forge, a core module
with the file features `traits` (which turns on `bin`), `bin` and
`lifecycles`, acme/kit, which depends on forge, and a game that asks forge
for `traits`. The packages go to a local registry in a scratch home, so the
test runs offline.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use embr::project::lockfile::Lockfile;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/file-features")
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

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

/// [`run`], and fails the test when embr fails.
fn embr(dir: &Path, home: &Path, args: &[&str]) -> String {
    let output = run(dir, home, args);
    assert!(
        output.status.success(),
        "embr {args:?} in {}: {output:?}",
        dir.display()
    );
    String::from_utf8(output.stdout).unwrap()
}

fn features_of(game: &Path, name: &str) -> Vec<String> {
    let lock = Lockfile::parse(&fs::read_to_string(game.join("ember.lock")).unwrap()).unwrap();
    let package = lock.packages.iter().find(|p| p.name == name).unwrap();
    package.features.clone()
}

/// the paths of the files in the archive at `path`.
fn archive_files(path: &Path) -> Vec<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]));
    archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().display().to_string())
        .collect()
}

#[test]
fn file_features_install_link_and_show() {
    let base = std::env::temp_dir().join(format!("embr-file-features-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    copy(&fixtures(), &base);
    // --dry-run names the entry and the files of each feature
    let dry = embr(
        &base.join("forge"),
        &home,
        &["publish", "--local", "--dry-run"],
    );
    assert!(
        dry.contains(
            "Feature traits: entry src/traits/init.luau, 2 files:\n  src/traits/Trait.luau\n  src/traits/init.luau\n"
        ),
        "{dry}"
    );
    assert!(
        dry.contains("Feature bin: entry src/bin/init.luau, 1 file:\n"),
        "{dry}"
    );
    for package in ["forge", "kit"] {
        embr(&base.join(package), &home, &["publish", "--local"]);
    }
    let game = base.join("game");
    embr(&game, &home, &["install"]);

    // the store holds the core and the enabled features, never lifecycles
    let store = game.join("packages/luau/.ember/acme_forge");
    for kept in [
        "ember.toml",
        "src/init.luau",
        "src/traits/init.luau",
        "src/traits/Trait.luau",
        "src/bin/init.luau",
        "src/hooks/init.luau",
    ] {
        assert!(store.join(kept).is_file(), "{kept}");
    }
    assert!(!store.join("src/lifecycles").exists());
    // the archive keeps every file
    let archive = home.join(".ember/local/archives/acme/forge/1.0.0/luau.tar.gz");
    assert!(
        archive_files(&archive).contains(&"src/lifecycles/init.luau".to_string()),
        "{:?}",
        archive_files(&archive)
    );
    // traits turns bin on, and the lock records each enabled feature
    assert_eq!(features_of(&game, "acme/forge"), ["bin", "hooks", "traits"]);

    // one link file joins the main module and each enabled entry
    let link = fs::read_to_string(game.join("packages/luau/forge.luau")).unwrap();
    for line in [
        "local module = require(\"./.ember/acme_forge/src\")\n",
        "local feature_bin = require(\"./.ember/acme_forge/src/bin\")\n",
        "local feature_traits = require(\"./.ember/acme_forge/src/traits\")\n",
        // hooks returns nil, so it loads but adds no fields and no type
        "local feature_hooks = require(\"./.ember/acme_forge/src/hooks\")\n",
        "export type World = module.World\n",
        "export type Trait = feature_traits.Trait\n",
        "return (merged :: any) :: typeof(module) & typeof(feature_bin) & typeof(feature_traits)\n",
    ] {
        assert!(link.contains(line), "{line}\n{link}");
    }
    assert!(!link.contains("lifecycles"), "{link}");
    // a package that depends on forge gets the same parts in its nested link
    let nested =
        fs::read_to_string(game.join("packages/luau/.ember/acme_kit/packages/luau/forge.luau"))
            .unwrap();
    assert!(
        nested.contains("local feature_traits = require(\"../../../acme_forge/src/traits\")"),
        "{nested}"
    );
    // the game reaches every enabled part through one require
    match Command::new("luau")
        .arg("main.luau")
        .current_dir(&game)
        .output()
    {
        Ok(output) => assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "one\ttwo\tbin\tbin\tnil\t1\n",
            "{output:?}"
        ),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }

    // the commands show file features as features
    let tree = embr(&game, &home, &["tree"]);
    assert!(
        tree.contains("acme/forge@1.0.0 [bin, hooks, traits]"),
        "{tree}"
    );
    let info = embr(&game, &home, &["info", "acme/forge", "--index", "local"]);
    assert!(
        info.contains("traits      bin (files: src/traits; entry: src/traits)"),
        "{info}"
    );
    let json: serde_json::Value = serde_json::from_str(&embr(
        &game,
        &home,
        &["info", "acme/forge", "--index", "local", "--json"],
    ))
    .unwrap();
    assert_eq!(
        json["shown"]["feature_files"]["lifecycles"],
        serde_json::json!({ "entry": "src/lifecycles", "files": ["src/lifecycles/**"] })
    );

    // add turns lifecycles on, and the next install brings its files back
    embr(
        &game,
        &home,
        &[
            "add",
            "acme/forge",
            "--index",
            "local",
            "--features",
            "traits,hooks,lifecycles",
        ],
    );
    embr(&game, &home, &["install"]);
    assert!(store.join("src/lifecycles/init.luau").is_file());
    let link = fs::read_to_string(game.join("packages/luau/forge.luau")).unwrap();
    assert!(
        link.contains("local feature_lifecycles = require("),
        "{link}"
    );
    assert_eq!(
        features_of(&game, "acme/forge"),
        ["bin", "hooks", "lifecycles", "traits"]
    );
    let _ = fs::remove_dir_all(&base);
}

/** publish refuses a `[feature-files]` that would make install delete a
file that the package needs, also with --dry-run. */
#[test]
fn publish_refuses_a_feature_that_owns_the_main_entry() {
    let base = std::env::temp_dir().join(format!("embr-file-features-bad-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    let forge = base.join("forge");
    copy(&fixtures().join("forge"), &forge);
    let manifest = fs::read_to_string(forge.join("ember.toml")).unwrap();
    fs::write(
        forge.join("ember.toml"),
        manifest.replace(
            "files = [\"src/bin\"]",
            "files = [\"src/bin\", \"src/init.luau\"]",
        ),
    )
    .unwrap();
    let output = run(&forge, &home, &["publish", "--local", "--dry-run"]);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .contains("feature-files.bin owns the main entry src/init.luau, which always installs"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(&base);
}
