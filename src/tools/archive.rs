/*! Turning a GitHub release into a binary: which asset fits this platform,
how to unpack it, and which of the unpacked files is the tool. */

use crate::error::Error;
use crate::net::http::responses::Asset;
use std::fs;
use std::path::{Path, PathBuf};

/// checksum/signature companions that must never be picked as the platform asset.
const METADATA_SUFFIXES: &[&str] = &[
    ".sha256",
    ".sha256sum",
    ".sig",
    ".asc",
    ".txt",
    ".json",
    ".pem",
];

/** Asset-name spellings for an OS. matching is word-start anchored (see
`contains_word_start`) so the "win" alias doesn't hit "darwin". */
fn os_aliases(os: &str) -> &'static [&'static str] {
    match os {
        "windows" => &["windows", "win64", "win32", "win"],
        "macos" => &["macos", "darwin", "apple", "osx"],
        "linux" => &["linux", "musl"],
        _ => &[],
    }
}

/// asset-name spellings for a CPU arch.
fn arch_aliases(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["x86_64", "amd64", "x64"],
        "aarch64" => &["aarch64", "arm64"],
        _ => &[],
    }
}

/** Picks the release asset for an os/arch pair (std::env::consts values).
prefers a full OS+arch match, falling back to OS-only since many tools
ship one arch per OS. None means nothing fits. */
pub fn select_asset<'a>(assets: &'a [Asset], os: &str, arch: &str) -> Option<&'a Asset> {
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

/** True when `needle` sits at a word start in `haystack` (string start or
right after a non-alphanumeric). keeps short aliases honest: "win"
matches "win64" and "tool-win.zip" but not "darwin". */
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

/** Unpacks a downloaded asset into `dest`. assets that aren't archives are
the executable itself and get written to `raw_target` as-is. */
pub fn extract(
    asset_name: &str,
    bytes: &[u8],
    dest: &Path,
    raw_target: &Path,
) -> Result<(), Error> {
    match kind(asset_name, bytes) {
        Kind::Zip => {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec()))?;
            archive.extract(dest)?;
        }
        Kind::TarGz => {
            let decoder = flate2::read::GzDecoder::new(bytes);
            tar::Archive::new(decoder).unpack(dest)?;
        }
        Kind::Tar => {
            tar::Archive::new(bytes).unpack(dest)?;
        }
        Kind::Raw => {
            fs::write(raw_target, bytes)?;
        }
    }
    Ok(())
}

/// how a release asset's bytes get unpacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Zip,
    TarGz,
    Tar,
    /// not an archive: the asset is the executable itself.
    Raw,
}

/** Decides how to unpack an asset. content is magic-byte sniffed rather
than trusted from the file name because ureq transparently decodes
Content-Encoding: gzip (see index::download); only plain tar, which has
no leading magic, falls back to the name. */
fn kind(name: &str, bytes: &[u8]) -> Kind {
    if bytes.starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
        Kind::Zip
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        Kind::TarGz
    } else if name.to_ascii_lowercase().ends_with(".tar") {
        Kind::Tar
    } else {
        Kind::Raw
    }
}

/// every regular file under `dir` with its size, recursively.
pub fn collect_files(dir: &Path, files: &mut Vec<(PathBuf, u64)>) -> Result<(), Error> {
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

/** Picks the tool's executable among the extracted files. on platforms with
an exe suffix (windows) files carrying it are preferred; within the pool
a file named after the repo or alias wins, else the largest file
(archives bundle READMEs, licenses, completions). */
pub fn pick_executable(
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
        /* the .sha256 companion sorts before the real zip to prove metadata
        files get skipped even when their names match the platform. */
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
        // no macos x86_64 build exists: fall back to the OS-only match.
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
        assert_eq!(kind("tool.zip", b"PK\x03\x04rest of zip"), Kind::Zip);
        assert_eq!(kind("tool.tar.gz", &[0x1f, 0x8b, 0x08, 0x00]), Kind::TarGz);
        /* magic bytes win over the name: a ".tar" that's really gzipped
        (ureq didn't decode it) still gets gunzipped. */
        assert_eq!(kind("tool.tar", &[0x1f, 0x8b, 0x08, 0x00]), Kind::TarGz);
        assert_eq!(kind("tool.tar", b"plain tar header bytes"), Kind::Tar);
        assert_eq!(kind("tool.exe", b"MZ\x90\x00"), Kind::Raw);
        assert_eq!(
            kind("tool-linux-x86_64", &[0x7f, b'E', b'L', b'F']),
            Kind::Raw
        );
    }

    #[test]
    fn raw_assets_are_written_as_the_executable() {
        let base = std::env::temp_dir().join("lpm-test-archive-raw");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let target = base.join("tool");
        extract("tool-linux-x86_64", b"\x7fELF binary", &base, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"\x7fELF binary");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn picks_executables_from_extracted_files() {
        // unix: a file named after the tool beats the largest file.
        let files = vec![
            (PathBuf::from("staging/README.md"), 10_000),
            (PathBuf::from("staging/bin/stylua"), 2_000),
            (PathBuf::from("staging/LICENSE"), 1_000),
        ];
        assert_eq!(
            pick_executable(&files, "stylua", "stylua", "").unwrap(),
            PathBuf::from("staging/bin/stylua")
        );

        // the alias counts as a name match too.
        let files = vec![(PathBuf::from("staging/fmt"), 5)];
        assert_eq!(
            pick_executable(&files, "some-formatter", "fmt", "").unwrap(),
            PathBuf::from("staging/fmt")
        );

        // windows: .exe files win over everything else.
        let files = vec![
            (PathBuf::from("staging/README.md"), 10_000),
            (PathBuf::from("staging/rojo.exe"), 5_000),
        ];
        assert_eq!(
            pick_executable(&files, "rojo", "rojo", ".exe").unwrap(),
            PathBuf::from("staging/rojo.exe")
        );

        // no name match anywhere: the largest file wins.
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
}
