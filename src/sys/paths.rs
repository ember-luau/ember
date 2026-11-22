/*! The ~/.ember layout plus a couple of path helpers. one module owns these
so nothing else rebuilds `home/.ember/...` by hand. */

use crate::error::Error;
use std::path::{Path, PathBuf};

/// ~/.ember, the root of everything embr installs or caches.
pub fn ember_dir() -> Result<PathBuf, Error> {
    let home = dirs::home_dir().ok_or(Error::NoHomeDir)?;
    Ok(home.join(".ember"))
}

/// ~/.ember/config.toml, the settings for every project on this machine.
pub fn config_file() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("config.toml"))
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

/// ~/.ember/credentials.toml, the Ember token kept between commands. auth writes it owner-only on unix.
pub fn credentials_file() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("credentials.toml"))
}

/// ~/.ember/index-cache, shallow clones of the git indices.
pub fn index_cache_dir() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("index-cache"))
}

/// ~/.ember/git-cache, one bare clone per git dependency.
pub fn git_cache_dir() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("git-cache"))
}

/// ~/.ember/templates, one folder per template that `embr template save` keeps.
pub fn templates_dir() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("templates"))
}

/// ~/.ember/presets, one `<name>.toml` per preset that `embr preset save` keeps.
pub fn presets_dir() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("presets"))
}

/// ~/.ember/local, the registry that `embr publish --local` writes.
pub fn local_dir() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("local"))
}

/** ~/.ember/shorthands.json, the tool shorthands of the registry, kept for a
day. `embr cache clean` deletes it. */
pub fn shorthands_file() -> Result<PathBuf, Error> {
    Ok(ember_dir()?.join("shorthands.json"))
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

/** a new, empty folder in the temp folder that only this user can enter,
`<prefix>-<16 random hex characters>`. the caller removes it.

the temp folder is shared between the users of a computer. a name that
follows from the pid can be made first by somebody else, who then owns the
folder that embr writes a binary or a git folder into. so the name is
random, the folder must not exist yet, and on unix its mode is 0700. */
pub fn private_temp_dir(prefix: &str) -> Result<PathBuf, Error> {
    use ring::rand::SecureRandom;

    let random = ring::rand::SystemRandom::new();
    for _ in 0..8 {
        let mut bytes = [0u8; 8];
        random
            .fill(&mut bytes)
            .map_err(|_| std::io::Error::other("the system gave no random bytes"))?;
        let name: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("{prefix}-{name}"));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not make a new folder in the temp folder",
    )
    .into())
}

/** whether `version` is safe as the name of a storage folder: the version
of a tool, which comes from [tools] and is joined onto ~/.ember/tools. a
release tag may hold a `/`, so the folders it names may nest, but no part
may be empty, `.` or `..`, and it holds no backslash, no drive letter and
no control character. `owner/repo@../../x` named a folder outside the
tools folder before this. */
pub fn is_safe_version(version: &str) -> bool {
    !version.is_empty()
        && !version.starts_with('-')
        && !version.contains(['\\', ':'])
        && !version.chars().any(char::is_control)
        && version
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/** one ordinary path segment, no separators, no drive letter, not `.` or
`..`. for names that come from a manifest, where a TOML key can be quoted
anything, before they are joined onto a folder embr writes into. */
pub fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !Path::new(name).is_absolute()
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
            templates_dir().unwrap(),
        ] {
            assert!(
                path.starts_with(&root),
                "{} escaped ~/.ember",
                path.display()
            );
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

    #[test]
    fn a_private_temp_dir_is_new_and_closed_to_others() {
        let first = private_temp_dir("embr-test-private").unwrap();
        let second = private_temp_dir("embr-test-private").unwrap();
        assert_ne!(first, second);
        assert!(first.is_dir() && std::fs::read_dir(&first).unwrap().next().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&first).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let _ = std::fs::remove_dir(&first);
        let _ = std::fs::remove_dir(&second);
    }

    #[test]
    fn tool_versions_stay_inside_their_folder() {
        for good in [
            "7.4.4",
            "v2.0.0-rc.1",
            "0.1.0+build.5",
            "release/1.0",
            "nightly",
        ] {
            assert!(is_safe_version(good), "{good}");
        }
        for bad in [
            "",
            "..",
            "../../../../tmp/x",
            "1.0/../../..",
            "/etc",
            "1.0/",
            "a//b",
            "C:\\x",
            "c:x",
            "..\\..\\x",
            "-rf",
            "1.0\n",
        ] {
            assert!(!is_safe_version(bad), "{bad:?}");
        }
    }
}
