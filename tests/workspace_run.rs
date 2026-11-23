/*! `embr run` with the workspace flags, and `embr install -p`, in a scratch
workspace. Each script appends a line to one log file, so the log holds the
order in which the members ran.

Unix only: the scripts are sh, and the tests point HOME at a scratch
folder. */
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/** a workspace with the members `a`, `b` and `c` under `packages`. each
entry of `edges` is (member, the member it depends on). each member has a
`build` script that writes its name to `log`, and `b` has a `check` script
that fails. */
fn workspace(name: &str, edges: &[(&str, &str)]) -> PathBuf {
    let base = std::env::temp_dir().join(format!("embr-test-wsrun-{name}"));
    let _ = fs::remove_dir_all(&base);
    let write = |path: &str, text: &str| {
        let path = base.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "ember.toml",
        "[target]\nenvironment = \"luau\"\nworkspace = [\"packages/*\"]\n",
    );
    for member in ["a", "b", "c"] {
        let mut text = format!(
            "[package]\nname = \"acme/{member}\"\nversion = \"1.0.0\"\n\n\
             [target]\nenvironment = \"luau\"\n\n[scripts]\n\
             build = \"echo {member} >> ../../log\"\n\
             check = \"echo {member} >> ../../log{}\"\n\n[dependencies]\n",
            if member == "b" { " && exit 3" } else { "" }
        );
        for (_, on) in edges.iter().filter(|(from, _)| *from == member) {
            text.push_str(&format!("{on} = {{ workspace = \"acme/{on}\" }}\n"));
        }
        write(&format!("packages/{member}/ember.toml"), &text);
        write(&format!("packages/{member}/src/init.luau"), "return 1\n");
    }
    base
}

fn run(dir: &Path, base: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_embr"))
        .args(args)
        .current_dir(dir)
        .env("HOME", base.join("home"))
        .env("NO_COLOR", "1")
        .env_remove("EMBER_TOKEN")
        .output()
        .expect("embr runs")
}

/// runs embr at the root, and gives the members in the order the log has them. the log starts empty.
fn order(base: &Path, args: &[&str]) -> (Output, Vec<String>) {
    let _ = fs::remove_file(base.join("log"));
    let output = run(base, base, args);
    let log = fs::read_to_string(base.join("log")).unwrap_or_default();
    (output, log.lines().map(str::to_string).collect())
}

fn git(base: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(base)
        .env("HOME", base.join("home"))
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

#[test]
fn members_run_in_dependency_order() {
    // path order is a, b, c. a depends on c, and c on b
    let base = workspace("order", &[("a", "c"), ("c", "b")]);

    let (output, log) = order(&base, &["run", "--workspace", "build"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(log, ["b", "c", "a"]);
    // each line of a member has the name of the member in front
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[acme/b] "), "{stdout}");

    // more jobs than members: the edges still decide the order
    let (output, log) = order(&base, &["run", "--workspace", "--jobs", "8", "build"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(log, ["b", "c", "a"]);

    // a member of the workspace is a place to run it from too
    let _ = fs::remove_file(base.join("log"));
    let output = run(
        &base.join("packages/a"),
        &base,
        &["run", "--workspace", "build"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read_to_string(base.join("log")).unwrap(), "b\nc\na\n");

    // a word after the script name goes to the script, also a flag of `embr run`
    let (output, log) = order(&base, &["run", "-p", "acme/a", "build", "--workspace"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(log, ["a --workspace"]);

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn the_flags_select_members() {
    let base = workspace("select", &[("a", "c"), ("c", "b")]);

    let (_, log) = order(&base, &["run", "-p", "acme/c", "-p", "a", "build"]);
    assert_eq!(log, ["c", "a"]);
    let (_, log) = order(&base, &["run", "--filter", "acme/[ab]", "build"]);
    assert_eq!(log, ["b", "a"]);
    let (_, log) = order(&base, &["run", "--filter", "packages/c", "build"]);
    assert_eq!(log, ["c"]);

    let (output, log) = order(&base, &["run", "-p", "acme/none", "build"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(log.is_empty());
    let (output, _) = order(&base, &["run", "--filter", "zzz*", "build"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No workspace member matches 'zzz*'"),
        "{stderr}"
    );
    let (output, _) = order(&base, &["run", "--workspace", "none"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("has a script named 'none'"), "{stderr}");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_cycle_is_an_error_that_names_the_members() {
    let base = workspace("cycle", &[("a", "b"), ("b", "a")]);

    let (output, log) = order(&base, &["run", "--workspace", "build"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(stderr.contains("acme/a -> acme/b -> acme/a"), "{stderr}");
    assert!(log.is_empty(), "no member runs: {log:?}");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_failure_stops_the_members_that_did_not_start() {
    // b runs first and fails. c and a come after it
    let base = workspace("failure", &[("a", "c"), ("c", "b")]);

    let (output, log) = order(&base, &["run", "--workspace", "check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(log, ["b"]);
    assert!(stderr.contains("'check' failed in acme/b"), "{stderr}");
    assert!(stderr.contains("not started: acme/a, acme/c"), "{stderr}");

    let (output, log) = order(&base, &["run", "--workspace", "--continue", "check"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(log, ["b", "c", "a"]);

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn changed_takes_the_changed_members_and_their_dependents() {
    let base = workspace("changed", &[("a", "c"), ("c", "b")]);
    fs::write(base.join(".gitignore"), "log\nhome\n").unwrap();
    git(&base, &["init", "-q"]);
    git(&base, &["add", "."]);
    git(&base, &["commit", "-q", "-m", "first"]);

    // nothing changed: no member runs, and that is no error
    let (output, log) = order(&base, &["run", "--changed", "build"]);
    assert!(output.status.success(), "{output:?}");
    assert!(log.is_empty(), "{log:?}");

    // an edit in c: c runs, and a, which depends on it. b does not
    fs::write(base.join("packages/c/src/init.luau"), "return 2\n").unwrap();
    let (_, log) = order(&base, &["run", "--changed", "build"]);
    assert_eq!(log, ["c", "a"]);

    // a file that git does not track yet counts
    git(&base, &["checkout", "-q", "."]);
    fs::write(base.join("packages/a/src/new.luau"), "return 3\n").unwrap();
    let (_, log) = order(&base, &["run", "--changed", "build"]);
    assert_eq!(log, ["a"]);

    // --since compares with another ref
    git(&base, &["add", "."]);
    git(&base, &["commit", "-q", "-m", "second"]);
    fs::write(base.join("packages/b/src/init.luau"), "return 4\n").unwrap();
    git(&base, &["commit", "-q", "-am", "third"]);
    let (_, log) = order(&base, &["run", "--changed", "build"]);
    assert!(log.is_empty(), "{log:?}");
    let (_, log) = order(&base, &["run", "--changed", "--since", "HEAD~1", "build"]);
    assert_eq!(log, ["b", "c", "a"]);
    let (_, log) = order(&base, &["run", "--changed", "--since", "HEAD~2", "build"]);
    assert_eq!(log, ["b", "c", "a"]);

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn install_p_installs_one_member() {
    let base = workspace("install", &[("a", "b")]);

    let output = run(&base, &base, &["install", "-p", "acme/a"]);
    assert!(output.status.success(), "{output:?}");
    assert!(base.join("packages/a/ember.lock").is_file());
    assert!(!base.join("packages/c/ember.lock").exists());
    assert!(!base.join("ember.lock").exists());

    let output = run(&base, &base, &["install", "-p", "acme/none"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr.contains("No workspace member is named 'acme/none'"),
        "{stderr}"
    );

    let _ = fs::remove_dir_all(&base);
}
