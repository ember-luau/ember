/*! workspaces, pesde-style: root lpm.toml lists member dirs as globs under
[target] workspace, members depend on each other via { workspace = "scope/name" },
and publish/install at the root run for every member. no nested workspaces:
a member's own globs are never iterated from the outer root, same as pesde. */

use crate::error::Error;
use crate::project::manifest::{MANIFEST_FILE, Manifest};
use globset::{GlobBuilder, GlobSetBuilder};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Member {
    /// absolute path of the member's dir.
    pub dir: PathBuf,
    pub manifest: Manifest,
}

pub struct Workspace {
    /// absolute path of the root, the dir whose manifest lists the member globs.
    pub root: PathBuf,
    /** members in path order. the root itself is only included when the globs
    contain a literal "." (pesde's self-reference). */
    pub members: Vec<Member>,
}

impl Workspace {
    /** opens the workspace rooted at `root`; its manifest must define member globs.
    a matched dir without lpm.toml is an error, same as pesde. */
    pub fn open(root: &Path) -> Result<Self, Error> {
        let root = absolute(root)?;
        let manifest = Manifest::load_from(&root.join(MANIFEST_FILE))?;
        let mut members = Vec::new();
        for dir in member_dirs(&root, manifest.workspace_members())? {
            let manifest_path = dir.join(MANIFEST_FILE);
            if !manifest_path.exists() {
                return Err(Error::WorkspaceMemberMissingManifest(dir));
            }
            members.push(Member {
                manifest: Manifest::load_from(&manifest_path)?,
                dir,
            });
        }
        Ok(Workspace { root, members })
    }

    /// the member publishing under `name`, if any.
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members
            .iter()
            .find(|member| member.manifest.package.name == name)
    }
}

/** the workspace `project_dir` belongs to: walk up, take the first ancestor manifest
whose member globs match it (pesde's `find_roots`). ancestors that don't claim it
are skipped; None when nothing does. */
pub fn containing(project_dir: &Path) -> Result<Option<Workspace>, Error> {
    let project_dir = absolute(project_dir)?;
    let mut current = project_dir.parent();
    while let Some(dir) = current {
        current = dir.parent();
        let manifest_path = dir.join(MANIFEST_FILE);
        if !manifest_path.exists() {
            continue;
        }
        let manifest = Manifest::load_from(&manifest_path)?;
        if manifest.workspace_members().is_empty() {
            continue;
        }
        if member_dirs(dir, manifest.workspace_members())?.contains(&project_dir) {
            return Ok(Some(Workspace::open(dir)?));
        }
    }
    Ok(None)
}

/** dirs under `root` matching the member globs, sorted. pesde's glob extensions:
`!` prefix subtracts matches, a literal "." makes the root itself a member. */
fn member_dirs(root: &Path, globs: &[String]) -> Result<Vec<PathBuf>, Error> {
    let invalid = |glob: &str, error: globset::Error| Error::WorkspaceGlobInvalid {
        glob: glob.to_string(),
        reason: error.to_string(),
    };

    let mut positive = GlobSetBuilder::new();
    let mut negative = GlobSetBuilder::new();
    let mut include_root = false;
    for entry in globs {
        if entry == "." {
            include_root = true;
            continue;
        }
        let (builder, pattern) = match entry.strip_prefix('!') {
            Some(negated) => (&mut negative, negated),
            None => (&mut positive, entry.as_str()),
        };
        // literal_separator: `*` stays within one path segment, so `packages/`
        // + `*` won't swallow packages/core/src. `**` crosses, wax-style.
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|error| invalid(entry, error))?;
        builder.add(glob);
    }
    let positive = positive.build().map_err(|error| invalid("", error))?;
    let negative = negative.build().map_err(|error| invalid("", error))?;

    let mut dirs = Vec::new();
    if include_root {
        dirs.push(root.to_path_buf());
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | ".lpm-staging")
            ) {
                continue;
            }
            let relative = path.strip_prefix(root).expect("walk stays under root");
            if positive.is_match(relative) && !negative.is_match(relative) {
                dirs.push(path.clone());
            }
            stack.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

/** lexical forward-slash path from dir `from` to `to`. no filesystem access, so
both must be absolute or share the same base. link files use this to require a
workspace member from an output folder. */
pub fn relative_path(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(from, to)| from == to)
        .count();

    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// `std::path::absolute` with our error type. never canonicalize: symlink targets would break the lexical path math above.
fn absolute(path: &Path) -> Result<PathBuf, Error> {
    Ok(std::path::absolute(path)?)
}

/** runs `f` with the process cwd set to `dir`, restored afterwards even on error.
lets workspace-wide install/publish reuse the cwd-relative command logic per member. */
pub fn in_dir<T>(dir: &Path, f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    let previous = std::env::current_dir()?;
    std::env::set_current_dir(dir)?;
    let result = f();
    std::env::set_current_dir(previous)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, file: &str, contents: &str) {
        let path = dir.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn manifest(name: &str, version: &str) -> String {
        format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n")
    }

    /// chief-shaped tree: private root with packages/* members.
    fn write_workspace(base: &Path) {
        let root = format!(
            "{}private = true\n\n[target]\nenvironment = \"shared\"\n\
             workspace = [\"packages/*\", \"!packages/skipped\"]\n",
            manifest("acme/root", "0.0.0")
        );
        write(base, "lpm.toml", &root);
        write(
            &base.join("packages/core"),
            "lpm.toml",
            &manifest("acme/core", "1.2.0"),
        );
        write(
            &base.join("packages/extra"),
            "lpm.toml",
            &manifest("acme/extra", "0.3.0"),
        );
        write(&base.join("packages/skipped"), "lpm.toml", "");
        // stray file under packages/ must not count as a member.
        write(base, "packages/README.md", "not a member");
    }

    #[test]
    fn discovers_members_from_globs() {
        let base = std::env::temp_dir().join("lpm-test-workspace-globs");
        let _ = fs::remove_dir_all(&base);
        write_workspace(&base);

        let workspace = Workspace::open(&base).unwrap();
        let names: Vec<_> = workspace
            .members
            .iter()
            .map(|member| member.manifest.package.name.as_str())
            .collect();
        assert_eq!(names, ["acme/core", "acme/extra"]);
        assert!(workspace.member("acme/core").is_some());
        assert!(workspace.member("acme/skipped").is_none());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn members_without_manifests_are_an_error() {
        let base = std::env::temp_dir().join("lpm-test-workspace-missing");
        let _ = fs::remove_dir_all(&base);
        write(
            &base,
            "lpm.toml",
            &format!(
                "{}\n[target]\nenvironment = \"shared\"\nworkspace = [\"packages/*\"]\n",
                manifest("acme/root", "0.0.0")
            ),
        );
        fs::create_dir_all(base.join("packages/empty")).unwrap();

        assert!(matches!(
            Workspace::open(&base),
            Err(Error::WorkspaceMemberMissingManifest(_))
        ));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn containing_walks_up_to_the_claiming_root() {
        let base = std::env::temp_dir().join("lpm-test-workspace-containing");
        let _ = fs::remove_dir_all(&base);
        write_workspace(&base);

        let workspace = containing(&base.join("packages/core")).unwrap().unwrap();
        assert_eq!(workspace.root, absolute(&base).unwrap());

        // excluded member and an unrelated dir belong to no workspace.
        assert!(
            containing(&base.join("packages/skipped"))
                .unwrap()
                .is_none()
        );
        let outside = std::env::temp_dir().join("lpm-test-workspace-outside");
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&outside).unwrap();
        assert!(containing(&outside).unwrap().is_none());

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn relative_paths_are_lexical() {
        let relative = |from: &str, to: &str| relative_path(Path::new(from), Path::new(to));
        // output folder to a sibling member of the same root.
        assert_eq!(relative("packages/shared", "packages/core"), "../core");
        // member's output folder up to a sibling member.
        assert_eq!(relative("pkg/packages/shared", "core"), "../../../core");
        assert_eq!(relative("a", "a"), ".");
        assert_eq!(relative("a/b", "a/b/c"), "c");
    }
}
