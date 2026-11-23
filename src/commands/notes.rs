/*! `embr notes`: the release notes of one published version. the author
writes them, and the registry shows them above the changelog. `embr publish
--notes` sends the same file in the same way. */

use crate::commands::yank::as_owner;
use crate::error::Error;
use crate::net::registry;
use crate::project::manifest::split_package_name;
use crate::ui;
use clap::Args;
use std::path::{Path, PathBuf};

/// the registry's cap on release notes, in bytes.
const MAX_NOTES_BYTES: usize = 20_000;

#[derive(Args, Debug)]
pub struct NotesArgs {
    /// The version, as scope/name@version
    pub package: String,
    /// Markdown file with the release notes
    #[arg(required_unless_present = "remove", conflicts_with = "remove")]
    pub file: Option<PathBuf>,
    /// Remove the release notes of the version
    #[arg(long)]
    pub remove: bool,
    /// Print the result as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

/** the text of a notes file. the registry takes 1 to 20,000 bytes of
UTF-8, and a file outside that stops here, before any call. */
pub(crate) fn read(path: &Path) -> Result<String, Error> {
    let invalid =
        |reason: String| Error::NotesInvalid(format!("The notes file {} {reason}", path.display()));
    let bytes = std::fs::read(path).map_err(|error| invalid(format!("did not open: {error}")))?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("is not UTF-8 text".to_string()))?;
    if text.is_empty() || text.len() > MAX_NOTES_BYTES {
        return Err(invalid(format!(
            "has {} bytes; release notes have 1 to {MAX_NOTES_BYTES} bytes",
            text.len()
        )));
    }
    Ok(text)
}

/// sets the release notes of a version, or removes them. a second set replaces the first.
pub fn run(args: NotesArgs) -> Result<(), Error> {
    ui::json_or_text(args.json, || {
        let (name, version) = args.package.split_once('@').ok_or_else(|| {
            Error::NotesInvalid(format!(
                "'{}' names no version; write it as scope/name@version",
                args.package
            ))
        })?;
        split_package_name(name)?;
        let version = semver::Version::parse(version)?;
        let body = args.file.as_deref().map(read).transpose()?;

        as_owner(|token| {
            ui::with_spinner("Contacting the registry", || {
                registry::notes(Some(token), None, name, &version, body.as_deref())
            })
        })?;

        match &body {
            Some(_) => ui::print_success(&format!("Set the release notes of {name}@{version}")),
            None => ui::print_success(&format!("Removed the release notes of {name}@{version}")),
        }
        Ok(serde_json::json!({
            "version": ui::JSON_VERSION,
            "name": name,
            "package_version": version.to_string(),
            "removed": body.is_none(),
            "bytes": body.as_ref().map(String::len),
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notes_file_has_1_to_20000_bytes_of_utf8() {
        let dir = std::env::temp_dir().join("embr-test-notes-file");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        assert_eq!(read(&file("ok.md", b"# 1.0\n")).unwrap(), "# 1.0\n");
        assert!(read(&file("full.md", &[b'a'; MAX_NOTES_BYTES])).is_ok());
        for (name, bytes, says) in [
            ("empty.md", &b""[..], "has 0 bytes"),
            (
                "long.md",
                &[b'a'; MAX_NOTES_BYTES + 1][..],
                "has 20001 bytes",
            ),
            ("binary.md", &[0xff, 0xfe][..], "is not UTF-8"),
        ] {
            let error = read(&file(name, bytes)).unwrap_err().to_string();
            assert!(error.contains(says), "{error}");
        }
        assert!(read(&dir.join("missing.md")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
