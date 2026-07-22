use crate::error::Error;
use crate::github::GithubAPI;
use crate::http;
use crate::http::responses::{Asset, Release};
use crate::manifest::{MANIFEST_FILE, Tool};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn lpm_dir() -> Result<PathBuf, Error> {
    Ok(dirs::home_dir().ok_or(Error::NoHomeDir)?.join(".lpm"))
}

/// Root under which tool binaries are stored, one folder per repo@version.
pub fn tools_dir() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("tools"))
}

/// Folder the per-alias shims live in; `lpm self install` puts it on PATH.
pub fn bin_dir() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("bin"))
}

/// Tools added with `lpm tool add --global` are recorded here and resolve in
/// any directory; project [tools] entries only resolve inside that project.
pub fn global_manifest_path() -> Result<PathBuf, Error> {
    Ok(lpm_dir()?.join("tools.toml"))
}

/// Where one version of a tool is stored: ~/.lpm/tools/{owner}_{repo}/{version}.
/// GitHub owner names cannot contain '_', so the first '_' always marks the
/// owner/repo split when reading the folder name back.
pub fn storage_dir(repository: &str, version: &str) -> Result<PathBuf, Error> {
    Ok(tools_dir()?
        .join(repository.replace('/', "_"))
        .join(version))
}

/// Installs a manifest tool, skipping all network work when the requested
/// version is already stored. Returns true when a download happened, false
/// when the cached copy was reused (the bin shim is refreshed either way).
pub fn install_tool(alias: &str, tool: &Tool, github: &GithubAPI) -> Result<bool, Error> {
    let storage = storage_dir(&tool.repository, &tool.version)?;
    let stored = storage.join(executable_name(repo_short_name(&tool.repository)));

    if stored.exists() {
        write_shim(&shim_path(alias)?)?;
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
        write_shim(&shim_path(alias)?)?;
        return Ok(false);
    }

    let asset =
        select_asset(&release.assets, env::consts::OS, env::consts::ARCH).ok_or_else(|| {
            Error::NoMatchingAsset {
                tool: repository.to_string(),
                version: version.to_string(),
            }
        })?;
    let bytes = http::get_bytes(&asset.browser_download_url, &[])?;

    // Extract into a staging sibling first so a failed install never leaves a
    // half-written storage dir behind (same dance as install.rs).
    let staging = sibling_with_suffix(&storage, ".tmp");
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    let target = staging.join(executable_name(repo));
    match archive_kind(&asset.name, &bytes) {
        ArchiveKind::Zip => {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            archive.extract(&staging)?;
        }
        ArchiveKind::TarGz => {
            let decoder = flate2::read::GzDecoder::new(bytes.as_slice());
            tar::Archive::new(decoder).unpack(&staging)?;
        }
        ArchiveKind::Tar => {
            tar::Archive::new(bytes.as_slice()).unpack(&staging)?;
        }
        // Not an archive at all: the asset itself is the executable.
        ArchiveKind::Raw => {
            fs::write(&target, &bytes)?;
        }
    }

    let mut files = Vec::new();
    collect_files(&staging, &mut files)?;
    let found = pick_executable(&files, repo, alias, env::consts::EXE_SUFFIX)
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
    write_shim(&shim_path(alias)?)?;
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

/// Path of the shim a tool alias is invoked through, e.g. ~/.lpm/bin/stylua.
fn shim_path(alias: &str) -> Result<PathBuf, Error> {
    Ok(bin_dir()?.join(format!("{alias}{}", env::consts::EXE_SUFFIX)))
}

/// Writes the alias shim: a copy of the running lpm executable named after
/// the alias (aftman-style). Invoked under that name, lpm dispatches to
/// whatever tool the surrounding manifest pins (see `shim_alias`/`run_shim`)
/// instead of running the CLI, which is what scopes tools to their projects.
fn write_shim(shim: &Path) -> Result<(), Error> {
    fs::create_dir_all(shim.parent().expect("bin dir has a parent"))?;
    fs::copy(env::current_exe()?, shim)?;
    make_executable(shim)
}

/// `path` with `suffix` appended to its final component, used to stage next
/// to the real storage dir on the same filesystem.
fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
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

/// The alias this process was invoked under, or None when running as the
/// real lpm CLI. Shims are copies of lpm named after their tool; anything
/// starting with "lpm" is treated as the CLI itself because release assets
/// are named "lpm-{os}-{arch}".
pub fn shim_alias() -> Option<String> {
    let exe = env::current_exe().ok()?;
    alias_from_stem(exe.file_stem()?.to_str()?)
}

fn alias_from_stem(stem: &str) -> Option<String> {
    let lower = stem.to_ascii_lowercase();
    if lower == "lpm" || lower.starts_with("lpm-") {
        None
    } else {
        Some(stem.to_string())
    }
}

/// Runs `alias` as a tool shim: resolves which repo@version the surrounding
/// project (or the global tools file) pins the alias to, then hands off to
/// the stored binary. Only returns the tool's exit code on platforms without
/// exec (Windows); on unix the tool replaces this process.
pub fn run_shim(alias: &str) -> Result<i32, Error> {
    let tool = resolve_alias(alias, &env::current_dir()?, &global_manifest_path()?)?;
    let stored = storage_dir(&tool.repository, &tool.version)?
        .join(executable_name(repo_short_name(&tool.repository)));
    if !stored.exists() {
        return Err(Error::ToolNotInstalled(alias.to_string()));
    }
    exec_tool(&stored)
}

/// Finds the tool an alias refers to: the nearest lpm.toml walking up from
/// `start` whose [tools] lists the alias wins, then the global tools file.
fn resolve_alias(alias: &str, start: &Path, global: &Path) -> Result<Tool, Error> {
    for dir in start.ancestors() {
        if let Some(tools) = tools_in(&dir.join(MANIFEST_FILE))?
            && let Some(tool) = get_tool(&tools, alias)
        {
            return Ok(tool.clone());
        }
    }
    if let Some(tools) = tools_in(global)?
        && let Some(tool) = get_tool(&tools, alias)
    {
        return Ok(tool.clone());
    }
    Err(Error::ToolNotManaged(alias.to_string()))
}

/// Tools pinned by the global tools file; empty when it does not exist yet.
pub fn global_tools() -> Result<BTreeMap<String, Tool>, Error> {
    Ok(tools_in(&global_manifest_path()?)?.unwrap_or_default())
}

/// Just the [tools] table of a manifest, parsed leniently so the shim can
/// read both project manifests and the global tools file (which has no
/// [package] section).
#[derive(Deserialize)]
struct ToolsTable {
    #[serde(default)]
    tools: BTreeMap<String, Tool>,
}

fn tools_in(path: &Path) -> Result<Option<BTreeMap<String, Tool>>, Error> {
    if !path.exists() {
        return Ok(None);
    }
    let table: ToolsTable = toml::from_str(&fs::read_to_string(path)?)?;
    Ok(Some(table.tools))
}

/// Alias lookup, exact key first; Windows resolves commands case-insensitively
/// so a shim can be invoked as "Rojo" while the manifest key is "rojo".
fn get_tool<'a>(tools: &'a BTreeMap<String, Tool>, alias: &str) -> Option<&'a Tool> {
    tools.get(alias).or_else(|| {
        tools
            .iter()
            .find_map(|(key, tool)| key.eq_ignore_ascii_case(alias).then_some(tool))
    })
}

#[cfg(unix)]
fn exec_tool(stored: &Path) -> Result<i32, Error> {
    use std::os::unix::process::CommandExt;
    // exec replaces this process on success, so reaching a return value at
    // all means it failed.
    Err(Error::Io(
        std::process::Command::new(stored)
            .args(env::args_os().skip(1))
            .exec(),
    ))
}

#[cfg(not(unix))]
fn exec_tool(stored: &Path) -> Result<i32, Error> {
    let status = std::process::Command::new(stored)
        .args(env::args_os().skip(1))
        .status()?;
    Ok(status.code().unwrap_or(1))
}

/// How a release asset's bytes should be unpacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    Zip,
    TarGz,
    Tar,
    /// Not an archive: the asset is the executable itself.
    Raw,
}

/// Decides how to unpack an asset. Content is magic-byte sniffed rather than
/// trusted from the file name because ureq transparently decodes
/// Content-Encoding: gzip (see index::download); only plain tar, which has no
/// leading magic, falls back to the name.
fn archive_kind(name: &str, bytes: &[u8]) -> ArchiveKind {
    if bytes.starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
        ArchiveKind::Zip
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        ArchiveKind::TarGz
    } else if name.to_ascii_lowercase().ends_with(".tar") {
        ArchiveKind::Tar
    } else {
        ArchiveKind::Raw
    }
}

/// Recursively collects every regular file under `dir` with its size.
fn collect_files(dir: &Path, files: &mut Vec<(PathBuf, u64)>) -> Result<(), Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&entry.path(), files)?;
        } else if file_type.is_file() {
            files.push((entry.path(), entry.metadata()?.len()));
        }
    }
    Ok(())
}

/// Picks the tool's executable among the extracted files. When the platform
/// uses an executable suffix (Windows), files carrying it are preferred;
/// within the pool a file named after the repo or alias wins, else the
/// largest file (archives bundle READMEs, licenses, completions).
fn pick_executable(
    files: &[(PathBuf, u64)],
    repo: &str,
    alias: &str,
    exe_suffix: &str,
) -> Option<PathBuf> {
    let suffix = exe_suffix.to_ascii_lowercase();
    let with_suffix: Vec<&(PathBuf, u64)> = if suffix.is_empty() {
        files.iter().collect()
    } else {
        files
            .iter()
            .filter(|(path, _)| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.to_ascii_lowercase().ends_with(&suffix))
            })
            .collect()
    };
    let pool = if with_suffix.is_empty() {
        files.iter().collect::<Vec<_>>()
    } else {
        with_suffix
    };

    let named_like_tool = |path: &Path| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case(repo) || stem.eq_ignore_ascii_case(alias))
    };

    pool.iter()
        .find(|(path, _)| named_like_tool(path))
        .or_else(|| pool.iter().max_by_key(|(_, size)| *size))
        .map(|(path, _)| path.clone())
}

/// Checksum/signature/metadata companions that must never be picked as the
/// tool binary.
const METADATA_SUFFIXES: &[&str] = &[
    ".sha256",
    ".sha256sum",
    ".sig",
    ".asc",
    ".txt",
    ".json",
    ".pem",
];

/// Names release assets use for an OS. Matching is word-start anchored (see
/// `contains_word_start`) so the "win" alias does not match "darwin".
fn os_aliases(os: &str) -> &'static [&'static str] {
    match os {
        "windows" => &["windows", "win64", "win32", "win"],
        "macos" => &["macos", "darwin", "apple", "osx"],
        "linux" => &["linux", "musl"],
        _ => &[],
    }
}

/// Names release assets use for a CPU architecture.
fn arch_aliases(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["x86_64", "amd64", "x64"],
        "aarch64" => &["aarch64", "arm64"],
        _ => &[],
    }
}

/// Picks the release asset for an os/arch pair (std::env::consts values).
/// Prefers a full OS+arch match, falling back to an OS-only match because
/// many tools publish a single arch per OS. None means nothing fits.
fn select_asset<'a>(assets: &'a [Asset], os: &str, arch: &str) -> Option<&'a Asset> {
    let os_names = os_aliases(os);
    let arch_names = arch_aliases(arch);

    let candidates: Vec<(&Asset, String)> = assets
        .iter()
        .map(|asset| (asset, asset.name.to_ascii_lowercase()))
        .filter(|(_, name)| {
            !METADATA_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix))
        })
        .collect();

    candidates
        .iter()
        .find(|(_, name)| matches_any(name, os_names) && matches_any(name, arch_names))
        .or_else(|| {
            candidates
                .iter()
                .find(|(_, name)| matches_any(name, os_names))
        })
        .map(|(asset, _)| *asset)
}

fn matches_any(name: &str, aliases: &[&str]) -> bool {
    aliases.iter().any(|alias| contains_word_start(name, alias))
}

/// True when `needle` appears in `haystack` at a word start (the beginning of
/// the string or right after a non-alphanumeric character). Keeps short
/// aliases honest: "win" matches "win64" and "tool-win.zip" but not "darwin".
fn contains_word_start(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut search_from = 0;
    while let Some(position) = haystack[search_from..].find(needle) {
        let start = search_from + position;
        let preceded_by_word = haystack[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        if !preceded_by_word {
            return true;
        }
        search_from = start + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets(names: &[&str]) -> Vec<Asset> {
        names
            .iter()
            .map(|name| Asset {
                name: name.to_string(),
                browser_download_url: format!("https://example.com/{name}"),
            })
            .collect()
    }

    #[test]
    fn selects_platform_assets() {
        // The .sha256 companion sorts before the real zip to prove metadata
        // files are skipped even when their names match the platform.
        let list = assets(&[
            "checksums.txt",
            "stylua-linux-x86_64.zip.sha256",
            "stylua-linux-x86_64.zip",
            "stylua-windows-x86_64.zip",
            "stylua-macos-aarch64.zip",
        ]);
        assert_eq!(
            select_asset(&list, "linux", "x86_64").unwrap().name,
            "stylua-linux-x86_64.zip"
        );
        assert_eq!(
            select_asset(&list, "windows", "x86_64").unwrap().name,
            "stylua-windows-x86_64.zip"
        );
        assert_eq!(
            select_asset(&list, "macos", "aarch64").unwrap().name,
            "stylua-macos-aarch64.zip"
        );
    }

    #[test]
    fn selects_versioned_asset_names() {
        let list = assets(&[
            "rojo-7.4.4-linux-x86_64.zip",
            "rojo-7.4.4-macos-aarch64.zip",
            "rojo-7.4.4-windows-x86_64.zip",
        ]);
        assert_eq!(
            select_asset(&list, "linux", "x86_64").unwrap().name,
            "rojo-7.4.4-linux-x86_64.zip"
        );
        assert_eq!(
            select_asset(&list, "macos", "aarch64").unwrap().name,
            "rojo-7.4.4-macos-aarch64.zip"
        );
        assert_eq!(
            select_asset(&list, "windows", "x86_64").unwrap().name,
            "rojo-7.4.4-windows-x86_64.zip"
        );
        // No macos x86_64 build exists: fall back to the OS-only match.
        assert_eq!(
            select_asset(&list, "macos", "x86_64").unwrap().name,
            "rojo-7.4.4-macos-aarch64.zip"
        );
    }

    #[test]
    fn prefers_full_match_over_os_only_match() {
        let list = assets(&["tool-macos-aarch64.zip", "tool-macos-x86_64.zip"]);
        assert_eq!(
            select_asset(&list, "macos", "x86_64").unwrap().name,
            "tool-macos-x86_64.zip"
        );
    }

    #[test]
    fn recognizes_alias_spellings() {
        let list = assets(&[
            "tool-win64.zip",
            "tool-darwin-arm64.tar.gz",
            "tool-linux-musl-amd64.tar.gz",
        ]);
        assert_eq!(
            select_asset(&list, "windows", "x86_64").unwrap().name,
            "tool-win64.zip"
        );
        assert_eq!(
            select_asset(&list, "macos", "aarch64").unwrap().name,
            "tool-darwin-arm64.tar.gz"
        );
        assert_eq!(
            select_asset(&list, "linux", "x86_64").unwrap().name,
            "tool-linux-musl-amd64.tar.gz"
        );
    }

    #[test]
    fn win_alias_does_not_match_darwin() {
        let list = assets(&["tool-darwin-x64.tar.gz"]);
        assert!(select_asset(&list, "windows", "x86_64").is_none());
        assert_eq!(
            select_asset(&list, "macos", "x86_64").unwrap().name,
            "tool-darwin-x64.tar.gz"
        );
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        assert!(select_asset(&[], "linux", "x86_64").is_none());
        let metadata_only = assets(&["checksums.txt", "tool-linux-x86_64.sha256"]);
        assert!(select_asset(&metadata_only, "linux", "x86_64").is_none());
        let wrong_os = assets(&["tool-windows-x86_64.zip"]);
        assert!(select_asset(&wrong_os, "linux", "x86_64").is_none());
    }

    #[test]
    fn sniffs_archive_kinds() {
        assert_eq!(
            archive_kind("tool.zip", b"PK\x03\x04rest of zip"),
            ArchiveKind::Zip
        );
        assert_eq!(
            archive_kind("tool.tar.gz", &[0x1f, 0x8b, 0x08, 0x00]),
            ArchiveKind::TarGz
        );
        // Magic bytes win over the name: a ".tar" that is really gzipped
        // (ureq did not decode it) must still be gunzipped.
        assert_eq!(
            archive_kind("tool.tar", &[0x1f, 0x8b, 0x08, 0x00]),
            ArchiveKind::TarGz
        );
        assert_eq!(
            archive_kind("tool.tar", b"plain tar header bytes"),
            ArchiveKind::Tar
        );
        assert_eq!(archive_kind("tool.exe", b"MZ\x90\x00"), ArchiveKind::Raw);
        assert_eq!(
            archive_kind("tool-linux-x86_64", &[0x7f, b'E', b'L', b'F']),
            ArchiveKind::Raw
        );
    }

    #[test]
    fn picks_executables_from_extracted_files() {
        // Unix: a file named after the tool beats the largest file.
        let files = vec![
            (PathBuf::from("staging/README.md"), 10_000),
            (PathBuf::from("staging/bin/stylua"), 2_000),
            (PathBuf::from("staging/LICENSE"), 1_000),
        ];
        assert_eq!(
            pick_executable(&files, "stylua", "stylua", "").unwrap(),
            PathBuf::from("staging/bin/stylua")
        );

        // The alias also counts as a name match.
        let files = vec![(PathBuf::from("staging/fmt"), 5)];
        assert_eq!(
            pick_executable(&files, "some-formatter", "fmt", "").unwrap(),
            PathBuf::from("staging/fmt")
        );

        // Windows: .exe files are preferred over everything else.
        let files = vec![
            (PathBuf::from("staging/README.md"), 10_000),
            (PathBuf::from("staging/rojo.exe"), 5_000),
        ];
        assert_eq!(
            pick_executable(&files, "rojo", "rojo", ".exe").unwrap(),
            PathBuf::from("staging/rojo.exe")
        );

        // No name match anywhere: the largest file wins.
        let files = vec![
            (PathBuf::from("staging/helper"), 10),
            (PathBuf::from("staging/tool-v2"), 9_000),
        ];
        assert_eq!(
            pick_executable(&files, "tool", "tool", "").unwrap(),
            PathBuf::from("staging/tool-v2")
        );

        assert!(pick_executable(&[], "tool", "tool", "").is_none());
    }

    #[test]
    fn storage_paths_include_owner_repo_and_version() {
        let dir = storage_dir("rojo-rbx/rojo", "7.4.4").unwrap();
        assert!(dir.starts_with(tools_dir().unwrap()));
        assert!(dir.ends_with(Path::new("tools").join("rojo-rbx_rojo").join("7.4.4")));
    }

    #[test]
    fn shim_aliases_exclude_lpm_itself() {
        assert_eq!(alias_from_stem("lpm"), None);
        assert_eq!(alias_from_stem("LPM"), None);
        // Release assets run before `self install` renames them.
        assert_eq!(alias_from_stem("lpm-windows-x86_64"), None);
        assert_eq!(alias_from_stem("rojo"), Some("rojo".to_string()));
        assert_eq!(alias_from_stem("StyLua"), Some("StyLua".to_string()));
    }

    #[test]
    fn resolves_aliases_from_nearest_manifest_then_global() {
        let base = std::env::temp_dir().join("lpm-test-resolve-alias");
        let _ = fs::remove_dir_all(&base);

        let project = base.join("project");
        let nested = project.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            project.join(MANIFEST_FILE),
            "[package]\nname = \"scope/name\"\nversion = \"0.1.0\"\n\n[tools]\nrojo = \"rojo-rbx/rojo@7.4.4\"\n",
        )
        .unwrap();
        let global = base.join("tools.toml");
        fs::write(
            &global,
            "[tools]\nstylua = \"johnnymorganz/stylua@2.0.0\"\n",
        )
        .unwrap();

        // Project manifests win, found from any directory below them.
        let tool = resolve_alias("rojo", &nested, &global).unwrap();
        assert_eq!(tool.repository, "rojo-rbx/rojo");
        assert_eq!(tool.version, "7.4.4");

        // Aliases the project does not list fall back to the global file.
        let tool = resolve_alias("stylua", &nested, &global).unwrap();
        assert_eq!(tool.repository, "johnnymorganz/stylua");

        // Windows command lookup is case-insensitive; resolution must be too.
        assert!(resolve_alias("Rojo", &nested, &global).is_ok());

        assert!(matches!(
            resolve_alias("wally", &nested, &global),
            Err(Error::ToolNotManaged(_))
        ));

        let _ = fs::remove_dir_all(&base);
    }

}
