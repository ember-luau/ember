/*! What embr does with a package, a lockfile and a cache that somebody made
to do harm. Each test is one attack as its author would run it, with the
real binary in a scratch home, and it fails when the defence goes.

Unix only: the test points HOME at a scratch folder, and on Windows embr
finds the home folder without HOME. */
#![cfg(unix)]

use embr::sys::hash::sha256_hex;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("embr-test-security-{name}"));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("home")).unwrap();
    base
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn command(dir: &Path, base: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_embr"));
    command
        .current_dir(dir)
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .env_remove("EMBER_TOKEN")
        .env_remove("EMBER_INDEX_TTL_SECS");
    command
}

/// runs embr in `dir` with the home folder of `base`.
fn run(dir: &Path, base: &Path, args: &[&str]) -> Output {
    command(dir, base).args(args).output().expect("embr runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// a gzipped tar of `files`, each a path, its bytes and its mode.
fn tar_gz(files: &[(&str, &str, u32)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, contents, mode) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(*mode);
        header.set_cksum();
        builder
            .append_data(&mut header, path, contents.as_bytes())
            .unwrap();
    }
    gzip(&builder.into_inner().unwrap())
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

const URL: &str = "https://registry.invalid/acme/lib/1.0.0.tar.gz";

/// a bundle in `base/bundle` that says `archive` is the archive of `url`.
fn bundle(base: &Path, url: &str, archive: &[u8]) -> PathBuf {
    let dir = base.join("bundle");
    fs::create_dir_all(dir.join("archives")).unwrap();
    fs::write(dir.join("archives/a"), archive).unwrap();
    write(
        &dir.join("bundle.json"),
        &format!(
            r#"{{"version":1,"archives":[{{"url":"{url}","file":"archives/a","sha256":"{}"}}]}}"#,
            sha256_hex(archive)
        ),
    );
    dir
}

/** C1. an Alloy project installs an Alloy package whose alloy.toml names an
ingot, a program that `alloy build` starts. install must not run a build in
the package. the `alloy` on PATH here records each call. */
#[test]
fn install_runs_no_build_in_an_alloy_package() {
    let base = scratch("alloy");
    let calls = base.join("alloy-calls");
    let bin = base.join("bin");
    write(
        &bin.join("alloy"),
        &format!("#!/bin/sh\necho \"$@\" >> {}\n", calls.display()),
    );
    fs::set_permissions(bin.join("alloy"), fs::Permissions::from_mode(0o755)).unwrap();

    let archive = tar_gz(&[
        (
            "ember.toml",
            "[package]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n[target]\n\
             environment = \"luau\"\nlanguage = \"alloy\"\nmain = \"src/init.luau\"\n",
            0o644,
        ),
        ("src/init.luau", "return 1\n", 0o644),
        ("src/init.aly", "local x = 1\n", 0o644),
        (
            "alloy.toml",
            "[build]\nin = \"src\"\nout = \"out\"\n[ingots]\nx = \"ing\"\n",
            0o644,
        ),
        (
            "ing/ingot.toml",
            "name = \"x\"\napi = 1\nbinary = \"run.sh\"\nhooks = [\"transform\"]\n",
            0o644,
        ),
        ("ing/run.sh", "#!/bin/sh\nid\n", 0o755),
    ]);
    let project = base.join("proj");
    write(
        &project.join("ember.toml"),
        "[target]\nenvironment = \"luau\"\nlanguage = \"alloy\"\n",
    );
    write(
        &project.join("ember.lock"),
        &format!(
            "version = 2\n\n[[package]]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n\
             environment = \"luau\"\nlink = \"lib\"\nindex = \"https://example.invalid/idx\"\n\
             type = \"tar-gz\"\nurl = \"{URL}\"\nsha256 = \"{}\"\n",
            sha256_hex(&archive)
        ),
    );
    let bundle = bundle(&base, URL, &archive);
    let imported = run(&project, &base, &["import", bundle.to_str().unwrap()]);
    assert!(imported.status.success(), "{}", text(&imported));

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let installed = command(&project, &base)
        .args(["install", "--locked"])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(installed.status.success(), "{}", text(&installed));
    // the published Luau is in place and is what the link reads
    assert!(
        project
            .join("packages/luau/.ember/acme_lib/src/init.luau")
            .is_file()
    );
    assert!(project.join("packages/luau/lib.luau").is_file());
    let calls = fs::read_to_string(&calls).unwrap_or_default();
    assert!(
        !calls.lines().any(|call| call.starts_with("build")),
        "install ran `alloy {calls}`"
    );
}
