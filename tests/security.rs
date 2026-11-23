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

/** M1. bundle.json says which URL its archive is for. an import that
believes it fills the cache slot of any URL, and an install with no hash to
compare then takes the archive. */
#[test]
fn an_import_takes_only_what_the_lock_of_the_folder_names() {
    let base = scratch("import");
    let archive = tar_gz(&[
        ("ember.toml", MANIFEST, 0o644),
        ("src/init.luau", "return \"ATTACKER CODE\"\n", 0o644),
    ]);
    let bundle = bundle(&base, URL, &archive);
    let cache = base.join("home/.ember/archive-cache");
    let victim = base.join("victim");
    fs::create_dir_all(&victim).unwrap();

    // no lock in the folder
    let imported = run(&victim, &base, &["import", bundle.to_str().unwrap()]);
    assert!(!imported.status.success(), "{}", text(&imported));
    assert!(
        text(&imported).contains("run it in the project folder that has the lockfile"),
        "{}",
        text(&imported)
    );
    assert!(!cache.exists(), "the archive went into the cache");

    // a lock that names the URL with another hash, and the copy of the lock in the bundle
    let lock = |sha256: &str| {
        format!(
            "version = 2\n\n[[package]]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n\
             environment = \"luau\"\nlink = \"lib\"\nindex = \"https://example.invalid/idx\"\n\
             type = \"tar-gz\"\nurl = \"{URL}\"\nsha256 = \"{sha256}\"\n"
        )
    };
    write(&bundle.join("ember.lock"), &lock(&sha256_hex(&archive)));
    write(
        &victim.join("ember.toml"),
        "[target]\nenvironment = \"luau\"\n",
    );
    write(
        &victim.join("ember.lock"),
        &lock(&sha256_hex(b"the real one")),
    );
    let imported = run(&victim, &base, &["import", bundle.to_str().unwrap()]);
    assert!(!imported.status.success(), "{}", text(&imported));
    assert!(
        text(&imported).contains("does not name"),
        "{}",
        text(&imported)
    );
    assert!(!cache.exists(), "the archive went into the cache");

    // the lock of the folder names the pair
    write(&victim.join("ember.lock"), &lock(&sha256_hex(&archive)));
    let imported = run(&victim, &base, &["import", bundle.to_str().unwrap()]);
    assert!(imported.status.success(), "{}", text(&imported));
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

/// L2. `embr cache clean` deleted the files of a folder that the cache folder links to.
#[test]
fn cache_clean_does_not_follow_a_linked_cache_folder() {
    let base = scratch("cacheclean");
    let docs = base.join("docs");
    write(&docs.join("thesis.txt"), "a\n");
    write(&docs.join("old/notes.txt"), "a\n");
    let ember = base.join("home/.ember");
    fs::create_dir_all(ember.join("archive-cache")).unwrap();
    std::os::unix::fs::symlink(&docs, ember.join("archive-cache/v1")).unwrap();
    std::os::unix::fs::symlink(&docs, ember.join("index-cache")).unwrap();

    for args in [
        &["cache", "clean", "--bad"][..],
        &["cache", "clean", "--older-than", "0"],
        &["cache", "clean", "--indexes"],
        &["cache", "clean", "--indexes", "--older-than", "0"],
        &["cache", "clean"],
    ] {
        let cleaned = run(&base, &base, args);
        assert!(cleaned.status.success(), "{args:?}: {}", text(&cleaned));
        assert!(docs.join("thesis.txt").is_file(), "{args:?} deleted a file");
        assert!(
            docs.join("old/notes.txt").is_file(),
            "{args:?} deleted a folder"
        );
    }
}

/** L3. a GNU sparse entry stores 512 bytes and declares a file of 7 GiB.
the cap counted the stored bytes, so 230 bytes of gzip unpacked to 7 GiB. */
#[test]
fn a_sparse_archive_is_refused() {
    let base = scratch("sparse");
    let mut builder = tar::Builder::new(Vec::new());
    for (path, contents) in [("ember.toml", MANIFEST), ("src/init.luau", "return 1\n")] {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, contents.as_bytes())
            .unwrap();
    }
    let real: u64 = 7 << 30;
    let mut header = tar::Header::new_gnu();
    header.set_path("src/b.luau").unwrap();
    header.set_mode(0o644);
    header.set_size(512);
    header.set_entry_type(tar::EntryType::GNUSparse);
    let gnu = header.as_gnu_mut().unwrap();
    gnu.sparse[0]
        .offset
        .copy_from_slice(format!("{:011o}\0", real - 512).as_bytes());
    gnu.sparse[0]
        .numbytes
        .copy_from_slice(format!("{:011o}\0", 512).as_bytes());
    gnu.realsize
        .copy_from_slice(format!("{real:011o}\0").as_bytes());
    header.set_cksum();
    builder.append(&header, &[b'A'; 512][..]).unwrap();
    let archive = gzip(&builder.into_inner().unwrap());

    let installed = install_archive(&base, "environment = \"luau\"\n", &archive);
    /* the archive does not unpack, so install drops it from the cache and
    asks the registry, which this test does not have */
    assert!(!installed.status.success(), "{}", text(&installed));
    assert!(
        !base
            .join("proj/packages/luau/.ember/acme_lib/src/b.luau")
            .exists()
    );
}

/// L8. an archive sets the mode of each file, and install kept setuid and the write bit of everyone.
#[test]
fn installed_files_keep_no_mode_bit_outside_0755() {
    let base = scratch("modes");
    let archive = tar_gz(&[
        ("ember.toml", MANIFEST, 0o644),
        ("src/init.luau", "return 1\n", 0o666),
        ("src/open.luau", "return 1\n", 0o777),
        ("src/suid", "#!/bin/sh\n", 0o6777),
    ]);
    let installed = install_archive(&base, "environment = \"luau\"\n", &archive);
    assert!(installed.status.success(), "{}", text(&installed));
    let stored = base.join("proj/packages/luau/.ember/acme_lib/src");
    for file in ["init.luau", "open.luau", "suid"] {
        let mode = fs::metadata(stored.join(file))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777 & !0o755, 0, "{file} has mode {mode:o}");
    }
}

/** M3. the tool.toml of a release names the binaries after the main one,
and each got a shim in ~/.ember/bin. a binary named `git` took the place of
git in every terminal. a name that is a command of the computer already now
gets no shim. */
#[test]
fn a_second_binary_of_a_tool_does_not_take_the_place_of_a_command() {
    let base = scratch("bins");
    let executable = |path: &Path, text: &str| {
        write(path, text);
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    let project = base.join("proj");
    write(
        &project.join("ember.toml"),
        "[target]\nenvironment = \"luau\"\n\n[tools]\nkit = \"acme/kit@1.0.0\"\n",
    );
    let store = base.join("home/.ember/tools/acme_kit/1.0.0");
    for bin in ["kit", "kit-fmt", "git"] {
        executable(&store.join(bin), "#!/bin/sh\necho attacker\n");
    }
    write(&store.join(".embr-bins"), "kit-fmt\ngit\n");
    let sha = "a".repeat(64);
    write(
        &store.join(".embr-asset"),
        &format!("{sha}  tool.toml\n{sha}  kit-linux-x64.zip\n"),
    );
    // the git of this computer
    executable(&base.join("usr/git"), "#!/bin/sh\necho git\n");

    let path = format!(
        "{}:{}:{}",
        base.join("home/.ember/bin").display(),
        base.join("usr").display(),
        std::env::var("PATH").unwrap()
    );
    let installed = command(&project, &base)
        .arg("install")
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(installed.status.success(), "{}", text(&installed));
    let bin = base.join("home/.ember/bin");
    assert!(bin.join("kit").is_file() && bin.join("kit-fmt").is_file());
    assert!(!bin.join("git").exists(), "a shim took the place of git");
    assert!(
        text(&installed).contains("`git` of acme/kit gets no shim"),
        "{}",
        text(&installed)
    );

    // the next install has nothing to do, and still writes no shim
    let again = command(&project, &base)
        .arg("install")
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(again.status.success(), "{}", text(&again));
    assert!(!bin.join("git").exists());
}

/// M3. `embr tool delete someone/embr` deleted ~/.ember/bin/embr, the shim of the repository name.
#[test]
fn a_tool_delete_keeps_the_binaries_of_embr() {
    let base = scratch("delete");
    let bin = base.join("home/.ember/bin");
    write(&bin.join("embr"), "embr\n");
    write(&bin.join("embx"), "embx\n");
    write(&bin.join("other"), "other\n");
    let store = base.join("home/.ember/tools/someone_embr/1.0.0");
    write(&store.join("embr"), "#!/bin/sh\n");
    write(&store.join(".embr-bins"), "embx\nother\n");

    let deleted = run(&base, &base, &["tool", "delete", "someone/embr"]);
    assert!(deleted.status.success(), "{}", text(&deleted));
    assert!(!store.exists());
    assert!(bin.join("embr").is_file(), "embr is gone");
    assert!(bin.join("embx").is_file(), "embx is gone");
    assert!(!bin.join("other").exists());
}

/** L4 and L5. `embr doctor` printed the password of EMBER_API_URL, and the
whole token when the token was no header value. people paste this report
into bug reports. */
#[test]
fn doctor_prints_no_token_and_no_password() {
    let base = scratch("doctor");
    for args in [&["doctor"][..], &["doctor", "--json"]] {
        let output = command(&base, &base)
            .args(args)
            .env("EMBER_API_URL", "http://user:hunter2@127.0.0.1:1")
            .env("EMBER_TOKEN", "ember_sec ret")
            .output()
            .unwrap();
        let shown = text(&output);
        assert!(shown.contains("127.0.0.1:1"), "{args:?}: {shown}");
        assert!(!shown.contains("hunter2"), "{args:?}: {shown}");
        assert!(!shown.contains("sec ret"), "{args:?}: {shown}");
        assert!(
            shown.contains("holds a character that no token has"),
            "{args:?}: {shown}"
        );
    }
}
