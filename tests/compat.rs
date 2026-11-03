/*! The projects that released versions of embr wrote. Each folder in
`tests/fixtures/compat` holds an ember.toml and the ember.lock that one
release wrote for it, unchanged. embr 1.x reads all of them.

The test publishes the packages of `registry` to a local registry in a
scratch home, so it runs offline. acme/fuel 0.2.1 is there too, which the
requirement `^0.2` takes, so an install that moves a locked version
shows.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use embr::project::lockfile::Lockfile;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// the releases that wrote a fixture, `embr-<version>`.
const RELEASES: [&str; 4] = ["0.10.0", "0.11.0", "0.12.0", "0.13.0"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compat")
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

/// runs embr in `dir` with `home`, and fails the test when embr fails.
fn embr(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("EMBER_TOKEN")
        .env_remove("EMBER_INDEX_TTL_SECS")
        .output()
        .expect("embr runs");
    assert!(
        output.status.success(),
        "embr {args:?} in {}: {output:?}",
        dir.display()
    );
    output
}

/// what the lock in `dir` pins: (name, environment root) -> version.
fn pins(dir: &Path) -> BTreeMap<(String, String), String> {
    let text = fs::read_to_string(dir.join("ember.lock")).unwrap();
    let lock = Lockfile::parse(&text).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    lock.packages
        .iter()
        .map(|package| {
            let root = package.context().to_string();
            ((package.name.clone(), root), package.version.clone())
        })
        .collect()
}

#[test]
fn locks_from_released_versions_install_unchanged() {
    for release in RELEASES {
        let base =
            std::env::temp_dir().join(format!("embr-compat-{release}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let home = base.join("home");
        fs::create_dir_all(&home).unwrap();
        for package in fs::read_dir(fixtures().join("registry")).unwrap() {
            let package = package.unwrap().path();
            let dir = base.join("registry").join(package.file_name().unwrap());
            copy(&package, &dir);
            embr(&dir, &home, &["publish", "--local"]);
        }
        let game = base.join("game");
        copy(&fixtures().join(format!("embr-{release}")), &game);
        let locked = pins(&game);
        assert_eq!(
            locked[&("acme/fuel".into(), "roblox_server".into())],
            "0.2.0"
        );

        /* --locked installs each pinned version, also with acme/fuel 0.2.1
        out, and leaves the lock as it is */
        let output = embr(&game, &home, &["install", "--locked"]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        for ((name, _), version) in &locked {
            assert!(
                stdout.contains(&format!("{name}@{version} →")),
                "{release}: {stdout}"
            );
        }
        assert!(!stdout.contains("0.2.1"), "{release}: {stdout}");
        assert_eq!(
            pins(&game),
            locked,
            "--locked changed the lock of {release}"
        );
        // the lock's patch record applies again
        let rocket = game.join("packages/roblox/.ember/acme_rocket/src/init.luau");
        assert!(fs::read_to_string(rocket).unwrap().contains("patched"));
        // the line names the folder: roblox_server installs to packages/server
        assert!(
            stdout.contains("acme/signal@2.0.0 → server/signal"),
            "{stdout}"
        );

        /* a plain install reads the old lock too, writes it again in the
        current format, and keeps each locked version that ember.toml takes */
        embr(&game, &home, &["install"]);
        assert_eq!(pins(&game), locked, "install moved a version of {release}");
        // --update moves the package that it names, and nothing else
        embr(&game, &home, &["install", "--update", "acme/fuel"]);
        let moved = pins(&game);
        let fuel = ("acme/fuel".to_string(), "roblox_server".to_string());
        assert_eq!(moved[&fuel], "0.2.1", "{release}");
        let rest = |pins: &BTreeMap<_, _>| {
            let mut pins = pins.clone();
            pins.remove(&fuel);
            pins
        };
        assert_eq!(rest(&moved), rest(&locked), "{release}");
        let _ = fs::remove_dir_all(&base);
    }
}
