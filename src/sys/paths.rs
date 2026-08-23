/*! The ~/.ember layout plus a couple of path helpers. one module owns these
so nothing else rebuilds `home/.ember/...` by hand. */

use crate::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Once;

/// the root's name before the ember rename. moved aside on first use.
const LEGACY_ROOT: &str = ".lpm";

/// ~/.ember, the root of everything embr installs or caches.
pub fn ember_dir() -> Result<PathBuf, Error> {
    let home = dirs::home_dir().ok_or(Error::NoHomeDir)?;
    let root = home.join(".ember");
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if adopt_legacy_root(&home, &root) {
            eprintln!(
                "Moved {} to {}. Run `embr self install` to point your PATH at the new location.",
                home.join(LEGACY_ROOT).display(),
                root.display()
            );
        }
    });
    Ok(root)
}

/** Renames ~/.lpm to ~/.ember. Returns whether it moved anything.

Called once per process, the first time anything asks for the root.

A rename rather than a fresh directory because the old root holds installed
tools, the archive and index caches, and the GitHub token. Leaving it behind
would silently re-download every tool and log the user out.

Best effort on purpose: if the rename fails, the caller gets an empty ~/.ember
and pays a re-download, which is a cost, not a corruption. It is deliberately
skipped when ~/.ember already exists, so a half-finished move never merges two
roots.

`self install` puts the bin dir on PATH, so the move leaves that entry stale.
On Windows `self install` rewrites the registry entry. On unix the PATH line
lives in the user's own shell profile and only they can edit it, so `ember_dir`
prints a notice when this returns true. */
fn adopt_legacy_root(home: &Path, root: &Path) -> bool {
    if root.exists() {
        return false;
    }
    let legacy = home.join(LEGACY_ROOT);
    legacy.is_dir() && std::fs::rename(&legacy, root).is_ok()
}

/// ~/.ember/bin, the per-alias tool shims. `embr self install` puts it on PATH.
pub fn bin_dir() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("bin"))
}

/// ~/.ember/tools, one folder per repo, then per version.
pub fn tools_dir() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("tools"))
}

/** ~/.ember/tools.toml, tools added with `--global`. these resolve anywhere,
unlike a project's [tools], which only resolve inside it. */
pub fn global_tools_file() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("tools.toml"))
}

/// ~/.ember/credentials.toml, the GitHub token kept between commands. auth writes it owner-only on unix.
pub fn credentials_file() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("credentials.toml"))
}

/// ~/.ember/index-cache, shallow clones of the git indices.
pub fn index_cache_dir() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("index-cache"))
}

/// ~/.ember/archive-cache, downloaded package archives. `embr cache clean` deletes it.
pub fn archive_cache_root() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("archive-cache"))
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
    fn everything_lives_under_dot_ember() {
        /* skip on machines with no home dir instead of failing. the layout
        shape is all this checks. */
        let Ok(root) = ember_dir() else {
            return;
        };
        assert!(root.ends_with(".ember"));
        for path in [
            bin_dir().unwrap(),
            tools_dir().unwrap(),
            global_tools_file().unwrap(),
            credentials_file().unwrap(),
            index_cache_dir().unwrap(),
        ] {
            assert!(
                path.starts_with(&root),
                "{} escaped ~/.ember",
                path.display()
            );
        }
        assert!(credentials_file().unwrap().ends_with("credentials.toml"));
    }

    /// a home with the given roots present, cleaned first so reruns are honest.
    fn home_with(name: &str, roots: &[&str]) -> PathBuf {
        let home = std::env::temp_dir().join(format!("embr-test-root-{name}"));
        let _ = std::fs::remove_dir_all(&home);
        for root in roots {
            std::fs::create_dir_all(home.join(root)).unwrap();
        }
        home
    }

    #[test]
    fn the_legacy_root_is_adopted_when_there_is_no_new_one() {
        let home = home_with("adopt", &[".lpm"]);
        std::fs::write(home.join(".lpm/credentials.toml"), "token = \"keep me\"").unwrap();

        assert!(adopt_legacy_root(&home, &home.join(".ember")));
        assert!(!home.join(".lpm").exists());
        // the move carries the contents, which is the whole point of renaming
        assert_eq!(
            std::fs::read_to_string(home.join(".ember/credentials.toml")).unwrap(),
            "token = \"keep me\""
        );
    }

    #[test]
    fn an_existing_new_root_is_never_merged_with_the_legacy_one() {
        let home = home_with("both", &[".lpm", ".ember"]);
        assert!(!adopt_legacy_root(&home, &home.join(".ember")));
        // both survive untouched: the caller uses ~/.ember and ignores ~/.lpm
        assert!(home.join(".lpm").is_dir());
        assert!(home.join(".ember").is_dir());
    }

    #[test]
    fn a_home_with_neither_root_moves_nothing() {
        let home = home_with("neither", &[]);
        assert!(!adopt_legacy_root(&home, &home.join(".ember")));
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
