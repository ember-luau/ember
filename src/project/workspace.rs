/*! workspaces, pesde-style. root ember.toml lists member dirs as globs under
[target] workspace, members depend on each other via { workspace = "scope/name" },
and publish/install at the root run for every member. no nested workspaces,
a member's own globs are never iterated from the outer root, same as pesde. */

use crate::error::Error;
use crate::project::manifest::{Dependency, Manifest, manifest_in};
use crate::sys::git;
use globset::{GlobBuilder, GlobSetBuilder};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Member {
    /// absolute path of the member's dir.
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Member {
    /** how commands name this member while working on it. its [package]
    name when it publishes one, its folder otherwise -- a member can be a
    plain project with no [package] table at all. the folder's own name,
    not `dir`, which `Workspace::open` made absolute and would spell out
    the user's home directory. */
    pub fn label(&self) -> String {
        self.manifest.name().map(str::to_string).unwrap_or_else(|| {
            self.dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.dir.display().to_string())
        })
    }
}

pub struct Workspace {
    /// absolute path of the root, the dir whose manifest lists the member globs.
    pub root: PathBuf,
    /** members in path order. the root itself is only included when the globs
    contain a literal ".", pesde's self-reference. */
    pub members: Vec<Member>,
}

impl Workspace {
    /** opens the workspace rooted at `root`. its manifest must define member globs.
    a matched dir without ember.toml is an error, same as pesde. */
    pub fn open(root: &Path) -> Result<Self, Error> {
        let root = absolute(root)?;
        let manifest = Manifest::load_from(&manifest_in(&root))?;
        let mut members = Vec::new();
        for dir in member_dirs(&root, manifest.workspace_members())? {
            let manifest_path = manifest_in(&dir);
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

    /// the member publishing under `name`, if any. members with no [package] table publish under no name and never match.
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members
            .iter()
            .find(|member| member.manifest.name() == Some(name))
    }

    /** the workspace of the current folder: the folder itself when its
    manifest lists members, else the root that claims it. */
    pub fn here() -> Result<Option<Self>, Error> {
        let here = Path::new(".");
        let listed = fs::read_to_string(manifest_in(here))
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
            .is_some_and(|raw| !member_globs(&raw).is_empty());
        if listed {
            return Self::open(here).map(Some);
        }
        containing(here)
    }

    /// the folder of member `index` from the root, with `/` between its parts. `.` is the root.
    pub fn path_of(&self, index: usize) -> String {
        relative_path(&self.root, &self.members[index].dir)
    }

    /** the member that a `-p` flag names: by its [package] name, else by
    the name of its folder, else by its path from the root. */
    pub fn find(&self, name: &str) -> Option<usize> {
        let by = |matches: &dyn Fn(usize, &Member) -> bool| {
            self.members
                .iter()
                .enumerate()
                .position(|(index, member)| matches(index, member))
        };
        by(&|_, member| member.manifest.name() == Some(name))
            .or_else(|| by(&|_, member| member.dir.file_name().is_some_and(|dir| dir == name)))
            .or_else(|| by(&|index, _| self.path_of(index) == name.trim_end_matches('/')))
    }

    /** for each member, the members that it depends on with `workspace =`,
    in any of its dependency tables. an entry that names no member is no
    edge: the resolver reports it at install. */
    pub fn edges(&self) -> Vec<Vec<usize>> {
        self.members
            .iter()
            .map(|member| {
                let manifest = &member.manifest;
                let mut edges: Vec<usize> = manifest
                    .all_dependencies()
                    .chain(&manifest.peer_dependencies)
                    .filter_map(|(_, dependency)| match dependency {
                        Dependency::Workspace { workspace, .. } => self
                            .members
                            .iter()
                            .position(|member| member.manifest.name() == Some(workspace)),
                        _ => None,
                    })
                    .collect();
                edges.sort_unstable();
                edges.dedup();
                edges
            })
            .collect()
    }

    /** the members in dependency order: each one after the members that it
    depends on, and in path order where no edge decides. a cycle has no
    such order, and the error names the members of one cycle. */
    pub fn order(&self) -> Result<Vec<usize>, Error> {
        let edges = self.edges();
        let mut placed = vec![false; self.members.len()];
        let mut order = Vec::new();
        while order.len() < self.members.len() {
            let next = (0..self.members.len())
                .find(|&member| !placed[member] && edges[member].iter().all(|&on| placed[on]));
            let Some(next) = next else {
                /* each member that is left depends on one that is left, so
                a walk along those edges comes back to a member it saw */
                let start = placed.iter().position(|placed| !placed).unwrap_or(0);
                let mut walk = vec![start];
                let cycle = loop {
                    let last = walk[walk.len() - 1];
                    let on = edges[last]
                        .iter()
                        .copied()
                        .find(|&on| !placed[on])
                        .unwrap_or(last);
                    if let Some(at) = walk.iter().position(|&seen| seen == on) {
                        walk.push(on);
                        break walk.split_off(at);
                    }
                    walk.push(on);
                };
                let names: Vec<String> = cycle
                    .iter()
                    .map(|&member| self.members[member].label())
                    .collect();
                return Err(std::io::Error::other(format!(
                    "The workspace members depend on each other in a cycle: {}; remove one `workspace =` dependency of the cycle",
                    names.join(" -> ")
                ))
                .into());
            };
            placed[next] = true;
            order.push(next);
        }
        Ok(order)
    }

    /// `members` and each member that depends on one of them, directly or through another member.
    pub fn with_dependents(&self, members: &[usize]) -> Vec<usize> {
        let edges = self.edges();
        let mut found = vec![false; self.members.len()];
        let mut stack = members.to_vec();
        while let Some(member) = stack.pop() {
            if std::mem::replace(&mut found[member], true) {
                continue;
            }
            stack.extend((0..edges.len()).filter(|&dependent| edges[dependent].contains(&member)));
        }
        (0..found.len()).filter(|&member| found[member]).collect()
    }

    /** the members with a file that differs from the git ref `since`, the
    files that git does not track yet included, and the members that depend
    on them. a file outside each member changes none. */
    pub fn changed(&self, since: &str) -> Result<Vec<usize>, Error> {
        // a ref is an argument of git, so it must not read as an option
        if since.starts_with('-') {
            return Err(std::io::Error::other(format!("'{since}' is not a git ref")).into());
        }
        let root = self.root.to_string_lossy();
        let git = |args: &[&str]| {
            let args = [&["-C", &*root][..], args].concat();
            git::capture_diff(&args).map_err(|error| {
                let reason = match error {
                    git::GitError::Missing => "git is not installed".to_string(),
                    git::GitError::Failed(reason) => reason,
                };
                Error::from(std::io::Error::other(format!(
                    "--changed reads the changes from git, and git failed: {reason}"
                )))
            })
        };
        // both lists are relative to the root, and hold only files under it
        let mut files = git(&["diff", "--name-only", "--relative", since, "--"])?;
        files.push_str(&git(&["ls-files", "--others", "--exclude-standard"])?);
        let changed: Vec<usize> = (0..self.members.len())
            .filter(|&member| {
                let path = self.path_of(member);
                files
                    .lines()
                    .any(|file| path == "." || Path::new(file).starts_with(&path))
            })
            .collect();
        Ok(self.with_dependents(&changed))
    }
}

/// the workspace `project_dir` belongs to, None when no root claims it.
pub fn containing(project_dir: &Path) -> Result<Option<Workspace>, Error> {
    root_of(project_dir)?
        .map(|(root, _)| Workspace::open(&root))
        .transpose()
}

/** the root that claims `project_dir`, its dir and its manifest as plain
TOML. walk up, take the first ancestor manifest whose member globs match it,
pesde's `find_roots`. ancestors that don't claim it are skipped. plain TOML,
because a member that inherits needs this root before it can load. */
pub(crate) fn root_of(project_dir: &Path) -> Result<Option<(PathBuf, toml::Table)>, Error> {
    let project_dir = absolute(project_dir)?;
    let mut current = project_dir.parent();
    while let Some(dir) = current {
        current = dir.parent();
        let manifest_path = manifest_in(dir);
        if !manifest_path.exists() {
            continue;
        }
        let raw: toml::Table = fs::read_to_string(&manifest_path)?.parse()?;
        let globs = member_globs(&raw);
        if !globs.is_empty() && member_dirs(dir, &globs)?.contains(&project_dir) {
            return Ok(Some((dir.to_path_buf(), raw)));
        }
    }
    Ok(None)
}

/** the member globs of a manifest read as plain TOML: `workspace`, or its
alias `workspace_members`, on the first target, the keys serde reads for
[`Manifest::workspace_members`]. */
pub(crate) fn member_globs(raw: &toml::Table) -> Vec<String> {
    let target = match raw.get("target") {
        Some(toml::Value::Array(targets)) => targets.first(),
        target => target,
    };
    target
        .and_then(|target| {
            target
                .get("workspace")
                .or_else(|| target.get("workspace_members"))
        })
        .and_then(toml::Value::as_array)
        .map(|globs| {
            globs
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/** dirs under `root` matching the member globs, sorted. pesde's glob extensions
apply, `!` prefix subtracts matches, a literal "." makes the root itself a member. */
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
        // literal_separator, so `*` stays within one path segment and `packages/`
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
                Some(".git" | ".ember-staging")
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

/** `path` with each `.` dropped and each `..` folded into the part before
it, by the text alone. one folder that two manifests spell in two ways is
then one path. */
pub fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// `std::path::absolute` with our error type. never canonicalize, symlink targets would break the lexical path math above.
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

    /// chief-shaped tree, private root with packages/* members.
    fn write_workspace(base: &Path) {
        let root = format!(
            "{}private = true\n\n[target]\nenvironment = \"roblox\"\n\
             workspace = [\"packages/*\", \"!packages/skipped\"]\n",
            manifest("acme/root", "0.0.0")
        );
        write(base, "ember.toml", &root);
        write(
            &base.join("packages/core"),
            "ember.toml",
            &manifest("acme/core", "1.2.0"),
        );
        write(
            &base.join("packages/extra"),
            "ember.toml",
            &manifest("acme/extra", "0.3.0"),
        );
        write(&base.join("packages/skipped"), "ember.toml", "");
        // stray file under packages/ must not count as a member.
        write(base, "packages/README.md", "not a member");
    }

    #[test]
    fn discovers_members_from_globs() {
        let base = std::env::temp_dir().join("embr-test-workspace-globs");
        let _ = fs::remove_dir_all(&base);
        write_workspace(&base);

        let workspace = Workspace::open(&base).unwrap();
        let names: Vec<_> = workspace
            .members
            .iter()
            .filter_map(|member| member.manifest.name())
            .collect();
        assert_eq!(names, ["acme/core", "acme/extra"]);
        assert!(workspace.member("acme/core").is_some());
        assert!(workspace.member("acme/skipped").is_none());

        /* a member that is a plain project, no [package] at all, is still a
        member -- it just publishes under no name, so nothing resolves a
        workspace dependency to it, and commands call it by its directory */
        write(&base.join("packages/app"), "ember.toml", "[dependencies]\n");
        let workspace = Workspace::open(&base).unwrap();
        let app = workspace
            .members
            .iter()
            .find(|member| member.dir.ends_with("app"))
            .unwrap();
        assert_eq!(app.manifest.name(), None);
        // the folder's name, not the absolute path `dir` holds
        assert_eq!(app.label(), "app");
        assert!(workspace.member("acme/app").is_none());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn members_without_manifests_are_an_error() {
        let base = std::env::temp_dir().join("embr-test-workspace-missing");
        let _ = fs::remove_dir_all(&base);
        write(
            &base,
            "ember.toml",
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
        let base = std::env::temp_dir().join("embr-test-workspace-containing");
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
        let outside = std::env::temp_dir().join("embr-test-workspace-outside");
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&outside).unwrap();
        assert!(containing(&outside).unwrap().is_none());

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&outside);
    }

    /// a root with the members a, b and c, where each entry of `edges` is (member, the member it depends on).
    fn write_graph(base: &Path, edges: &[(&str, &str)]) {
        let _ = fs::remove_dir_all(base);
        write(
            base,
            "ember.toml",
            "[target]\nenvironment = \"luau\"\nworkspace = [\"packages/*\"]\n",
        );
        for name in ["a", "b", "c"] {
            let mut text = manifest(&format!("acme/{name}"), "1.0.0");
            text.push_str("[target]\nenvironment = \"luau\"\n[dependencies]\n");
            for (_, on) in edges.iter().filter(|(member, _)| *member == name) {
                text.push_str(&format!("{on} = {{ workspace = \"acme/{on}\" }}\n"));
            }
            write(&base.join("packages").join(name), "ember.toml", &text);
        }
    }

    #[test]
    fn members_order_after_the_members_they_depend_on() {
        let base = std::env::temp_dir().join("embr-test-workspace-order");
        // path order is a, b, c. a depends on c, and c on b
        write_graph(&base, &[("a", "c"), ("c", "b")]);
        let workspace = Workspace::open(&base).unwrap();
        let labels = |members: Vec<usize>| -> Vec<String> {
            members
                .into_iter()
                .map(|member| workspace.members[member].label())
                .collect()
        };
        assert_eq!(
            labels(workspace.order().unwrap()),
            ["acme/b", "acme/c", "acme/a"]
        );
        // b changed, so c and a, which depend on it, count too
        let b = workspace.find("acme/b").unwrap();
        assert_eq!(
            labels(workspace.with_dependents(&[b])),
            ["acme/a", "acme/b", "acme/c"]
        );
        let a = workspace.find("packages/a").unwrap();
        assert_eq!(labels(workspace.with_dependents(&[a])), ["acme/a"]);
        assert!(workspace.find("acme/none").is_none());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_cycle_names_its_members() {
        let base = std::env::temp_dir().join("embr-test-workspace-cycle");
        // c is outside the cycle and only depends on it
        write_graph(&base, &[("a", "b"), ("b", "a"), ("c", "a")]);
        let error = Workspace::open(&base)
            .unwrap()
            .order()
            .unwrap_err()
            .to_string();
        assert!(error.contains("acme/a -> acme/b -> acme/a"), "{error}");
        assert!(!error.contains("acme/c"), "{error}");

        let _ = fs::remove_dir_all(&base);
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
