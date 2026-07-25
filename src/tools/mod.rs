//! Tools are GitHub-released binaries a project pins by version. This module
//! stores and installs them; `archive` picks and unpacks the right release
//! asset, `shim` covers how an alias resolves to one of the stored binaries.

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

/// Well-known tools addable by bare name; anything else must be "owner/repo".
const SHORTHANDS: &[(&str, &str)] = &[
    ("darklua", "seaofvoices/darklua"),
    ("lest", "luau-lest/lest"),
    ("luau-lsp", "johnnymorganz/luau-lsp"),
    ("rojo", "rojo-rbx/rojo"),
    ("stylua", "johnnymorganz/stylua"),
];

/// Expands a shorthand to its full repository name; names not in the table
/// are assumed to already be "owner/repo" and pass through unchanged.
pub fn expand_shorthand(name: &str) -> String {
    SHORTHANDS
        .iter()
        .find(|(short, _)| *short == name)
        .map_or_else(|| name.to_string(), |(_, full)| full.to_string())
}

/// The repository a shorthand stands for, for matching manifest entries by
/// their value rather than by their alias key.
pub fn shorthand_repository(name: &str) -> Option<&'static str> {
    SHORTHANDS
        .iter()
        .find(|(short, _)| *short == name)
        .map(|(_, full)| *full)
}

/// Where one version of a tool is stored: ~/.lpm/tools/{owner}_{repo}/{version}.
/// GitHub owner names cannot contain '_', so the first '_' always marks the
/// owner/repo split when reading the folder name back.
pub fn storage_dir(repository: &str, version: &str) -> Result<PathBuf, Error> {
    Ok(paths::tools_dir()?
        .join(repository.replace('/', "_"))
        .join(version))
}

/// Full path a tool's executable sits at once installed.
fn stored_executable(repository: &str, version: &str) -> Result<PathBuf, Error> {
    Ok(storage_dir(repository, version)?.join(executable_name(repo_short_name(repository))))
}

/// Installs a manifest tool, skipping all network work when the requested
/// version is already stored. Returns true when a download happened, false
/// when the cached copy was reused (the bin shim is refreshed either way).
pub fn install_tool(alias: &str, tool: &Tool, github: &GithubAPI) -> Result<bool, Error> {
    let stored = stored_executable(&tool.repository, &tool.version)?;
    if stored.exists() {
        shim::write(&shim::path(alias)?)?;
        return Ok(false);
    }

    let release = github.get_release(&tool.repository, &tool.version)?;
    install_release(alias, &tool.repository, &release)
}

/// Downloads the platform asset of an already-fetched release, extracts it,
/// stores the executable under `storage_dir`, and writes a bin shim named
/// after the alias. Returns true when a download happened, false when this
/// release version was already stored (the shim is refreshed either way).
pub fn install_release(alias: &str, repository: &str, release: &Release) -> Result<bool, Error> {
    let version = release.tag_name.trim_start_matches('v');
    let storage = storage_dir(repository, version)?;
    let repo = repo_short_name(repository);

    let stored = storage.join(executable_name(repo));
    if stored.exists() {
        shim::write(&shim::path(alias)?)?;
        return Ok(false);
    }

    let asset = archive::select_asset(&release.assets, env::consts::OS, env::consts::ARCH)
        .ok_or_else(|| Error::NoMatchingAsset {
            tool: repository.to_string(),
            version: version.to_string(),
        })?;
    let bytes = http::get_bytes(&asset.browser_download_url, &[])?;

    // Extract into a staging sibling first so a failed install never leaves a
    // half-written storage dir behind (same dance as install.rs).
    let staging = paths::with_suffix(&storage, ".tmp");
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    let target = staging.join(executable_name(repo));
    archive::extract(&asset.name, &bytes, &staging, &target)?;

    let mut files = Vec::new();
    archive::collect_files(&staging, &mut files)?;
    let found = archive::pick_executable(&files, repo, alias, env::consts::EXE_SUFFIX)
        .ok_or_else(|| Error::NoExecutableInAsset(alias.to_string()))?;
    if found != target {
        if target.exists() {
            fs::remove_file(&target)?;
        }
        fs::rename(&found, &target)?;
    }

    // Move the finished staging dir into place (same filesystem, so rename).
    if storage.exists() {
        fs::remove_dir_all(&storage)?;
    }
    fs::create_dir_all(storage.parent().expect("storage dir has a parent"))?;
    fs::rename(&staging, &storage)?;

    let stored = storage.join(executable_name(repo));
    make_executable(&stored)?;
    shim::write(&shim::path(alias)?)?;
    Ok(true)
}

/// The "repo" half of an "owner/repo" repository id; names the stored binary.
fn repo_short_name(repository: &str) -> &str {
    repository
        .split_once('/')
        .map_or(repository, |(_, repo)| repo)
}

/// File name a tool's executable is stored under, e.g. "rojo.exe" on Windows.
fn executable_name(repo: &str) -> String {
    format!("{repo}{}", env::consts::EXE_SUFFIX)
}

/// Marks a binary executable (0o755). No-op elsewhere: on Windows
/// executability comes from the .exe extension.
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), Error> {
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

    #[test]
    fn expands_known_shorthands_only() {
        assert_eq!(expand_shorthand("rojo"), "rojo-rbx/rojo");
        assert_eq!(expand_shorthand("acme/tool"), "acme/tool");
        assert_eq!(shorthand_repository("stylua"), Some("johnnymorganz/stylua"));
        assert_eq!(shorthand_repository("acme/tool"), None);
    }
}
