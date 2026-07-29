/*! Shelling out to git. the index cache clones and pulls with it; `lpm
init` reads the surrounding repo for prompt defaults. */

use std::process::Command;

/** Runs git, reporting failures as its stderr. callers wrap that string in
whatever error fits, which keeps this free of crate::Error. */
pub fn run(args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// how a git invocation went wrong: git isn't installed at all, or it ran and failed.
#[derive(Debug)]
pub enum GitError {
    Missing,
    Failed(String),
}

/** stdout of a git command where exit code 1 still counts as success:
`git diff` answers 1 when there ARE differences, which for patching is the
interesting case, and neither `run` nor `output` can express it (`run`
fails on any non-zero, `output` throws the stdout away). anything past 1
fails with stderr; a spawn failure means git isn't installed, which
callers want to say plainly. */
pub fn capture_diff(args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git").args(args).output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            GitError::Missing
        } else {
            GitError::Failed(format!("could not run git: {error}"))
        }
    })?;

    match output.status.code() {
        Some(0 | 1) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        _ => Err(GitError::Failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )),
    }
}

/// whether git can be spawned at all; cached, install asks once per patched package.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

/** Trimmed stdout of a git command, or None when there's nothing useful:
git missing, not a repo, or the value simply unset. */
pub fn output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

/** Normalizes a git remote URL (git@host:..., ssh://git@host/...,
https://host/....git) into a plain https link. None for protocols we
can't rewrite. */
pub fn remote_https_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(".git");

    if url.starts_with("http://") || url.starts_with("https://") {
        Some(url.to_string())
    } else if let Some(rest) = url.strip_prefix("ssh://git@") {
        Some(format!("https://{rest}"))
    } else if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        Some(format!("https://{host}/{path}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_remote_urls_to_https() {
        assert_eq!(
            remote_https_url("https://github.com/luaupm/cli.git").as_deref(),
            Some("https://github.com/luaupm/cli")
        );
        assert_eq!(
            remote_https_url("git@github.com:luaupm/cli.git").as_deref(),
            Some("https://github.com/luaupm/cli")
        );
        assert_eq!(
            remote_https_url("ssh://git@github.com/luaupm/cli").as_deref(),
            Some("https://github.com/luaupm/cli")
        );
        assert_eq!(remote_https_url("ftp://example.com/a/b"), None);
    }

    #[test]
    fn failed_commands_report_nothing() {
        // a flag git always rejects, so no repo needed.
        assert_eq!(output(&["--not-a-real-flag"]), None);
        assert!(run(&["--not-a-real-flag"]).is_err());
    }

    #[test]
    fn capture_diff_accepts_exit_zero_and_one() {
        if !available() {
            return; // no git on this machine; the helpers all degrade the same way
        }

        // exit 0 with stdout
        let version = capture_diff(&["--version"]).unwrap();
        assert!(version.contains("git version"));

        /* exit 1 is the differences-exist case: diff two files that differ.
        --no-index needs no repo */
        let dir = std::env::temp_dir().join("lpm-test-capture-diff");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.join("b.txt"), "two\n").unwrap();
        let diff = capture_diff(&[
            "-C",
            &dir.to_string_lossy(),
            "diff",
            "--no-index",
            "a.txt",
            "b.txt",
        ])
        .unwrap();
        assert!(diff.contains("-one"), "{diff}");
        assert!(diff.contains("+two"), "{diff}");

        // anything past 1 is a real failure and carries stderr
        let error = capture_diff(&["-C", &dir.to_string_lossy(), "diff", "--not-a-real-flag"]);
        assert!(matches!(error, Err(GitError::Failed(stderr)) if !stderr.is_empty()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
