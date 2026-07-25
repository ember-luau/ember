//! Everything lpm keeps outside the project: the ~/.lpm layout, plus the two
//! path helpers that used to be copy-pasted around it. One module owns these
//! so nothing has to rebuild `home/.lpm/...` by hand.

use crate::error::Error;
use std::path::{Path, PathBuf};

/// ~/.lpm — the root of everything lpm installs or caches.
pub fn lpm_dir() -> Result<PathBuf, Error> {
    Ok(dirs::home_dir().ok_or(Error::NoHomeDir)?.join(".lpm"))
}

/// ~/.lpm/bin — the per-alias tool shims, put on PATH by `lpm self install`.
pub fn bin_dir() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("bin"))
}

/// ~/.lpm/tools — tool binaries, one folder per repo, then per version.
pub fn tools_dir() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("tools"))
}

/// ~/.lpm/tools.toml — tools added with `--global`. They resolve in any
/// directory, unlike a project's [tools], which only resolve inside it.
pub fn global_tools_file() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("tools.toml"))
}

/// ~/.lpm/credentials.toml — the GitHub token, persisted between commands.
pub fn credentials_file() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("credentials.toml"))
}

/// ~/.lpm/index-cache — shallow clones of the git indices.
pub fn index_cache_dir() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("index-cache"))
}

/// Whether two paths point at the same thing on disk. Canonicalizing resolves
/// symlinks and casing; paths that can't be canonicalized (missing files)
/// count as different.
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// `path` with `suffix` glued onto its final component, for staging next to a
/// destination on the same filesystem (so the finish is a rename, not a copy).
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_lives_under_dot_lpm() {
        // Skipped on machines with no home directory rather than failing;
        // the shape of the layout is all this checks.
        let Ok(root) = lpm_dir() else {
            return;
        };
        assert!(root.ends_with(".lpm"));
        for path in [
            bin_dir().unwrap(),
            tools_dir().unwrap(),
            global_tools_file().unwrap(),
            credentials_file().unwrap(),
            index_cache_dir().unwrap(),
        ] {
            assert!(path.starts_with(&root), "{} escaped ~/.lpm", path.display());
        }
        assert!(credentials_file().unwrap().ends_with("credentials.toml"));
    }

    #[test]
    fn suffixes_the_last_component() {
        assert_eq!(
            with_suffix(Path::new("/tools/rojo/7.4.4"), ".tmp"),
            PathBuf::from("/tools/rojo/7.4.4.tmp")
        );
    }

    #[test]
    fn missing_paths_are_never_the_same_file() {
        assert!(!same_file(Path::new("/nope/a"), Path::new("/nope/a")));
    }
}
