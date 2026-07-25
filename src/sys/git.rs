//! Shelling out to git. The index cache clones and pulls with it; `lpm init`
//! reads the surrounding repo for prompt defaults.

use std::process::Command;

/// Runs git, reporting failures as its stderr. Callers wrap that string in
/// whatever error fits, so this stays free of crate::Error.
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

/// Trimmed stdout of a git command, or None when there is nothing useful to
/// report: git missing, not a repository, or the value simply unset.
pub fn output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Normalizes a git remote URL ("git@host:owner/repo.git",
/// "ssh://git@host/owner/repo", "https://host/owner/repo.git", ...) into a
/// plain https link. None for protocols we can't rewrite.
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
        // A flag git will always reject, so this needs no repository.
        assert_eq!(output(&["--not-a-real-flag"]), None);
        assert!(run(&["--not-a-real-flag"]).is_err());
    }
}
