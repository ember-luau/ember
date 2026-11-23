/*! `embr export` and `embr import`: the archives of a lockfile as a folder
that moves to a computer with no network. ember.lock holds the URL of each
archive, so a locked install needs the archives and no index. */

use crate::error::Error;
use crate::project::lockfile::{Lockfile, lockfile_path};
use crate::registry::index::{self, DownloadSource, Expect};
use crate::sys::hash::{fnv1a, is_sha256_hex, sha256_hex};
use crate::ui;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// the file of a bundle that maps each URL to its archive and SHA-256.
const BUNDLE_FILE: &str = "bundle.json";

/// the newest bundle format that this embr reads. a new key is additive.
const BUNDLE_VERSION: u32 = 1;

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Folder that receives the archives, ember.lock and bundle.json
    pub dir: PathBuf,
    /// Print the result as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// Folder that `embr export` wrote
    pub dir: PathBuf,
    /// Print the result as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize, Deserialize)]
struct Bundle {
    version: u32,
    archives: Vec<Archive>,
}

#[derive(Serialize, Deserialize)]
struct Archive {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    url: String,
    /// the archive, relative to the bundle folder, with `/` between its parts.
    file: String,
    sha256: String,
}

fn fail(message: String) -> Error {
    std::io::Error::other(message).into()
}

/** copies the archive of each registry package in ember.lock to the
folder. each archive comes from where an install reads it, and its hash is
checked against the lock first, so a changed archive never gets into a
bundle. a member, a path folder and a git commit have no archive. */
pub fn export(args: ExportArgs) -> Result<(), Error> {
    let lock = Lockfile::load()?;
    fs::create_dir_all(args.dir.join("archives"))?;

    let mut archives = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = BTreeSet::new();
    let mut total = 0;
    for package in &lock.packages {
        let (url, extension) = match &package.source {
            DownloadSource::Zip { url } => (url, "zip"),
            DownloadSource::TarGz { url } => (url, "tar.gz"),
            DownloadSource::Workspace { .. }
            | DownloadSource::Path { .. }
            | DownloadSource::Git { .. } => {
                skipped.push(package.id());
                continue;
            }
        };
        // a package in two environments is one archive
        if !seen.insert(url) {
            continue;
        }
        let expect = package.sha256.as_deref().map(|sha256| Expect {
            name: &package.name,
            version: &package.version,
            sha256,
            in_lock: true,
        });
        let (bytes, sha256) = index::archive_bytes(&package.source, expect.as_ref())?;
        // the name is for a reader. the hash of the URL keeps two archives apart
        let label: String = format!("{}-{}", package.name, package.version)
            .chars()
            .map(|c| match c {
                c if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' => c,
                _ => '-',
            })
            .collect();
        let file = format!(
            "archives/{label}-{:016x}.{extension}",
            fnv1a(url.as_bytes())
        );
        fs::write(args.dir.join(&file), &bytes)?;
        total += bytes.len() as u64;
        archives.push(Archive {
            name: package.name.clone(),
            version: package.version.clone(),
            url: url.clone(),
            file,
            sha256,
        });
    }
    fs::copy(lockfile_path(), args.dir.join("ember.lock"))?;
    let count = archives.len();
    let bundle = Bundle {
        version: BUNDLE_VERSION,
        archives,
    };
    let text = serde_json::to_string_pretty(&bundle).map_err(std::io::Error::from)?;
    fs::write(args.dir.join(BUNDLE_FILE), text + "\n")?;

    if args.json {
        return ui::print_json(&json!({
            "version": ui::JSON_VERSION,
            "dir": args.dir,
            "archives": count,
            "bytes": total,
            "skipped": skipped,
        }));
    }
    if !skipped.is_empty() {
        println!(
            "Not in the bundle, because a workspace member, a path folder and a git commit have no archive: {}",
            skipped.join(", ")
        );
    }
    ui::print_success(&format!(
        "Exported {count} archive{} to {} ({:.1} MiB)",
        ui::plural(count),
        args.dir.display(),
        total as f64 / (1024.0 * 1024.0)
    ));
    Ok(())
}

/** puts each archive of the bundle where an install looks for it. the
bundle is a folder from another computer, so bundle.json is checked before
a byte moves: no path may leave the folder, and an archive whose SHA-256
differs from bundle.json stops the import before that archive is stored. */
pub fn import(args: ImportArgs) -> Result<(), Error> {
    let path = args.dir.join(BUNDLE_FILE);
    let text = fs::read_to_string(&path).map_err(|error| {
        fail(format!(
            "{} does not read ({error}); give the folder that `embr export` wrote",
            path.display()
        ))
    })?;
    let bundle: Bundle = serde_json::from_str(&text)
        .map_err(|error| fail(format!("{} does not parse: {error}", path.display())))?;
    if bundle.version > BUNDLE_VERSION {
        return Err(fail(format!(
            "{} has format {}, and this embr reads up to {BUNDLE_VERSION}; run `embr self update`",
            path.display(),
            bundle.version
        )));
    }

    let root = args.dir.canonicalize()?;
    let mut files = Vec::new();
    for archive in &bundle.archives {
        files.push(archive_file(&root, archive)?);
    }
    let mut total = 0;
    for (archive, file) in bundle.archives.iter().zip(&files) {
        let bytes = fs::read(file)?;
        let received = sha256_hex(&bytes);
        if received != archive.sha256 {
            return Err(fail(format!(
                "{} has SHA-256 {received}, and bundle.json records {}; the bundle changed after `embr export` wrote it, so export it again",
                archive.file, archive.sha256
            )));
        }
        index::import_archive(&archive.url, &bytes)?;
        total += bytes.len() as u64;
    }

    let count = bundle.archives.len();
    if args.json {
        return ui::print_json(
            &json!({ "version": ui::JSON_VERSION, "imported": count, "bytes": total }),
        );
    }
    ui::print_success(&format!(
        "Imported {count} archive{} ({:.1} MiB). `embr install --offline --locked` installs them.",
        ui::plural(count),
        total as f64 / (1024.0 * 1024.0)
    ));
    Ok(())
}

/** the file of `archive` inside the bundle folder `root`. bundle.json is
text that anyone can edit, so a path with `..`, a root or a drive is
refused, and so is a link that leads out of the folder. */
fn archive_file(root: &Path, archive: &Archive) -> Result<PathBuf, Error> {
    let leaves = || {
        fail(format!(
            "bundle.json names '{}', a path that leaves the bundle folder",
            ui::printable(&archive.file)
        ))
    };
    let plain = !archive.file.is_empty()
        && !archive.file.contains(['\\', ':'])
        && Path::new(&archive.file)
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
    if !plain {
        return Err(leaves());
    }
    if !is_sha256_hex(&archive.sha256) {
        return Err(fail(format!(
            "bundle.json has no SHA-256 for '{}'",
            ui::printable(&archive.file)
        )));
    }
    let file = root.join(&archive.file).canonicalize().map_err(|error| {
        fail(format!(
            "{} is not in the bundle: {error}",
            ui::printable(&archive.file)
        ))
    })?;
    if !file.starts_with(root) {
        return Err(leaves());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_path_stays_in_the_bundle() {
        let root = std::env::temp_dir().join("embr-test-bundle-paths");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("archives")).unwrap();
        fs::write(root.join("archives/a.tar.gz"), b"a").unwrap();
        let root = root.canonicalize().unwrap();
        let archive = |file: &str| Archive {
            name: String::new(),
            version: String::new(),
            url: "https://example.com/a".to_string(),
            file: file.to_string(),
            sha256: sha256_hex(b"a"),
        };

        assert!(archive_file(&root, &archive("archives/a.tar.gz")).is_ok());
        for bad in [
            "../a.tar.gz",
            "archives/../../a.tar.gz",
            "/etc/passwd",
            "C:\\a",
            "archives\\..\\a",
            "",
        ] {
            let error = archive_file(&root, &archive(bad)).unwrap_err().to_string();
            assert!(error.contains("leaves the bundle folder"), "{bad}: {error}");
        }
        let mut no_hash = archive("archives/a.tar.gz");
        no_hash.sha256 = "abc".to_string();
        assert!(archive_file(&root, &no_hash).is_err());

        let _ = fs::remove_dir_all(&root);
    }
}
