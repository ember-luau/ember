/*! GitHub-released binaries a project pins by version. this module stores
and installs them. `archive` picks and unpacks the right release asset,
`shim` maps an alias to one of the stored binaries. */

pub mod archive;
pub mod shim;

use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::net::http;
use crate::net::http::responses::Release;
use crate::project::manifest::Tool;
use crate::sys::paths;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// tools addable and `lpm execute`-runnable by bare name. anything else
/// must be "owner/repo".
const SHORTHANDS: &[(&str, &str)] = &[
    ("create-chief-project", "ryancundiff/create-chief-project"),
    ("darklua", "seaofvoices/darklua"),
    ("lest", "luau-lest/lest"),
    ("luau-lsp", "johnnymorganz/luau-lsp"),
    ("luaux", "luau-xml/luaux"),
    ("rojo", "rojo-rbx/rojo"),
    ("stylua", "johnnymorganz/stylua"),
];

/// shorthand to full repo name, unknown names pass through as "owner/repo".
pub fn expand_shorthand(name: &str) -> String {
    shorthand_repository(name).map_or_else(|| name.to_string(), str::to_string)
}

/** the repo behind a shorthand, for matching manifest entries by value,
not alias key. case-insensitive, like alias resolution. windows command
lookup doesn't care and neither should "StyLua". */
pub fn shorthand_repository(name: &str) -> Option<&'static str> {
    SHORTHANDS
        .iter()
        .find(|(short, _)| short.eq_ignore_ascii_case(name))
        .map(|(_, full)| *full)
}

/** alias names lpm's own binaries occupy in ~/.lpm/bin. a tool shim under
one of these would shadow the CLI or the lpx launcher. case-insensitive,
matching how windows resolves commands. */
const RESERVED_ALIASES: &[&str] = &["lpm", "lpx"];

/// whether an alias would shadow lpm's own binaries as a bin shim.
pub fn is_reserved_alias(alias: &str) -> bool {
    RESERVED_ALIASES
        .iter()
        .any(|reserved| alias.eq_ignore_ascii_case(reserved))
}

/** ~/.lpm/tools/{owner}_{repo}/{version}. GitHub owner names can't contain
'_', so the first '_' always marks the owner/repo split when reading the
folder name back. */
pub fn storage_dir(repository: &str, version: &str) -> Result<PathBuf, Error> {
    Ok(paths::tools_dir()?
        .join(repository.replace('/', "_"))
        .join(version))
}

/// where a tool's executable sits once installed.
fn stored_executable(repository: &str, version: &str) -> Result<PathBuf, Error> {
    Ok(storage_dir(repository, version)?.join(executable_name(repo_short_name(repository))))
}

/** whether a pinned tool is fully present, version stored and alias
shimmed. the cheap check install's fast path runs instead of the full tool
phase, a few stats, no network. */
pub fn is_installed(alias: &str, tool: &Tool) -> Result<bool, Error> {
    if is_reserved_alias(alias) {
        return Err(Error::ReservedToolAlias(alias.to_string()));
    }
    Ok(stored_executable(&tool.repository, &tool.version)?.exists() && shim::path(alias)?.exists())
}

/** Installs a manifest tool, skipping the network when the version is
already stored. true = downloaded, false = cache reused. the bin shim
gets refreshed either way. */
pub fn install_tool(alias: &str, tool: &Tool, github: &GithubAPI) -> Result<bool, Error> {
    if is_reserved_alias(alias) {
        return Err(Error::ReservedToolAlias(alias.to_string()));
    }
    let stored = stored_executable(&tool.repository, &tool.version)?;
    if stored.exists() {
        shim::write(&shim::path(alias)?)?;
        return Ok(false);
    }

    let release = github.get_release(&tool.repository, &tool.version)?;
    install_release(alias, &tool.repository, &release)
}

/** Downloads the platform asset of a fetched release, extracts it, stores
the executable under `storage_dir`, and writes a bin shim named after
the alias. true = downloaded, false = version already stored. the shim
gets refreshed either way. */
pub fn install_release(alias: &str, repository: &str, release: &Release) -> Result<bool, Error> {
    let (_, downloaded) = store_release(repository, release, alias)?;
    shim::write(&shim::path(alias)?)?;
    Ok(downloaded)
}

/** the storage half of `install_release`. downloads the platform asset of
a fetched release, extracts it, and stores the executable, no bin shim,
no manifest, which is what `lpm execute` runs on. returns the stored
executable and whether a download happened. `name_hint` is an extra
candidate name when picking the executable out of a multi-file asset. */
pub fn store_release(
    repository: &str,
    release: &Release,
    name_hint: &str,
) -> Result<(PathBuf, bool), Error> {
    let version = release.tag_name.trim_start_matches('v');
    let storage = storage_dir(repository, version)?;
    let repo = repo_short_name(repository);

    let stored = storage.join(executable_name(repo));
    if stored.exists() {
        return Ok((stored, false));
    }

    let asset = archive::select_asset(&release.assets, env::consts::OS, env::consts::ARCH)
        .ok_or_else(|| Error::NoMatchingAsset {
            tool: repository.to_string(),
            version: version.to_string(),
        })?;
    let bytes = http::get_bytes(&asset.browser_download_url, &[])?;

    /* extract into a staging sibling first so a failed install never
    leaves a half-written storage dir behind, same dance as install.rs.
    pid-suffixed because concurrent `lpx` runs of one cold tool are
    routine and must not tear down each other's staging mid-extraction. */
    let staging = Staging(paths::with_suffix(
        &storage,
        &format!(".{}.tmp", std::process::id()),
    ));
    if staging.0.exists() {
        fs::remove_dir_all(&staging.0)?;
    }
    fs::create_dir_all(&staging.0)?;

    let target = staging.0.join(executable_name(repo));
    archive::extract(&asset.name, &bytes, &staging.0, &target)?;

    let mut files = Vec::new();
    archive::collect_files(&staging.0, &mut files)?;
    let found = archive::pick_executable(&files, repo, name_hint, env::consts::EXE_SUFFIX)
        .ok_or_else(|| Error::NoExecutableInAsset(name_hint.to_string()))?;
    place_executable(&found, &target)?;

    /* a concurrent run may have stored this version while we downloaded.
    its copy of the same release is just as good, and possibly already
    executing, in which case deleting it would fail on windows anyway */
    if stored.exists() {
        return Ok((stored, true));
    }

    // move staging into place, same filesystem, so it's a rename.
    if storage.exists() {
        fs::remove_dir_all(&storage)?;
    }
    fs::create_dir_all(storage.parent().expect("storage dir has a parent"))?;
    fs::rename(&staging.0, &storage)?;

    let stored = storage.join(executable_name(repo));
    make_executable(&stored)?;
    Ok((stored, true))
}

/** the stored executable for an exact version of a repo, downloading that
release when storage is missing. the `lpm execute <spec>@version` path,
no shim, no manifest entry. */
pub fn ensure_stored(
    repository: &str,
    version: &str,
    github: &GithubAPI,
) -> Result<PathBuf, Error> {
    let stored = stored_executable(repository, version)?;
    if stored.exists() {
        return Ok(stored);
    }
    let release = github.get_release(repository, version)?;
    Ok(store_release(repository, &release, repo_short_name(repository))?.0)
}

/** the stored executable for a repo's latest release, asking GitHub at
most once per TTL. the last answer is stamped under the repo's storage
dir, and a fresh stamp whose version is still stored means zero network.
anonymous GitHub API calls are capped at 60/hour, so `lpx stylua` in a
loop must coast on the stamp. `refresh` forces the question. */
pub fn ensure_latest(
    repository: &str,
    github: &GithubAPI,
    refresh: bool,
) -> Result<PathBuf, Error> {
    let stamp = latest_stamp(repository)?;
    if !refresh && let Some(version) = fresh_latest_version(&stamp) {
        return ensure_stored(repository, &version, github);
    }

    let release = match github.get_latest_release(repository) {
        Ok(release) => release,
        /* offline or rate-limited past the TTL, a stale answer whose
        binaries are still stored beats dying. the stamp is left as-is,
        so the next run asks again instead of trusting this one. */
        Err(error) => {
            if let Some(version) = stamped_version(&stamp) {
                let stored = stored_executable(repository, &version)?;
                if stored.exists() {
                    eprintln!(
                        "warning: could not check {repository} for a newer release ({error}); running {version}"
                    );
                    return Ok(stored);
                }
            }
            return Err(error);
        }
    };
    let stored = store_release(repository, &release, repo_short_name(repository))?.0;

    /* stamped only after a successful store, so a failed download is
    retried next run instead of being remembered as the answer */
    if let Some(parent) = stamp.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(
        &stamp,
        format!("{}\n", release.tag_name.trim_start_matches('v')),
    );

    Ok(stored)
}

/** where a repo's latest-release answer is remembered. the version string,
with the file's mtime marking when GitHub was last asked. lives beside the
version dirs, whose readers all skip non-directories. */
fn latest_stamp(repository: &str) -> Result<PathBuf, Error> {
    Ok(paths::tools_dir()?
        .join(repository.replace('/', "_"))
        .join(".latest"))
}

/** the stamped version if the stamp is younger than the TTL, None
otherwise. deliberately shares the index TTL knob `LPM_INDEX_TTL_SECS`,
0 = never fresh, one dial for how stale cached answers may be. */
fn fresh_latest_version(stamp: &Path) -> Option<String> {
    use crate::registry::index;
    if !index::stamp_fresh(stamp, index::index_ttl()) {
        return None;
    }
    stamped_version(stamp)
}

/// the version a stamp remembers, fresh or stale, the offline fallback.
fn stamped_version(stamp: &Path) -> Option<String> {
    let version = fs::read_to_string(stamp).ok()?;
    let version = version.trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// the "repo" half of "owner/repo", names the stored binary.
/** the extraction staging dir, removed on the way out.

on success it has already been renamed into storage and there is nothing
left to remove. on failure -- and every step between creating it and that
rename can fail -- it would otherwise stay behind forever, one
`<version>.<pid>.tmp` per attempt, which is how a tool that cannot
install quietly fills its own storage folder. */
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/** puts the executable the archive actually shipped under the name lpm
stores it by.

the two are compared through the filesystem rather than as text, because
on windows and macOS they can be one file under two spellings:
JohnnyMorganz/StyLua ships `stylua.exe` while the repo's short name makes
the target `StyLua.exe`. Comparing paths as strings called those
different, so `target.exists()` -- true, being the very file just
extracted -- deleted it, and the rename then failed with a bare "the
system cannot find the file specified". Tools whose archive name matches
their repo, like rojo, never hit it. */
fn place_executable(found: &Path, target: &Path) -> Result<(), Error> {
    if paths::same_file(found, target) {
        return Ok(());
    }
    if target.exists() {
        fs::remove_file(target)?;
    }
    fs::rename(found, target)?;
    Ok(())
}

fn repo_short_name(repository: &str) -> &str {
    repository
        .split_once('/')
        .map_or(repository, |(_, repo)| repo)
}

/// "rojo.exe" on windows, "rojo" elsewhere.
fn executable_name(repo: &str) -> String {
    format!("{repo}{}", env::consts::EXE_SUFFIX)
}

/// chmod 755. no-op on windows, where the .exe extension is what makes it executable.
#[cfg(unix)]
pub(crate) fn make_executable(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn make_executable(_path: &Path) -> Result<(), Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_paths_include_owner_repo_and_version() {
        let dir = storage_dir("rojo-rbx/rojo", "7.4.4").unwrap();
        assert!(dir.starts_with(paths::tools_dir().unwrap()));
        assert!(dir.ends_with(Path::new("tools").join("rojo-rbx_rojo").join("7.4.4")));
    }

    /** regression. JohnnyMorganz/StyLua ships `stylua.exe`, and the repo's
    short name makes lpm want `StyLua.exe`. On a case-insensitive
    filesystem those are one file, and comparing the paths as text treated
    them as two: the target "already existed", so it was deleted -- it was
    the extracted binary -- and the rename then failed with os error 2.
    Every attempt also left its staging folder behind. */
    #[test]
    fn an_archive_naming_its_binary_in_another_case_still_installs() {
        let staging = std::env::temp_dir().join("lpm-test-place-executable");
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).unwrap();

        // what the archive shipped, and what lpm wants to store it as
        let found = staging.join("stylua.exe");
        let target = staging.join("StyLua.exe");
        fs::write(&found, b"the real binary").unwrap();

        place_executable(&found, &target).unwrap();

        // readable under the name lpm stores by, whichever spelling won
        assert_eq!(fs::read(&target).unwrap(), b"the real binary");
        // and exactly one binary, never a deleted one or a duplicate
        let left: Vec<_> = fs::read_dir(&staging).unwrap().flatten().collect();
        assert_eq!(
            left.len(),
            1,
            "{:?}",
            left.iter().map(|e| e.path()).collect::<Vec<_>>()
        );

        // a genuinely different name is still moved into place
        let other = staging.join("stylua-linux");
        fs::write(&other, b"another").unwrap();
        place_executable(&other, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"another");
        assert!(!other.exists());

        let _ = fs::remove_dir_all(&staging);
    }

    /// a failed install must not leave its `<version>.<pid>.tmp` folder behind.
    #[test]
    fn staging_is_removed_however_the_install_ends() {
        let dir = std::env::temp_dir().join("lpm-test-staging-guard");
        let _ = fs::remove_dir_all(&dir);

        {
            let staging = Staging(dir.clone());
            fs::create_dir_all(&staging.0).unwrap();
            fs::write(staging.0.join("half-extracted"), b"x").unwrap();
            assert!(dir.is_dir());
        }
        assert!(!dir.exists(), "staging outlived the install");
    }

    #[test]
    fn expands_known_shorthands_only() {
        assert_eq!(expand_shorthand("rojo"), "rojo-rbx/rojo");
        assert_eq!(expand_shorthand("acme/tool"), "acme/tool");
        assert_eq!(shorthand_repository("stylua"), Some("johnnymorganz/stylua"));
        assert_eq!(shorthand_repository("acme/tool"), None);
        assert_eq!(expand_shorthand("luaux"), "luau-xml/luaux");
        // case-insensitive, like alias resolution and windows command lookup
        assert_eq!(expand_shorthand("StyLua"), "johnnymorganz/stylua");
        assert_eq!(shorthand_repository("ROJO"), Some("rojo-rbx/rojo"));
    }

    #[test]
    fn shorthands_are_valid_repositories_and_never_reserved() {
        for (short, full) in SHORTHANDS {
            assert!(!short.contains('/'), "{short} is not a bare name");
            assert!(Tool::split_repository(full).is_ok(), "{full}");
            assert!(!is_reserved_alias(short), "{short} is reserved");
        }
        // the marquee lpx demo stays wired to its repo
        assert_eq!(
            expand_shorthand("create-chief-project"),
            "ryancundiff/create-chief-project"
        );
    }

    #[test]
    fn reserved_aliases_refuse_tool_installs() {
        assert!(is_reserved_alias("lpm"));
        assert!(is_reserved_alias("LPX"));
        assert!(!is_reserved_alias("rojo"));

        let tool = Tool {
            repository: "acme/tool".to_string(),
            version: "1.0.0".to_string(),
        };
        assert!(matches!(
            is_installed("lpx", &tool),
            Err(Error::ReservedToolAlias(_))
        ));
        assert!(matches!(
            install_tool("LPM", &tool, &GithubAPI::new()),
            Err(Error::ReservedToolAlias(_))
        ));
    }

    #[test]
    fn latest_stamp_freshness_gates_reuse() {
        // the assertions below assume the default TTL
        if std::env::var_os("LPM_INDEX_TTL_SECS").is_some() {
            return;
        }
        let dir = std::env::temp_dir().join("lpm-test-latest-stamp");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let stamp = dir.join(".latest");

        // no stamp, no answer
        assert_eq!(fresh_latest_version(&stamp), None);

        // a fresh stamp gives the stored version, whitespace trimmed
        fs::write(&stamp, "2.0.2\n").unwrap();
        assert_eq!(fresh_latest_version(&stamp).as_deref(), Some("2.0.2"));

        // a fresh but empty stamp is never an answer
        fs::write(&stamp, "\n").unwrap();
        assert_eq!(fresh_latest_version(&stamp), None);

        // aged past the TTL means stale, but still the offline fallback answer
        fs::write(&stamp, "2.0.2\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 60 * 60);
        fs::File::options()
            .write(true)
            .open(&stamp)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(fresh_latest_version(&stamp), None);
        assert_eq!(stamped_version(&stamp).as_deref(), Some("2.0.2"));

        let _ = fs::remove_dir_all(&dir);
    }
}
