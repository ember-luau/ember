/*! editing side of the manifest. every command that changes lpm.toml (or the
global tools file) needs the same steps: open the right file, reach for a table,
write it back. all of it goes through `ManifestDoc` so the toml_edit dance lives
in one place and comments/formatting survive every write. */

use super::MANIFEST_FILE;
use crate::error::Error;
use crate::sys::paths;
use std::fs;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item, Table};

/// which file a command edits. most tool commands take a `--global` flag that picks between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// lpm.toml in the current directory.
    Project,
    /// ~/.lpm/tools.toml, whose tools resolve in any directory.
    Global,
}

impl Scope {
    pub fn from_global(global: bool) -> Self {
        if global {
            Scope::Global
        } else {
            Scope::Project
        }
    }

    pub fn path(self) -> Result<PathBuf, Error> {
        match self {
            Scope::Project => Ok(PathBuf::from(MANIFEST_FILE)),
            Scope::Global => paths::global_tools_file(),
        }
    }

    /// how the file is named when a command talks about it.
    pub fn label(self) -> &'static str {
        match self {
            Scope::Project => "lpm.toml",
            Scope::Global => "the global tools file",
        }
    }
}

/// a manifest opened for editing.
pub struct ManifestDoc {
    path: PathBuf,
    document: DocumentMut,
}

impl ManifestDoc {
    /** opens an existing manifest. a missing project file is the friendly
    `ManifestMissing` error rather than a raw io one; these commands get run
    outside projects all the time. */
    pub fn open(scope: Scope) -> Result<Self, Error> {
        let path = scope.path()?;
        if !path.exists() {
            return Err(Error::ManifestMissing);
        }
        let document = fs::read_to_string(&path)?.parse()?;
        Ok(ManifestDoc { path, document })
    }

    /** same, except a missing global tools file starts an empty document (it only
    exists once the first global tool is added). project manifests are still never
    created here; that's `lpm init`'s job. */
    pub fn open_or_create(scope: Scope) -> Result<Self, Error> {
        let path = scope.path()?;
        if scope == Scope::Global && !path.exists() {
            return Ok(ManifestDoc {
                path,
                document: DocumentMut::new(),
            });
        }
        Self::open(scope)
    }

    /// the table named `name`, or None when the file has no such section. a key that exists but isn't a table (`tools = 3`) is always an error.
    pub fn table(&mut self, name: &str) -> Result<Option<&mut Table>, Error> {
        match self.document.get_mut(name) {
            None => Ok(None),
            Some(item) => item
                .as_table_mut()
                .map(Some)
                .ok_or_else(|| not_a_table(name)),
        }
    }

    /// the table named `name`, erroring when the manifest doesn't have it.
    pub fn require_table(&mut self, name: &str) -> Result<&mut Table, Error> {
        self.table(name)?
            .ok_or_else(|| Error::ManifestInvalid(format!("[{name}] doesn't exist")))
    }

    /// the table named `name`, appended to the file when it's missing.
    pub fn table_or_create(&mut self, name: &str) -> Result<&mut Table, Error> {
        self.document
            .entry(name)
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| not_a_table(name))
    }

    /// drops an emptied table so removing the last entry doesn't leave a bare `[tools]` header behind.
    pub fn drop_if_empty(&mut self, name: &str) {
        if self
            .document
            .get(name)
            .and_then(Item::as_table)
            .is_some_and(Table::is_empty)
        {
            self.document.remove(name);
        }
    }

    /// writes the document back. parent dirs get created because the global tools file lives in ~/.lpm, which may not exist yet.
    pub fn save(&self) -> Result<(), Error> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, self.document.to_string())?;
        Ok(())
    }
}

fn not_a_table(name: &str) -> Error {
    Error::ManifestInvalid(format!("[{name}] is not a table"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> ManifestDoc {
        ManifestDoc {
            path: PathBuf::from(MANIFEST_FILE),
            document: text.parse().unwrap(),
        }
    }

    #[test]
    fn finds_tables_and_reports_missing_ones() {
        let mut manifest = doc("[tools]\nrojo = \"rojo-rbx/rojo@7.4.4\"\n");
        assert!(manifest.table("tools").unwrap().is_some());
        assert!(manifest.table("indices").unwrap().is_none());
        assert!(matches!(
            manifest.require_table("indices"),
            Err(Error::ManifestInvalid(_))
        ));
    }

    #[test]
    fn rejects_keys_that_are_not_tables() {
        let mut manifest = doc("tools = 3\n");
        assert!(matches!(
            manifest.table("tools"),
            Err(Error::ManifestInvalid(_))
        ));
        assert!(matches!(
            manifest.table_or_create("tools"),
            Err(Error::ManifestInvalid(_))
        ));
    }

    #[test]
    fn creating_a_table_preserves_the_rest_of_the_file() {
        let mut manifest = doc("# keep me\n[package]\nname = \"scope/name\"\n");
        manifest.table_or_create("tools").unwrap()["rojo"] =
            toml_edit::value("rojo-rbx/rojo@7.4.4");

        let written = manifest.document.to_string();
        assert!(written.starts_with("# keep me\n[package]"));
        assert!(written.contains("[tools]\nrojo = \"rojo-rbx/rojo@7.4.4\""));
    }

    #[test]
    fn empty_tables_are_dropped() {
        let mut manifest = doc("[package]\nname = \"scope/name\"\n\n[tools]\nrojo = \"a/b@1\"\n");
        manifest.require_table("tools").unwrap().remove("rojo");
        manifest.drop_if_empty("tools");
        assert!(!manifest.document.to_string().contains("[tools]"));

        // a table with entries left in it stays put.
        let mut manifest = doc("[tools]\nrojo = \"a/b@1\"\n");
        manifest.drop_if_empty("tools");
        assert!(manifest.document.to_string().contains("[tools]"));
    }
}
