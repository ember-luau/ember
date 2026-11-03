/*! `embr tree` and `embr why`. ember.lock records no edges, so both read
the graph that install left on disk. The manifest's dependencies are the
roots, and each installed package's own manifest gives its children. */

use crate::commands::install::stored_at;
use crate::error::Error;
use crate::project::lockfile::{LockedPackage, Lockfile};
use crate::project::manifest::{Environment, Manifest, manifest_in};
use crate::project::package::{self, DependencyKind};
use crate::ui;
use clap::Args;
use crossterm::style::Stylize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Args, Debug)]
pub struct TreeArgs {
    /// Levels to show under the direct dependencies, where 0 shows the direct dependencies only
    #[arg(long)]
    pub depth: Option<usize>,
    /// Print the tree as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct WhyArgs {
    /// Package to explain, as scope/name or as the alias that a dependent gives it
    pub package: String,
    /// Print the paths as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

pub fn tree(args: TreeArgs) -> Result<(), Error> {
    let (manifest, lock) = load()?;
    let graph = Graph::read(&manifest, &lock, Path::new(""))?;
    if args.json {
        return ui::print_json(&graph.tree_json(args.depth));
    }
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
    if args.json {
        return ui::print_json(&graph.why_json(args.package.trim(), &paths));
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
            // a peer is the project's own copy, so no package brings it in
            stack.extend(graph.children[node].iter().filter_map(installs));
        }
    }
    Some(by)
}

/// one manifest entry, and the installed package it became.
struct Root {
    alias: String,
    node: usize,
    dev: bool,
    optional: bool,
}

/// one hop of a path: the alias the parent gives the package, and the package.
type Hop<'graph> = (&'graph str, usize);

/// one edge of a package: the alias it gives the child, the child, and how it declares it.
type Edge = (String, usize, DependencyKind);

/// the child of `edge` when the edge installs it, which a peer edge never does.
fn installs((_, child, kind): &Edge) -> Option<usize> {
    (!matches!(kind, DependencyKind::Peer(_))).then_some(*child)
}

/** the installed graph. a node is an index into `packages`, and
`children[node]` holds the edges of that package. */
struct Graph<'lock> {
    packages: &'lock [LockedPackage],
    roots: Vec<Root>,
    children: Vec<Vec<Edge>>,
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
                    .or_else(|| packages.iter().position(|package| direct(&package)));
                let node = match node {
                    Some(node) => node,
                    // install skipped it, which is what optional allows
                    None if dependency.optional() => continue,
                    None => {
                        return Err(Error::NotInstalledYet(format!(
                            "ember.lock has no {name} for '{alias}'"
                        )));
                    }
                };
                roots.push(Root {
                    alias: alias.clone(),
                    node,
                    dev,
                    optional: dependency.optional(),
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
                    .map(|(alias, dependency)| {
                        let kind = match dependency.optional() {
                            true => DependencyKind::Optional,
                            false => DependencyKind::Required,
                        };
                        (alias.clone(), dependency.package().to_lowercase(), kind)
                    })
                    .collect()
            } else {
                package::declared_dependencies(&storage)
            };
            let edges = declared
                .into_iter()
                .filter_map(|(alias, declared, kind)| {
                    // an [overrides] redirect on this edge installed the replacement
                    let name = package.redirects.get(&alias).unwrap_or(&declared);
                    let child = packages.iter().position(|child| {
                        child.name.eq_ignore_ascii_case(name)
                            && child.context() == package.context()
                    })?;
                    Some((alias, child, kind))
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
                if root.optional {
                    label.push_str(&" (optional)".dark_grey().to_string());
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
        for (index, (alias, child, kind)) in children.iter().enumerate() {
            let mut label = self.label(*child, alias);
            let at = Some((prefix.as_str(), index + 1 == children.len()));
            let mut depth = depth.map(|depth| depth - 1);
            match kind {
                DependencyKind::Required => {}
                DependencyKind::Optional => {
                    label.push_str(&" (optional)".dark_grey().to_string());
                }
                /* a peer is the project's own copy. its levels show where
                the project lists it, so the line here stops */
                DependencyKind::Peer(_) => {
                    label.push_str(&" (peer)".dark_grey().to_string());
                    depth = Some(0);
                }
            }
            self.branch(out, at, *child, label, depth, shown);
        }
    }

    /** the `--json` tree: the roots in the order of [`Graph::tree`], each
    with its dependencies under it. a package whose dependencies were shown
    already gets `"repeated": true` and no dependencies, as `(*)` does. */
    fn tree_json(&self, depth: Option<usize>) -> Value {
        let mut shown = vec![false; self.packages.len()];
        let mut roots = Vec::new();
        for context in self.contexts() {
            for root in &self.roots {
                if self.packages[root.node].context() != context {
                    continue;
                }
                let kind = match root.optional {
                    true => DependencyKind::Optional,
                    false => DependencyKind::Required,
                };
                let mut node = self.node_json(root.node, &root.alias, &kind, depth, &mut shown);
                node["dev"] = json!(root.dev);
                roots.push(node);
            }
        }
        json!({ "version": ui::JSON_VERSION, "dependencies": roots })
    }

    /** one package of the `--json` tree, and the levels under it. `kind`
    gives the `optional` and `peer` fields, as the text marks them. */
    fn node_json(
        &self,
        node: usize,
        alias: &str,
        kind: &DependencyKind,
        depth: Option<usize>,
        shown: &mut [bool],
    ) -> Value {
        let package = &self.packages[node];
        let children = &self.children[node];
        let repeated = shown[node] && !children.is_empty();
        let mut dependencies = Vec::new();
        if !repeated && depth != Some(0) {
            shown[node] = true;
            for (alias, child, kind) in children {
                // a peer stops here, as in the text tree
                let depth = match kind {
                    DependencyKind::Peer(_) => Some(0),
                    _ => depth.map(|depth| depth - 1),
                };
                dependencies.push(self.node_json(*child, alias, kind, depth, shown));
            }
        }
        json!({
            "alias": alias,
            "name": package.name,
            "version": package.version,
            "environment": package.context().as_str(),
            "optional": matches!(kind, DependencyKind::Optional),
            "peer": matches!(kind, DependencyKind::Peer(_)),
            "repeated": repeated,
            "dependencies": dependencies,
        })
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
        // a peer edge installs nothing, so it explains no package
        for (alias, child, _) in self.children[node]
            .iter()
            .filter(|edge| installs(edge).is_some())
        {
            // a cycle would walk for ever
            if path.iter().any(|&(_, node)| node == *child) {
                continue;
            }
            path.push((alias, *child));
            self.walk(path, wanted, found);
            path.pop();
        }
    }

    /** the `--json` paths: each package that matched, in the order of
    [`Graph::why`], with each path to it. a path starts at the manifest
    entry and ends at the package. */
    fn why_json(&self, wanted: &str, paths: &[(&Root, Vec<Hop<'_>>)]) -> Value {
        let mut targets: Vec<usize> = Vec::new();
        for (_, path) in paths {
            let (_, target) = path[path.len() - 1];
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        let matches: Vec<Value> = targets
            .into_iter()
            .map(|target| {
                let package = &self.packages[target];
                let paths: Vec<Value> = paths
                    .iter()
                    .filter(|(_, path)| path[path.len() - 1].1 == target)
                    .map(|(root, path)| {
                        let hops: Vec<Value> = path
                            .iter()
                            .map(|&(alias, node)| {
                                let package = &self.packages[node];
                                json!({
                                    "alias": alias,
                                    "name": package.name,
                                    "version": package.version,
                                })
                            })
                            .collect();
                        json!({ "dev": root.dev, "path": hops })
                    })
                    .collect();
                json!({
                    "name": package.name,
                    "version": package.version,
                    "environment": package.context().as_str(),
                    "paths": paths,
                })
            })
            .collect();
        json!({ "version": ui::JSON_VERSION, "package": wanted, "matches": matches })
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

    /** an optional dependency that install skipped is no root, and one it
    installed says (optional). a peer edge points at the project's copy:
    the tree marks it and stops there, and it explains no package. */
    #[test]
    fn optional_and_peer_edges_are_marked() {
        let base = std::env::temp_dir().join("embr-test-tree-optional-peer");
        let _ = fs::remove_dir_all(&base);
        let store = base.join("packages/roblox/.ember");
        write(
            &store.join("acme_ui"),
            "ember.toml",
            "[dependencies]\nextra = { name = \"acme/trace\", version = \"^1\", optional = true }\n\
             [peer-dependencies]\nReact = { name = \"jsdotlua/react\", version = \"^17.0.0\" }\n",
        );
        write(
            &store.join("jsdotlua_react"),
            "ember.toml",
            "[dependencies]\nshared = { name = \"jsdotlua/shared\", version = \"^17\" }\n",
        );
        write(&store.join("jsdotlua_shared"), "init.luau", "");
        write(&store.join("acme_trace"), "init.luau", "");
        let manifest: Manifest = toml::from_str(
            "[target]\nenvironment = \"roblox\"\n[dependencies]\n\
             ui = { name = \"acme/ui\", version = \"^1\" }\n\
             React = { name = \"jsdotlua/react\", version = \"^17\" }\n\
             signal = { name = \"acme/signal\", version = \"^1\", optional = true }\n\
             trace = { name = \"acme/trace\", version = \"^1\", optional = true }\n",
        )
        .unwrap();
        let package = |name: &str, version: &str, link: Option<&str>| {
            let link = link.map_or(String::new(), |link| format!("link = \"{link}\"\n"));
            format!(
                "[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nenvironment = \"roblox\"\n\
                 {link}index = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n"
            )
        };
        let lock: Lockfile = toml::from_str(&format!(
            "version = 2\n{}{}{}{}optional = true\n",
            package("acme/ui", "1.0.0", Some("ui")),
            package("jsdotlua/react", "17.0.2", Some("React")),
            package("jsdotlua/shared", "17.0.2", None),
            package("acme/trace", "1.0.0", Some("trace")),
        ))
        .unwrap();
        let graph = Graph::read(&manifest, &lock, &base).unwrap();

        assert_eq!(
            plain(&graph.tree(None)),
            "jsdotlua/react@17.0.2 as 'React'\n\
             └── jsdotlua/shared@17.0.2\n\
             acme/trace@1.0.0 (optional)\n\
             acme/ui@1.0.0\n\
             ├── acme/trace@1.0.0 as 'extra' (optional)\n\
             └── jsdotlua/react@17.0.2 as 'React' (peer) (*)\n"
        );
        assert_eq!(
            plain(&graph.why(&graph.paths_to("jsdotlua/react"))),
            "jsdotlua/react@17.0.2\n  ember.toml > jsdotlua/react@17.0.2 as 'React'\n"
        );
        assert_eq!(
            plain(&graph.why(&graph.paths_to("acme/trace"))),
            "acme/trace@1.0.0\n  \
             ember.toml > acme/trace@1.0.0\n  \
             ember.toml > acme/ui@1.0.0 > acme/trace@1.0.0 as 'extra'\n"
        );
        assert!(graph.paths_to("acme/signal").is_empty());
        // the peer's own tree brings it in, not the package that needs it
        let by = brought_in_by(&manifest, &lock, &base).unwrap();
        assert_eq!(by[1], ["React"]);
        assert_eq!(by[2], ["React"]);
        // the json tree marks the same edges, and the peer stops there too
        let json = graph.tree_json(None);
        let marks = |node: &Value| json!([node["optional"], node["peer"]]);
        assert_eq!(marks(&json["dependencies"][1]), json!([true, false]));
        let ui = &json["dependencies"][2]["dependencies"];
        assert_eq!(marks(&ui[0]), json!([true, false]));
        assert_eq!(marks(&ui[1]), json!([false, true]));
        assert_eq!(ui[1]["repeated"], json!(true));
        assert_eq!(ui[1]["dependencies"], json!([]));
        let _ = fs::remove_dir_all(&base);
    }

    /// the `--json` shapes of `embr tree` and `embr why`.
    #[test]
    fn the_json_tree_and_paths_follow_the_text() {
        let base = project("json");
        let lock: Lockfile = toml::from_str(LOCK).unwrap();
        let graph = read(&base, &lock).unwrap();
        let node = |alias: &str, name: &str, version: &str, environment: &str, deps: Value| {
            json!({
                "alias": alias,
                "name": name,
                "version": version,
                "environment": environment,
                "optional": false,
                "peer": false,
                "repeated": false,
                "dependencies": deps,
            })
        };
        let root = |mut node: Value, dev: bool| {
            node["dev"] = json!(dev);
            node
        };
        let repeated = |alias: &str, name: &str, version: &str| {
            let mut node = node(alias, name, version, "roblox", json!([]));
            node["repeated"] = json!(true);
            node
        };
        let util = node("Util", "chief/util", "0.1.0", "roblox", json!([]));
        let traits = node("traits", "chief/traits", "0.2.0", "roblox", json!([util]));
        let helper = node("helper", "chief/helper", "1.0.0", "roblox", json!([traits]));
        let core = node(
            "Core",
            "chief/core",
            "0.3.0",
            "roblox",
            json!([helper, repeated("traits", "chief/traits", "0.2.0")]),
        );
        let tools = node(
            "tools",
            "acme/tools",
            "0.1.0",
            "roblox",
            json!([repeated("core", "chief/core", "0.3.0")]),
        );
        let lest = node("lest", "alloy/lest", "0.2.0", "roblox", json!([]));
        let server_util = node("util", "chief/util", "0.1.0", "roblox_server", json!([]));
        let signal = node(
            "signal",
            "acme/signal",
            "1.0.0",
            "roblox_server",
            json!([server_util]),
        );
        assert_eq!(
            graph.tree_json(None),
            json!({
                "version": 1,
                "dependencies": [
                    root(core, false),
                    root(tools, false),
                    root(lest, true),
                    root(signal, false),
                ],
            })
        );
        // depth 0 keeps the roots and drops what is under them
        let top = graph.tree_json(Some(0));
        assert_eq!(top["dependencies"].as_array().unwrap().len(), 4);
        assert_eq!(top["dependencies"][0]["dependencies"], json!([]));

        let hop = |alias: &str, name: &str, version: &str| json!({ "alias": alias, "name": name, "version": version });
        let paths = graph.paths_to("chief/helper");
        assert_eq!(
            graph.why_json("chief/helper", &paths),
            json!({
                "version": 1,
                "package": "chief/helper",
                "matches": [{
                    "name": "chief/helper",
                    "version": "1.0.0",
                    "environment": "roblox",
                    "paths": [
                        {
                            "dev": false,
                            "path": [
                                hop("Core", "chief/core", "0.3.0"),
                                hop("helper", "chief/helper", "1.0.0"),
                            ],
                        },
                        {
                            "dev": false,
                            "path": [
                                hop("tools", "acme/tools", "0.1.0"),
                                hop("core", "chief/core", "0.3.0"),
                                hop("helper", "chief/helper", "1.0.0"),
                            ],
                        },
                    ],
                }],
            })
        );
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
