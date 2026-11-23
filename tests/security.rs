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

const MANIFEST: &str = "[package]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n\
                        [target]\nenvironment = \"luau\"\nmain = \"src/init.luau\"\n";

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

/** installs `archive` as acme/lib in the project `base/proj` with the
target `target`: the lock names the archive, an import puts it in the
cache, and a locked install unpacks it. nothing reads the network. */
fn install_archive(base: &Path, target: &str, archive: &[u8]) -> Output {
    let project = base.join("proj");
    let environment = target
        .split('"')
        .nth(1)
        .expect("the target names its environment");
    write(&project.join("ember.toml"), &format!("[target]\n{target}"));
    write(
        &project.join("ember.lock"),
        &format!(
            "version = 2\n\n[[package]]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n\
             environment = \"{environment}\"\nlink = \"lib\"\nindex = \"https://example.invalid/idx\"\n\
             type = \"tar-gz\"\nurl = \"{URL}\"\nsha256 = \"{}\"\n",
            sha256_hex(archive)
        ),
    );
    let bundle = bundle(base, URL, archive);
    let imported = run(&project, base, &["import", bundle.to_str().unwrap()]);
    assert!(imported.status.success(), "{}", text(&imported));
    run(&project, base, &["install", "--locked"])
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

/** H1. a path package names itself, and the name is its folder in the
store. on Windows `\` is a separator, so this name is a path that leaves
the project. */
#[test]
fn a_package_name_that_is_a_path_is_refused() {
    let base = scratch("name");
    write(
        &base.join("evil/ember.toml"),
        "[package]\nname = 'a\\..\\..\\..\\..\\x'\nversion = \"1.0.0\"\n\
         [target]\nenvironment = \"luau\"\nmain = \"src/init.luau\"\n",
    );
    write(&base.join("evil/src/init.luau"), "return 1\n");
    let project = base.join("proj");
    write(
        &project.join("ember.toml"),
        "[target]\nenvironment = \"luau\"\n[dependencies]\ndep = { path = \"../evil\" }\n",
    );
    let installed = run(&project, &base, &["install"]);
    assert!(!installed.status.success(), "{}", text(&installed));
    assert!(
        text(&installed).contains("can't name a folder"),
        "{}",
        text(&installed)
    );
    assert!(!project.join("ember.lock").exists());

    // the same name in a lock that a pull request changed
    write(
        &project.join("ember.lock"),
        "version = 2\n\n[[package]]\nname = 'a\\..\\..\\..\\..\\x'\nversion = \"1.0.0\"\n\
         environment = \"luau\"\nlink = \"dep\"\nindex = \"git\"\ntype = \"git\"\n\
         url = \"https://git.invalid/evil\"\nrev = \"0123456789abcdef0123456789abcdef01234567\"\n",
    );
    let locked = run(&project, &base, &["install", "--locked"]);
    assert!(!locked.status.success(), "{}", text(&locked));
    assert!(text(&locked).contains("package name"), "{}", text(&locked));
    assert!(!project.join("packages").exists());
}

/** H2. `embr patch` makes a folder from the name and the version in
ember.lock, and `--force` deletes the folder that is there. */
#[test]
fn a_lock_version_that_is_a_path_is_refused() {
    let base = scratch("version");
    let victim = base.join("X");
    write(&victim.join("keep.txt"), "keep\n");
    let project = base.join("a/b/proj");
    write(
        &project.join("ember.toml"),
        "[target]\nenvironment = \"luau\"\n",
    );
    write(
        &project.join("ember.lock"),
        &format!(
            "version = 2\n\n[[package]]\nname = \"acme/lib\"\nversion = \"1.0.0/../../../../X\"\n\
             environment = \"luau\"\nlink = \"lib\"\nindex = \"https://example.invalid/idx\"\n\
             type = \"tar-gz\"\nurl = \"{URL}\"\nsha256 = \"{}\"\n",
            sha256_hex(b"x")
        ),
    );
    let patched = run(&project, &base, &["patch", "acme/lib", "--force"]);
    assert!(!patched.status.success(), "{}", text(&patched));
    assert!(
        text(&patched).contains("which is not a version"),
        "{}",
        text(&patched)
    );
    assert_eq!(
        fs::read_to_string(victim.join("keep.txt")).unwrap(),
        "keep\n"
    );
}

/** M2. a package states its own entry. install reads the module there and
writes it back when it rewrites instance requires, so an entry with `..`
changed a file of the project. */
#[test]
fn an_entry_outside_the_package_is_not_read_or_written() {
    let base = scratch("entry");
    let outside = "local Foo = require(script.Parent.Foo)\nreturn 1\n";
    write(&base.join("proj/packages/x.luau"), outside);
    write(&base.join("proj/packages/Foo.luau"), "return 1\n");
    let archive = tar_gz(&[
        (
            "ember.toml",
            "[package]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n[target]\n\
             environment = \"roblox\"\nmain = \"../../../x\"\n",
            0o644,
        ),
        ("src/init.luau", "return 1\n", 0o644),
    ]);
    let installed = install_archive(&base, "environment = \"roblox\"\n", &archive);
    assert!(installed.status.success(), "{}", text(&installed));
    assert!(
        text(&installed).contains("which is outside the package"),
        "{}",
        text(&installed)
    );
    assert_eq!(
        fs::read_to_string(base.join("proj/packages/x.luau")).unwrap(),
        outside
    );
    // no entry, so no link file that requires a path outside the package
    assert!(!base.join("proj/packages/roblox/lib.luau").exists());
}

/** L1. the temp file of ember.lock had the pid in its name. a link of that
name sent the write to the file it points at, and ember.lock became the
link. */
#[test]
fn the_lock_is_not_written_through_a_link() {
    let base = scratch("locktmp");
    let target = base.join("target.txt");
    write(&target, "ORIGINAL\n");
    let project = base.join("proj");
    write(
        &project.join("ember.toml"),
        "[target]\nenvironment = \"luau\"\n",
    );
    // the shell makes the link under its own pid, then becomes embr
    let installed = Command::new("sh")
        .arg("-c")
        .arg("ln -s \"$TARGET\" ember.lock.tmp-$$; exec \"$EMBR\" install")
        .env("TARGET", &target)
        .env("EMBR", env!("CARGO_BIN_EXE_embr"))
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(installed.status.success(), "{}", text(&installed));
    assert_eq!(fs::read_to_string(&target).unwrap(), "ORIGINAL\n");
    let lock = fs::symlink_metadata(project.join("ember.lock")).unwrap();
    assert!(lock.file_type().is_file(), "ember.lock is a link");
}
