/*! `embr doctor`, `embr cache`, `embr pack`, `embr export` and `embr
import`, and the index marks of `embr tree` and `embr outdated`, as a
person runs them. A package goes to a local registry in a scratch home, so
the tests make no request.

Unix only: the tests point HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// a scratch folder with `lib`, a package in the local registry of `home`, and `game`, which depends on it.
fn fixture(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("embr-test-tools-{name}"));
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
        .env_remove("EMBER_API_URL")
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

fn json(dir: &Path, base: &Path, args: &[&str]) -> Value {
    serde_json::from_str(&embr(dir, base, args)).unwrap()
}

/// each file under `dir` with its bytes, sorted, to compare a folder before and after a command.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    files.sort();
    files
}

/** moves the lock of `game` from the local registry to a URL that no
server answers, with the hash of the archive, as a lock of a registry
package has it. the archive then comes from the archive cache or not at
all. returns the URL. */
fn lock_to_url(base: &Path) -> String {
    let game = base.join("game");
    let archive = base.join("home/.ember/local/archives/acme/lib/1.0.0/luau.tar.gz");
    let sha256 = embr::sys::hash::sha256_hex(&fs::read(archive).unwrap());
    // a closed port of this computer: a request fails at once, with no name lookup
    let url = "http://127.0.0.1:1/acme/lib/1.0.0/luau.tar.gz";
    let lock = fs::read_to_string(game.join("ember.lock")).unwrap();
    let local = "url = \"local:acme/lib/1.0.0/luau.tar.gz\"\n";
    assert!(lock.contains(local), "{lock}");
    fs::write(
        game.join("ember.lock"),
        lock.replace(local, &format!("url = \"{url}\"\nsha256 = \"{sha256}\"\n")),
    )
    .unwrap();
    url.to_string()
}

#[test]
fn pack_writes_the_bytes_that_publish_stores() {
    let base = fixture("pack");
    let lib = base.join("lib");
    let published =
        fs::read(base.join("home/.ember/local/archives/acme/lib/1.0.0/luau.tar.gz")).unwrap();

    embr(&lib, &base, &["pack"]);
    let packed = lib.join("lib-1.0.0-luau.tar.gz");
    assert_eq!(fs::read(&packed).unwrap(), published);

    // the file of the first pack is no part of the second
    let out = base.join("out.tar.gz");
    let document = json(
        &lib,
        &base,
        &["pack", "--json", "-o", out.to_str().unwrap()],
    );
    assert_eq!(fs::read(&out).unwrap(), published);
    let archive = &document["archives"][0];
    assert_eq!(document["version"], 1);
    assert_eq!(archive["name"], "acme/lib");
    assert_eq!(archive["target"], "luau");
    assert_eq!(archive["bytes"], published.len());
    assert_eq!(
        archive["sha256"],
        embr::sys::hash::sha256_hex(&published).as_str()
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn an_export_installs_offline_after_an_import() {
    let base = fixture("bundle");
    let game = base.join("game");
    let bundle = base.join("bundle");
    let home = base.join("home/.ember");
    embr(&game, &base, &["install"]);

    // the local registry: the archive goes back where the lock names it
    embr(&game, &base, &["export", bundle.to_str().unwrap()]);
    assert!(bundle.join("ember.lock").is_file());
    fs::remove_dir_all(home.join("local")).unwrap();
    fs::remove_dir_all(game.join("packages")).unwrap();
    let missing = run(&game, &base, &["install", "--offline", "--locked"]);
    assert!(!missing.status.success(), "{missing:?}");
    embr(&game, &base, &["import", bundle.to_str().unwrap()]);
    embr(&game, &base, &["install", "--offline", "--locked"]);
    assert!(game.join("packages").is_dir());

    /* a registry URL: the archive cache holds the archive. the first import
    fills the cache from a bundle whose URL is the one in the lock */
    let url = lock_to_url(&base);
    let manifest = bundle.join("bundle.json");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        text.replace("local:acme/lib/1.0.0/luau.tar.gz", &url),
    )
    .unwrap();
    embr(&game, &base, &["import", bundle.to_str().unwrap()]);
    fs::remove_dir_all(home.join("local")).unwrap();
    embr(&game, &base, &["install", "--offline", "--locked"]);

    // the round trip: export from the cache, wipe the cache, import, install
    let second = base.join("second");
    let document = json(
        &game,
        &base,
        &["export", second.to_str().unwrap(), "--json"],
    );
    assert_eq!(document["archives"], 1);
    let listed: Value =
        serde_json::from_str(&fs::read_to_string(second.join("bundle.json")).unwrap()).unwrap();
    assert_eq!(listed["archives"][0]["url"], url.as_str());
    fs::remove_dir_all(home.join("archive-cache")).unwrap();
    fs::remove_dir_all(game.join("packages")).unwrap();
    let missing = run(&game, &base, &["install", "--offline", "--locked"]);
    assert!(!missing.status.success(), "{missing:?}");
    let document = json(
        &game,
        &base,
        &["import", second.to_str().unwrap(), "--json"],
    );
    assert_eq!(document["imported"], 1);
    embr(&game, &base, &["install", "--offline", "--locked"]);
    assert!(game.join("packages").is_dir());

    let _ = fs::remove_dir_all(&base);
}

/// `game` installed, its lock moved to a URL, and the archive of that URL in the archive cache.
fn cached(name: &str) -> (PathBuf, String) {
    let base = fixture(name);
    let game = base.join("game");
    let bundle = base.join("bundle");
    embr(&game, &base, &["install"]);
    embr(&game, &base, &["export", bundle.to_str().unwrap()]);
    let url = lock_to_url(&base);
    let manifest = bundle.join("bundle.json");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        text.replace("local:acme/lib/1.0.0/luau.tar.gz", &url),
    )
    .unwrap();
    embr(&game, &base, &["import", bundle.to_str().unwrap()]);
    (base, url)
}

#[test]
fn export_and_import_check_each_hash() {
    let (base, _) = cached("bundle-checks");
    let game = base.join("game");
    let bundle = base.join("bundle");
    let cache = base.join("home/.ember/archive-cache");
    let before = snapshot(&cache);
    assert_eq!(before.len(), 3, "an archive, its .meta and its .sha256");

    // import: an archive that differs from bundle.json stays out of the cache
    let manifest = bundle.join("bundle.json");
    let listed: Value = serde_json::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
    let file = bundle.join(listed["archives"][0]["file"].as_str().unwrap());
    let good = fs::read(&file).unwrap();
    fs::remove_dir_all(&cache).unwrap();
    fs::write(&file, b"another archive").unwrap();
    let refused = run(&game, &base, &["import", bundle.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(stderr.contains("SHA-256"), "{stderr}");
    assert!(snapshot(&cache).is_empty());
    fs::write(&file, &good).unwrap();

    // import: a path that leaves the bundle folder is refused before a read
    fs::write(base.join("outside.tar.gz"), &good).unwrap();
    let text = fs::read_to_string(&manifest).unwrap();
    let name = listed["archives"][0]["file"].as_str().unwrap();
    fs::write(&manifest, text.replace(name, "../outside.tar.gz")).unwrap();
    let refused = run(&game, &base, &["import", bundle.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(stderr.contains("leaves the bundle folder"), "{stderr}");
    assert!(snapshot(&cache).is_empty());
    fs::write(&manifest, &text).unwrap();
    embr(&game, &base, &["import", bundle.to_str().unwrap()]);
    assert_eq!(snapshot(&cache), before);

    // export: an archive with another hash than the lock names is not copied
    let lock = fs::read_to_string(game.join("ember.lock")).unwrap();
    let sha256 = embr::sys::hash::sha256_hex(&good);
    fs::write(
        game.join("ember.lock"),
        lock.replace(&sha256, &"0".repeat(64)),
    )
    .unwrap();
    let second = base.join("second");
    let refused = run(&game, &base, &["export", second.to_str().unwrap()]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(snapshot(&second).is_empty());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn cache_verify_reports_and_a_dry_run_deletes_nothing() {
    let (base, url) = cached("cache");
    let game = base.join("game");
    let cache = base.join("home/.ember/archive-cache");

    let info = json(&game, &base, &["cache", "info", "--json"]);
    let archives = &info["caches"][0];
    assert_eq!(archives["name"], "archives");
    assert_eq!(archives["entries"], 1);
    assert!(archives["bytes"].as_u64().unwrap() > 0);
    assert_eq!(info["bytes"], archives["bytes"]);

    let verified = json(&game, &base, &["cache", "verify", "--json"]);
    assert_eq!(verified["checked"], 1);
    assert_eq!(verified["without_sha256"], 0);
    assert_eq!(verified["bad"], serde_json::json!([]));

    // a changed archive is reported, with exit code 1, and stays where it is
    let archive = snapshot(&cache)
        .into_iter()
        .map(|(path, _)| path)
        .find(|path| {
            path.extension()
                .is_none_or(|e| e != "meta" && e != "sha256")
        })
        .unwrap();
    fs::write(&archive, b"changed").unwrap();
    let before = snapshot(&cache);
    let bad = run(&game, &base, &["cache", "verify", "--json"]);
    assert_eq!(bad.status.code(), Some(1), "{bad:?}");
    let report: Value = serde_json::from_slice(&bad.stdout).unwrap();
    assert_eq!(report["bad"][0]["url"], url.as_str());
    assert_eq!(report["bad"][0]["path"], archive.to_str().unwrap());
    let bad = run(&game, &base, &["cache", "verify"]);
    assert_eq!(bad.status.code(), Some(1), "{bad:?}");
    assert!(String::from_utf8_lossy(&bad.stdout).contains("embr cache clean --bad"));
    assert_eq!(snapshot(&cache), before);

    // each dry run lists the entry and deletes nothing
    for flags in [
        &["--dry-run"][..],
        &["--dry-run", "--bad"],
        &["--dry-run", "--archives"],
        &["--dry-run", "--older-than", "0"],
    ] {
        let args = [&["cache", "clean", "--json"], flags].concat();
        let plan = json(&game, &base, &args);
        assert_eq!(plan["dry_run"], true, "{flags:?}");
        assert!(plan["bytes"].as_u64().unwrap() > 0, "{flags:?}: {plan}");
        assert_eq!(snapshot(&cache), before, "{flags:?}");
    }
    let text = embr(&game, &base, &["cache", "clean", "--dry-run", "--bad"]);
    assert!(text.contains("Would remove 1 entry"), "{text}");
    // a selector that names another cache, or an age that nothing has, takes nothing
    for flags in [&["--indexes"][..], &["--git"], &["--older-than", "30"]] {
        let args = [&["cache", "clean", "--json"], flags].concat();
        let plan = json(&game, &base, &args);
        assert_eq!(plan["removed"], serde_json::json!([]), "{flags:?}");
        assert_eq!(snapshot(&cache), before, "{flags:?}");
    }

    // --bad deletes the bad entry with its sidecars
    embr(&game, &base, &["cache", "clean", "--bad"]);
    assert!(snapshot(&cache).is_empty());
    assert!(cache.is_dir(), "only the entry goes");

    // no selector deletes each cache, as it always did
    embr(
        &game,
        &base,
        &["import", base.join("bundle").to_str().unwrap()],
    );
    let text = embr(&game, &base, &["cache", "clean"]);
    assert!(
        text.contains("Removed the archive, index, git and shorthand caches"),
        "{text}"
    );
    assert!(!cache.exists());
    assert!(embr(&game, &base, &["cache", "clean"]).contains("Nothing cached"));

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn doctor_reports_and_prints_no_token() {
    let base = fixture("doctor");
    let game = base.join("game");

    // before an install: a warning with a fix, and exit code 0
    let report = json(&game, &base, &["doctor", "--offline", "--json"]);
    assert_eq!(report["version"], 1);
    let checks = report["checks"].as_array().unwrap();
    let check = |name: &str| {
        checks
            .iter()
            .find(|check| check["name"] == name)
            .unwrap_or_else(|| panic!("no check named {name}"))
    };
    assert_eq!(check("manifest")["status"], "ok");
    assert_eq!(check("lockfile")["status"], "warning");
    assert_eq!(check("lockfile")["fix"], "Run `embr install`");
    assert_eq!(check("api")["status"], "skipped");
    assert_eq!(check("login")["status"], "skipped");
    for group in ["Ember", "Environment", "Project", "Registry", "Cache"] {
        assert!(
            checks.iter().any(|check| check["group"] == group),
            "{group}"
        );
    }
    for check in checks {
        let needs_fix = check["status"] == "warning" || check["status"] == "failure";
        assert_eq!(check["fix"].is_string(), needs_fix, "{check}");
        assert_ne!(check["status"], "failure", "{check}");
    }

    embr(&game, &base, &["install"]);
    let report = json(&game, &base, &["doctor", "--offline", "--json"]);
    let status = |name: &str| {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == name)
            .unwrap()["status"]
            .clone()
    };
    assert_eq!(status("lockfile"), "ok");
    assert_eq!(status("install"), "ok");

    // no project is no failure
    let report = json(&base, &base, &["doctor", "--offline", "--json"]);
    let manifest = &report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "manifest")
        .unwrap()["status"];
    assert_eq!(manifest, "skipped");

    /* an API that does not answer is a failure with exit code 1, and the
    token shows in no stream. the API is a closed port of this computer */
    let online = Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(["doctor"])
        .current_dir(&game)
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .env("EMBER_TOKEN", "ember_secret-token-value")
        .env("EMBER_API_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&online.stdout),
        String::from_utf8_lossy(&online.stderr)
    );
    assert_eq!(online.status.code(), Some(1), "{text}");
    assert!(text.contains("fix: "), "{text}");
    assert!(!text.contains("secret-token-value"), "{text}");

    // a manifest that does not parse is a failure with a fix
    fs::write(game.join("ember.toml"), "[target\n").unwrap();
    let broken = run(&game, &base, &["doctor", "--offline", "--json"]);
    assert_eq!(broken.status.code(), Some(1), "{broken:?}");
    let report: Value = serde_json::from_slice(&broken.stdout).unwrap();
    let manifest = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "manifest")
        .unwrap();
    assert_eq!(manifest["status"], "failure");
    assert!(manifest["fix"].is_string());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn tree_and_outdated_mark_what_the_index_marks() {
    let base = fixture("marks");
    let game = base.join("game");
    embr(&game, &base, &["install"]);

    let tree = json(&game, &base, &["tree", "--json"]);
    let node = &tree["dependencies"][0];
    assert_eq!(node["latest"], "1.0.0");
    assert_eq!(node["deprecated"], false);
    assert_eq!(node["yanked"], false);
    let text = embr(&game, &base, &["tree"]);
    assert!(
        !text.contains("deprecated") && !text.contains("yanked"),
        "{text}"
    );

    // the index marks the package as deprecated and the version as yanked
    let index = base.join("home/.ember/local/index/acme/lib");
    let mut entries: toml::Table = fs::read_to_string(&index).unwrap().parse().unwrap();
    let entry = entries["1.0.0 luau"].as_table_mut().unwrap();
    entry.insert("yanked".into(), true.into());
    let mut meta = toml::Table::new();
    meta.insert("deprecated".into(), "use acme/other".into());
    entries.insert("meta".into(), meta.into());
    fs::write(&index, toml::to_string_pretty(&entries).unwrap()).unwrap();

    let tree = json(&game, &base, &["tree", "--json"]);
    let node = &tree["dependencies"][0];
    assert_eq!(node["deprecated"], true);
    assert_eq!(node["yanked"], true);
    // the one version is yanked, so the index has no version to move to
    assert_eq!(node["latest"], Value::Null);
    let text = embr(&game, &base, &["tree"]);
    assert!(
        text.contains("acme/lib@1.0.0 (deprecated) (yanked)"),
        "{text}"
    );

    let outdated = json(&game, &base, &["outdated", "--json"]);
    let row = &outdated["dependencies"][0];
    assert_eq!(row["deprecated"], true);
    assert_eq!(row["yanked"], true);
    let text = embr(&game, &base, &["outdated"]);
    assert!(text.contains("deprecated  yanked"), "{text}");

    let _ = fs::remove_dir_all(&base);
}
