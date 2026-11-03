/*! `embr tree` and `embr why`. ember.lock records no edges, so both read
the graph that install left on disk. The manifest's dependencies are the
roots, and each installed package's own manifest gives its children. */

use crate::commands::install::stored_at;
use crate::error::Error;
use crate::project::lockfile::{LockedPackage, Lockfile};
use crate::project::manifest::{Environment, Manifest, manifest_in};
use crate::project::package;
use clap::Args;
use crossterm::style::Stylize;
use std::path::Path;

#[derive(Args, Debug)]
pub struct TreeArgs {
    /// Levels to show under the direct dependencies, where 0 shows the direct dependencies only
    #[arg(long)]
    pub depth: Option<usize>,
}

#[derive(Args, Debug)]
pub struct WhyArgs {
    /// Package to explain, as scope/name or as the alias that a dependent gives it
    pub package: String,
}

pub fn tree(args: TreeArgs) -> Result<(), Error> {
    let (manifest, lock) = load()?;
    let graph = Graph::read(&manifest, &lock, Path::new(""))?;
    if graph.roots.is_empty() {
        println!("ember.toml has no dependencies");
        return Ok(());
    }
    print!("{}", graph.tree(args.depth));
    Ok(())
}

pub fn why(args: WhyArgs) -> Result<(), Error> {
    let (manifest, lock) = load()?;
    let graph = Graph::read(&manifest, &lock, Path::new(""))?;
    let paths = graph.paths_to(args.package.trim());
    if paths.is_empty() {
        return Err(Error::NotADependency(args.package));
    }
    print!("{}", graph.why(&paths));
    Ok(())
}

/// the manifest and the lock of the project here. `embr notices` reads them too.
pub(crate) fn load() -> Result<(Manifest, Lockfile), Error> {
    let manifest = Manifest::load()?;
    let lock = match Lockfile::load() {
        Err(Error::LockfileMissing) => {
            return Err(Error::NotInstalledYet("ember.lock is missing".to_string()));
        }
        lock => lock?,
    };
    Ok((manifest, lock))
}

/** for each package of `lock`, the aliases of the manifest entries whose
tree holds it, in manifest order, for the project in `project`. None when
the installed graph does not read, as before an install. */
pub(crate) fn brought_in_by(
    manifest: &Manifest,
    lock: &Lockfile,
    project: &Path,
) -> Option<Vec<Vec<String>>> {
    let graph = Graph::read(manifest, lock, project).ok()?;
    let mut by = vec![Vec::<String>::new(); graph.packages.len()];
    for root in &graph.roots {
        let mut seen = vec![false; graph.packages.len()];
        let mut stack = vec![root.node];
        while let Some(node) = stack.pop() {
            if std::mem::replace(&mut seen[node], true) {
                continue;
            }
            if !by[node].contains(&root.alias) {
                by[node].push(root.alias.clone());
            }
            stack.extend(graph.children[node].iter().map(|(_, child)| *child));
        }
    }
    Some(by)
}

/// one manifest entry, and the installed package it became.
struct Root {
    alias: String,
    node: usize,
    dev: bool,
}

/// one hop of a path: the alias the parent gives the package, and the package.
type Hop<'graph> = (&'graph str, usize);

/** the installed graph. a node is an index into `packages`, and
`children[node]` holds the edges of that package as (alias, node). */
struct Graph<'lock> {
    packages: &'lock [LockedPackage],
    roots: Vec<Root>,
    children: Vec<Vec<(String, usize)>>,
}

impl<'lock> Graph<'lock> {
    /** the graph of the project in `project`, as install wrote it. a child
    is found by (name, context), as install links it, so each environment
    root stays a tree of its own. */
    fn read(manifest: &Manifest, lock: &'lock Lockfile, project: &Path) -> Result<Self, Error> {
        let packages = &lock.packages[..];

        let mut roots = Vec::new();
        for (dev, entries) in [
            (false, &manifest.dependencies),
            (true, &manifest.dev_dependencies),
        ] {
            for (alias, dependency) in entries {
                let name = dependency.package();
                let direct = |package: &&LockedPackage| {
                    package.link.is_some() && package.name.eq_ignore_ascii_case(name)
                };
                /* the link is the alias. a second alias for a package that
                the first one brought in gets no link, so the name decides */
                let node = packages
                    .iter()
                    .position(|package| direct(&package) && package.link.as_deref() == Some(alias))
                    .or_else(|| packages.iter().position(|package| direct(&package)))
                    .ok_or_else(|| {
                        Error::NotInstalledYet(format!("ember.lock has no {name} for '{alias}'"))
                    })?;
                roots.push(Root {
                    alias: alias.clone(),
                    node,
                    dev,
                });
            }
        }

        let mut children = Vec::new();
        for package in packages {
            let (storage, in_place) = stored_at(manifest, package);
            let storage = project.join(storage);
            if !storage.is_dir() {
                return Err(Error::NotInstalledYet(format!(
                    "{} is not in {}",
                    package.name,
                    storage.display()
                )));
            }
            /* a member is the source tree itself. its manifest can name
            another member with `workspace`, which the reader of shipped
            manifests skips, and the resolver walks those edges too */
            let declared = if in_place {
                Manifest::load_from(&manifest_in(&storage))?
                    .dependencies
                    .iter()
                    .map(|(alias, dependency)| (alias.clone(), dependency.package().to_lowercase()))
                    .collect()
            } else {
                package::declared_dependencies(&storage)
            };
            let edges = declared
                .into_iter()
                .filter_map(|(alias, declared)| {
                    // an [overrides] redirect on this edge installed the replacement
                    let name = package.redirects.get(&alias).unwrap_or(&declared);
                    let child = packages.iter().position(|child| {
                        child.name.eq_ignore_ascii_case(name)
                            && child.context() == package.context()
                    })?;
                    Some((alias, child))
                })
                .collect();
            children.push(edges);
        }

        Ok(Graph {
            packages,
            roots,
            children,
        })
    }

    /// the environment roots that the direct dependencies install under.
    fn contexts(&self) -> Vec<Environment> {
        let mut contexts: Vec<Environment> = self
            .roots
            .iter()
            .map(|root| self.packages[root.node].context())
            .collect();
        contexts.sort();
        contexts.dedup();
        contexts
    }

    /** `scope/name@version`. the alias follows when it is not the short
    name, because the alias names the link file. */
    fn label(&self, node: usize, alias: &str) -> String {
        let package = &self.packages[node];
        let mut label = format!("{}@{}", package.name, package.version);
        if package.name.rsplit('/').next() != Some(alias) {
            label.push_str(&format!(" as '{alias}'").dark_grey().to_string());
        }
        label
    }

    /** the tree text. a package whose children were shown already gets
    `(*)` in place of them, as `cargo tree` does. */
    fn tree(&self, depth: Option<usize>) -> String {
        let mut out = String::new();
        let mut shown = vec![false; self.packages.len()];
        let contexts = self.contexts();
        for &context in &contexts {
            let roots: Vec<&Root> = self
                .roots
                .iter()
                .filter(|root| self.packages[root.node].context() == context)
                .collect();
            // the environment heads its roots only when there is more than one
            let grouped = contexts.len() > 1;
            if grouped {
                out.push_str(&format!("{}\n", context.as_str().bold()));
            }
            for (index, root) in roots.iter().enumerate() {
                let mut label = self.label(root.node, &root.alias);
                if root.dev {
                    label.push_str(&" (dev)".dark_grey().to_string());
                }
                let at = grouped.then_some(("", index + 1 == roots.len()));
                self.branch(&mut out, at, root.node, label, depth, &mut shown);
            }
        }
        out
    }

    /** one package and the levels under it. `at` is the prefix of the
    line and whether it is the last of its parent. None is the top level,
    which takes no branch line. */
    fn branch(
        &self,
        out: &mut String,
        at: Option<(&str, bool)>,
        node: usize,
        label: String,
        depth: Option<usize>,
        shown: &mut [bool],
    ) {
        let children = &self.children[node];
        let repeated = shown[node] && !children.is_empty();
        let (prefix, line, below) = match at {
            None => ("", "", ""),
            Some((prefix, true)) => (prefix, "└── ", "    "),
            Some((prefix, false)) => (prefix, "├── ", "│   "),
        };
        let mark = if repeated { " (*)" } else { "" };
        out.push_str(&format!("{prefix}{line}{label}{}\n", mark.dark_grey()));
        if repeated || depth == Some(0) {
            return;
        }
        shown[node] = true;
        let prefix = format!("{prefix}{below}");
        for (index, (alias, child)) in children.iter().enumerate() {
            let label = self.label(*child, alias);
            let at = Some((prefix.as_str(), index + 1 == children.len()));
            let depth = depth.map(|depth| depth - 1);
            self.branch(out, at, *child, label, depth, shown);
        }
    }

    /** every path from a manifest entry down to a package that `wanted`
    names, by its scope/name or by the alias of the last hop. */
    // ponytail: every path, so many diamonds print many lines. dedupe by parent if that grows too long
    fn paths_to(&self, wanted: &str) -> Vec<(&Root, Vec<Hop<'_>>)> {
        let mut found = Vec::new();
        for root in &self.roots {
            let mut path = vec![(root.alias.as_str(), root.node)];
            self.walk(&mut path, wanted, &mut |path| found.push((root, path)));
        }
        found
    }

    fn walk<'graph>(
        &'graph self,
        path: &mut Vec<Hop<'graph>>,
        wanted: &str,
        found: &mut impl FnMut(Vec<Hop<'graph>>),
    ) {
        let &(alias, node) = path.last().expect("a path starts at its root");
        if alias == wanted || self.packages[node].name.eq_ignore_ascii_case(wanted) {
            found(path.clone());
            return;
        }
        for (alias, child) in &self.children[node] {
            // a cycle would walk for ever
            if path.iter().any(|&(_, node)| node == *child) {
                continue;
            }
            path.push((alias, *child));
            self.walk(path, wanted, found);
            path.pop();
        }
    }

    /// the why text: each package that matched, then each path to it.
    fn why(&self, paths: &[(&Root, Vec<Hop<'_>>)]) -> String {
        let grouped = self.contexts().len() > 1;
        let mut targets: Vec<usize> = Vec::new();
        for (_, path) in paths {
            let (_, target) = path[path.len() - 1];
            if !targets.contains(&target) {
                targets.push(target);
            }
        }

        let mut out = String::new();
        for target in targets {
            let package = &self.packages[target];
            let mut head = format!("{}@{}", package.name, package.version);
            if grouped {
                head.push_str(&format!(" in {}", package.context()));
            }
            out.push_str(&format!("{}\n", head.bold()));
            for (root, path) in paths
                .iter()
                .filter(|(_, path)| path[path.len() - 1].1 == target)
            {
                let mut line = vec![if root.dev {
                    "ember.toml [dev-dependencies]".to_string()
                } else {
                    "ember.toml".to_string()
                }];
                line.extend(path.iter().map(|&(alias, node)| self.label(node, alias)));
                out.push_str(&format!("  {}\n", line.join(" > ")));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write(dir: &Path, file: &str, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(file), contents).unwrap();
    }

    /// drops the colour escapes, so a test reads the text alone.
    fn plain(text: &str) -> String {
        let mut out = String::new();
        let mut characters = text.chars();
        while let Some(character) = characters.next() {
            if character == '\x1b' {
                characters.by_ref().find(|&escape| escape == 'm');
            } else {
                out.push(character);
            }
        }
        out
    }

    const MANIFEST: &str = "[target]\nenvironment = \"roblox\"\n\
        [dependencies]\n\
        Core = { name = \"chief/core\", version = \"^\" }\n\
        signal = { name = \"acme/signal\", version = \"^\", target = \"roblox_server\" }\n\
        tools = { workspace = \"acme/tools\" }\n\
        [dev-dependencies]\n\
        lest = { name = \"alloy/lest\", version = \"^\" }\n";

    /* core needs traits and helper, helper needs traits, traits needs
    util. so traits shows twice, and the second time it is (*) */
    const LOCK: &str = "version = 2\n\
        [[package]]\nname = \"chief/core\"\nversion = \"0.3.0\"\nenvironment = \"roblox\"\n\
        link = \"Core\"\nindex = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n\
        [[package]]\nname = \"chief/helper\"\nversion = \"1.0.0\"\nenvironment = \"roblox\"\n\
        index = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n\
        [[package]]\nname = \"chief/traits\"\nversion = \"0.2.0\"\nenvironment = \"roblox\"\n\
        index = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n\
        [[package]]\nname = \"chief/util\"\nversion = \"0.1.0\"\nenvironment = \"roblox\"\n\
        index = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n\
        [[package]]\nname = \"acme/signal\"\nversion = \"1.0.0\"\nenvironment = \"roblox\"\n\
        context = \"server\"\nlink = \"signal\"\nindex = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n\
        [[package]]\nname = \"chief/util\"\nversion = \"0.1.0\"\nenvironment = \"roblox\"\n\
        context = \"server\"\nindex = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n\
        [[package]]\nname = \"acme/tools\"\nversion = \"0.1.0\"\nenvironment = \"roblox\"\n\
        link = \"tools\"\nindex = \"workspace\"\ntype = \"workspace\"\npath = \"members/tools\"\n\
        [[package]]\nname = \"alloy/lest\"\nversion = \"0.2.0\"\nenvironment = \"roblox\"\n\
        link = \"lest\"\nindex = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n";

    /// the project of MANIFEST and LOCK, with each store folder install writes.
    fn project(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("embr-test-tree-{name}"));
        let _ = fs::remove_dir_all(&base);
        let roblox = base.join("packages/roblox/.ember");
        let dependency = |alias: &str, name: &str| {
            format!("[dependencies.{alias}]\nname = \"{name}\"\nversion = \"^\"\n")
        };
        write(
            &roblox.join("chief_core"),
            "ember.toml",
            &format!(
                "{}{}",
                dependency("traits", "chief/traits"),
                dependency("helper", "chief/helper")
            ),
        );
        write(
            &roblox.join("chief_helper"),
            "ember.toml",
            &dependency("traits", "chief/traits"),
        );
        // a wally manifest reads the same way
        write(
            &roblox.join("chief_traits"),
            "wally.toml",
            "[dependencies]\nUtil = \"chief/util@^0.1\"\n",
        );
        write(&roblox.join("chief_util"), "init.luau", "");
        write(&roblox.join("alloy_lest"), "init.luau", "");
        let server = base.join("packages/server/.ember");
        write(
            &server.join("acme_signal"),
            "ember.toml",
            &dependency("util", "chief/util"),
        );
        write(&server.join("chief_util"), "init.luau", "");
        write(
            &base.join("members/tools"),
            "ember.toml",
            "[package]\nname = \"acme/tools\"\nversion = \"0.1.0\"\n\
             [target]\nenvironment = \"roblox\"\n\
             [dependencies]\ncore = { name = \"chief/core\", version = \"^\" }\n",
        );
        base
    }

    fn read<'lock>(base: &Path, lock: &'lock Lockfile) -> Result<Graph<'lock>, Error> {
        let manifest: Manifest = toml::from_str(MANIFEST).unwrap();
        Graph::read(&manifest, lock, base)
    }

    #[test]
    fn each_package_knows_the_manifest_entries_that_bring_it_in() {
        let base = project("brought");
        let lock: Lockfile = toml::from_str(LOCK).unwrap();
        let manifest: Manifest = toml::from_str(MANIFEST).unwrap();
        let by = brought_in_by(&manifest, &lock, &base).unwrap();
        let names: Vec<(&str, Vec<&str>)> = lock
            .packages
            .iter()
            .zip(&by)
            .map(|(package, by)| {
                (
                    package.name.as_str(),
                    by.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("chief/core", vec!["Core", "tools"]),
                ("chief/helper", vec!["Core", "tools"]),
                ("chief/traits", vec!["Core", "tools"]),
                ("chief/util", vec!["Core", "tools"]),
                ("acme/signal", vec!["signal"]),
                ("chief/util", vec!["signal"]),
                ("acme/tools", vec!["tools"]),
                ("alloy/lest", vec!["lest"]),
            ]
        );
        // before an install there is no graph to read
        assert!(brought_in_by(&manifest, &lock, &base.join("nothing")).is_none());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_tree_groups_by_environment_and_marks_repeats() {
        let base = project("shape");
        let lock: Lockfile = toml::from_str(LOCK).unwrap();
        let graph = read(&base, &lock).unwrap();

        assert_eq!(
            plain(&graph.tree(None)),
            "roblox\n\
             ├── chief/core@0.3.0 as 'Core'\n\
             │   ├── chief/helper@1.0.0\n\
             │   │   └── chief/traits@0.2.0\n\
             │   │       └── chief/util@0.1.0 as 'Util'\n\
             │   └── chief/traits@0.2.0 (*)\n\
             ├── acme/tools@0.1.0\n\
             │   └── chief/core@0.3.0 (*)\n\
             └── alloy/lest@0.2.0 (dev)\n\
             roblox_server\n\
             └── acme/signal@1.0.0\n    \
                 └── chief/util@0.1.0\n"
        );
        // depth 0 is the direct dependencies alone
        let top = plain(&graph.tree(Some(0)));
        assert_eq!(top.lines().count(), 6, "{top}");
        assert!(!top.contains("helper"), "{top}");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn why_lists_every_path_from_the_manifest() {
        let base = project("why");
        let lock: Lockfile = toml::from_str(LOCK).unwrap();
        let graph = read(&base, &lock).unwrap();

        let paths = graph.paths_to("chief/traits");
        assert_eq!(
            plain(&graph.why(&paths)),
            "chief/traits@0.2.0 in roblox\n  \
             ember.toml > chief/core@0.3.0 as 'Core' > chief/helper@1.0.0 > chief/traits@0.2.0\n  \
             ember.toml > chief/core@0.3.0 as 'Core' > chief/traits@0.2.0\n  \
             ember.toml > acme/tools@0.1.0 > chief/core@0.3.0 > chief/helper@1.0.0 > chief/traits@0.2.0\n  \
             ember.toml > acme/tools@0.1.0 > chief/core@0.3.0 > chief/traits@0.2.0\n"
        );
        // the alias of the last hop names a package too, in each tree it is in
        let util = plain(&graph.why(&graph.paths_to("Util")));
        assert!(util.starts_with("chief/util@0.1.0 in roblox\n"), "{util}");
        assert!(!util.contains("roblox_server"), "{util}");
        assert_eq!(graph.paths_to("util").len(), 1);
        assert!(graph.paths_to("acme/missing").is_empty());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_store_or_entry_says_to_install() {
        let base = project("stale");
        let lock: Lockfile = toml::from_str(LOCK).unwrap();
        fs::remove_dir_all(base.join("packages/roblox/.ember/chief_helper")).unwrap();
        let error = read(&base, &lock).err().expect("an error");
        assert!(matches!(error, Error::NotInstalledYet(_)), "{error}");
        assert!(
            error.to_string().contains("chief/helper is not in"),
            "{error}"
        );
        assert!(
            error.to_string().ends_with("run `embr install` first"),
            "{error}"
        );

        // a dependency added to ember.toml after the install
        let lock: Lockfile = toml::from_str(&LOCK.replace("link = \"lest\"\n", "")).unwrap();
        let error = read(&base, &lock).err().expect("an error");
        assert_eq!(
            error.to_string(),
            "ember.lock has no alloy/lest for 'lest'; run `embr install` first"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
