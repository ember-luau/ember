/*! The ~/.lpm layout plus a couple of path helpers. one module owns these
so nothing else rebuilds `home/.lpm/...` by hand. */

use crate::error::Error;
use std::path::{Path, PathBuf};

/// ~/.lpm, the root of everything lpm installs or caches.
pub fn lpm_dir() -> Result<PathBuf, Error> {
    Ok(dirs::home_dir().ok_or(Error::NoHomeDir)?.join(".lpm"))
}

/// ~/.lpm/bin, the per-alias tool shims. `lpm self install` puts it on PATH.
pub fn bin_dir() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("bin"))
}

/// ~/.lpm/tools, one folder per repo, then per version.
pub fn tools_dir() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("tools"))
}

/** ~/.lpm/tools.toml, tools added with `--global`. these resolve anywhere,
unlike a project's [tools], which only resolve inside it. */
pub fn global_tools_file() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("tools.toml"))
}

/// ~/.lpm/credentials.toml, the GitHub token kept between commands. auth writes it owner-only on unix.
pub fn credentials_file() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("credentials.toml"))
}

/// ~/.lpm/index-cache, shallow clones of the git indices.
pub fn index_cache_dir() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("index-cache"))
}

/// ~/.lpm/archive-cache, downloaded package archives. `lpm cache clean` deletes it.
pub fn archive_cache_root() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("archive-cache"))
}

/** the versioned layer inside the archive cache. bump the folder name when
the entry format changes and old entries just stop being found, instead of
being misread. */
pub fn archive_cache_dir() -> Result<PathBuf, Error> {
    Ok(archive_cache_root()?.join("v1"))
}

/** Whether two paths point at the same thing on disk. canonicalizing
handles symlinks and casing. paths that won't canonicalize, like missing
files, count as different. */
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/** `path` with `suffix` glued onto its last component, for staging next to
a destination on the same filesystem so the finish is a rename, not a
copy. */
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
        /* skip on machines with no home dir instead of failing. the layout
        shape is all this checks. */
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
