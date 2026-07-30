use crate::error::Error;
use crate::project::manifest::{
    Dependency, Environment, Manifest, Override, override_paths, parse_version_req,
};
use crate::project::workspace::{self, Workspace};
use crate::registry::index::{DownloadSource, Index, Refresh};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;

/// A package ready to download, the flattened result of resolution.
#[derive(Debug)]
pub struct ResolvedInstall {
    pub name: String,
    pub version: semver::Version,
    /// None means the environment must be detected from the extracted files.
    pub environment: Option<Environment>,
    /** The environment root this package installs under, the environment of
    the direct dependency whose subtree discovered it. Every root is
    self-contained, a server package's shared dependencies live under
    packages/server, so one package can resolve once per context. */
    pub context: Environment,
    pub source: DownloadSource,
    pub index_url: String,
    /** Name of the generated top-level link file, the [dependencies] alias
    for direct deps. None for transitives, only declared dependencies are
    part of the project's require surface. */
    pub link: Option<String>,
    /** Edges of THIS package that [overrides] rewrote, its declared alias ->
    the replacement's package name. The nested-link pass reads a package's
    declared dependencies back off disk by name, so a name-changing override
    must travel to the linker and the lockfile or the dependent would link
    the original, or nothing. */
    pub redirects: BTreeMap<String, String>,
}

/// Where a queued dependency comes from.
#[derive(Clone)]
enum Request {
    Registry {
        req_text: String,
        index_url: String,
    },
    /** A member of this project's workspace. Like pesde, any `version` on
    the specifier is ignored locally, you get the member's current version.
    The req only matters when publishing. */
    Workspace,
}

/// One dependency waiting to resolve.
struct QueueEntry {
    name: String,
    request: Request,
    /// manifest alias for direct deps, their top-level link. None for transitives.
    link: Option<String>,
    /// alias path from the root, for [overrides] matching and diagnostics.
    alias_path: Vec<String>,
    /** the inherited install root. None only for seeds, whose own resolve
    decides it. by the time children are queued it's always known. */
    context: Option<Environment>,
}

/** Resolves the manifest's dependency graph breadth-first. Transitive deps
flatten into one install set deduped by package name, cross-manager ones
too, like a pesde package pulling a wally one. A requirement that rejects
the already-chosen version is a hard error. Workspace deps resolve to
sibling projects on disk and bring their own deps into the same set. */
pub fn resolve(
    manifest: &Manifest,
    project_dir: &Path,
    refresh: Refresh,
    warnings: &mut Vec<String>,
) -> Result<Vec<ResolvedInstall>, Error> {
    let mut ttl_skipped = false;
    match resolve_once(manifest, project_dir, refresh, &mut ttl_skipped, warnings) {
        /* an index whose pull was skipped by the TTL can be stale, and most
        resolver errors don't say which index they came from, so any
        failure after a skip earns one full forced refresh and a re-run.
        the second outcome, good or bad, is the one that stands. */
        Err(_) if refresh == Refresh::Ttl && ttl_skipped => {
            let mut ignored = false;
            warnings.clear();
            resolve_once(
                manifest,
                project_dir,
                Refresh::Force,
                &mut ignored,
                warnings,
            )
        }
        result => result,
    }
}

fn resolve_once(
    manifest: &Manifest,
    project_dir: &Path,
    refresh: Refresh,
    ttl_skipped: &mut bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<ResolvedInstall>, Error> {
    let prefer_environment = manifest.target.as_ref().map(|target| target.environment);
    let mut indices: HashMap<String, Index> = HashMap::new();
    /* Only walk up looking for a claiming workspace root once a workspace
    dep actually appears. */
    let mut workspace_memo: Option<Option<Workspace>> = None;

    /* [overrides] expanded and validated up front, one (name, request) per
    alias path. a dangling alias, unknown index key, or two keys covering
    the same path all fail before any network happens, whether or not the
    path ends up matching an edge. Each edge the walk discovers carries its
    alias path from the root ("foo" -> "foo.bar" -> ...), an exact match
    rewrites that edge before it's queued. */
    let mut overrides: HashMap<Vec<String>, (String, Request)> = HashMap::new();
    for (key, value) in &manifest.overrides {
        for path in override_paths(key)? {
            let edge = overridden_edge(value, &path, manifest)?;
            if overrides.insert(path.clone(), edge).is_some() {
                return Err(Error::OverrideDuplicatePath(path.join(".")));
            }
        }
    }
    let mut overrides_matched: HashSet<Vec<String>> = HashSet::new();
    /* enough breadcrumbs to say WHY an override never fired. every queued
    edge path and its package, the one path each package was walked under
    per context since children are enumerated once per context, and each
    root alias's context, so diagnostics look in the right tree */
    let mut discovered: HashMap<Vec<String>, String> = HashMap::new();
    let mut walked_at: HashMap<(String, Environment), Vec<String>> = HashMap::new();
    let mut seed_contexts: HashMap<String, Environment> = HashMap::new();

    let mut queue: VecDeque<QueueEntry> = VecDeque::new();

    /* Seed all direct deps before any transitive one is discovered. first
    entry per (name, context) wins, so a package that also shows up
    transitively still links under its manifest alias. */
    for (alias, dependency) in &manifest.dependencies {
        let name = dependency_name(dependency).to_lowercase();
        discovered.insert(vec![alias.clone()], name.clone());
        queue.push_back(QueueEntry {
            name,
            request: request_for(dependency, manifest)?,
            link: Some(alias.clone()),
            alias_path: vec![alias.clone()],
            context: None,
        });
    }

    /* (name, context) -> (what we resolved, the req that won). BTreeMap
    keeps the install set, and the lockfile written from it, in name order
    with context breaking ties. one package may appear once per context,
    every environment root is self-contained, wally-style. */
    let mut resolved: BTreeMap<(String, Environment), (ResolvedInstall, String)> = BTreeMap::new();

    while let Some(QueueEntry {
        name,
        request,
        link,
        alias_path,
        context,
    }) = queue.pop_front()
    {
        let req_text = match &request {
            Request::Registry { req_text, .. } => req_text.clone(),
            Request::Workspace => "*".to_string(),
        };
        let req = parse_version_req(&req_text)?;

        /* transitives inherit their context, so their dedup gate runs
        before any network. a seed's context is decided by its own resolve,
        so its gate has to wait until just after, below */
        if let Some(context) = context
            && let Some((existing, first_req)) = resolved.get(&(name.clone(), context))
        {
            if req.matches(&existing.version) {
                continue;
            }
            return Err(Error::DependencyConflict {
                name,
                context,
                first: first_req.clone(),
                second: req_text,
            });
        }

        /* a workspace member shadows the registry copy of its name in
        EVERY context. the member is the copy being developed, and a
        server tree quietly pulling the published version while shared
        uses the local one would split the two. seeds all resolve before
        any transitive pops, so a seeded member is always visible here. */
        let request = match request {
            Request::Registry { .. }
                if resolved.iter().any(|((resolved_name, _), (install, _))| {
                    *resolved_name == name
                        && matches!(install.source, DownloadSource::Workspace { .. })
                }) =>
            {
                Request::Workspace
            }
            request => request,
        };

        /* children are collected, not queued. whether this package's edges
        walk at all is decided by the gates, and the side effects, matched
        overrides, discovered paths, redirects, must not land for a walk
        that never happens */
        let mut redirects: BTreeMap<String, String> = BTreeMap::new();
        let mut pending_matched: Vec<Vec<String>> = Vec::new();
        let mut pending_discovered: Vec<(Vec<String>, String)> = Vec::new();
        let mut children: Vec<(String, Request, Vec<String>)> = Vec::new();

        let (version, environment, source, index_url) = match request {
            Request::Registry { index_url, .. } => {
                let index = open_index(&mut indices, &index_url, refresh, ttl_skipped)?;
                /* a multi-target entry should pick the target of the tree
                it installs into. seeds fall back to the project's own */
                let package = index.resolve(&name, &req, context.or(prefer_environment))?;

                for dependency in &package.dependencies {
                    let mut child_path = alias_path.clone();
                    child_path.push(dependency.alias.clone());

                    let (child_name, child_request) = match overrides.get(&child_path) {
                        Some((child_name, child_request)) => {
                            pending_matched.push(child_path.clone());
                            redirects.insert(dependency.alias.clone(), child_name.clone());
                            (child_name.clone(), child_request.clone())
                        }
                        None => (
                            dependency.name.clone(),
                            Request::Registry {
                                req_text: dependency.version_req.clone(),
                                index_url: dependency
                                    .index_url
                                    .clone()
                                    .unwrap_or_else(|| index_url.clone()),
                            },
                        ),
                    };
                    pending_discovered.push((child_path.clone(), child_name.clone()));
                    children.push((child_name, child_request, child_path));
                }

                (
                    package.version,
                    package.environment,
                    package.source,
                    index_url,
                )
            }
            Request::Workspace => {
                let workspace = workspace_context(&mut workspace_memo, manifest, project_dir)?
                    .ok_or_else(|| Error::NotInWorkspace(name.clone()))?;
                let member = workspace
                    .member(&name)
                    .ok_or_else(|| Error::NoWorkspaceMember(name.clone()))?;
                let version = semver::Version::parse(&member.manifest.package.version)?;
                let environment = member
                    .manifest
                    .target
                    .as_ref()
                    .map(|target| target.environment)
                    .ok_or_else(|| Error::UnknownPackageEnvironment(name.clone()))?;

                /* The member's own deps install for the consumer too. its
                registry deps resolve against the member's [indices], but
                the consumer's [overrides] still apply to the edges. */
                for (alias, dependency) in &member.manifest.dependencies {
                    let mut child_path = alias_path.clone();
                    child_path.push(alias.clone());

                    let (child_name, child_request) = match overrides.get(&child_path) {
                        Some((child_name, child_request)) => {
                            pending_matched.push(child_path.clone());
                            redirects.insert(alias.clone(), child_name.clone());
                            (child_name.clone(), child_request.clone())
                        }
                        None => (
                            dependency_name(dependency).to_lowercase(),
                            request_for(dependency, &member.manifest)?,
                        ),
                    };
                    pending_discovered.push((child_path.clone(), child_name.clone()));
                    children.push((child_name, child_request, child_path));
                }

                let path =
                    workspace::relative_path(&std::path::absolute(project_dir)?, &member.dir);
                (
                    version,
                    Some(environment),
                    DownloadSource::Workspace { path },
                    "workspace".to_string(),
                )
            }
        };

        /* a seed's context is its own environment, else the project's.
        the subtree's placement can't wait for archive extraction */
        let seeded_here = context.is_none();
        let context = match context {
            Some(context) => context,
            None => seed_context(environment, prefer_environment, &name)?,
        };
        if seeded_here {
            /* recorded even when the gate below dedups, so unmatched-
            override diagnostics can still find this root's tree */
            seed_contexts.insert(alias_path[0].clone(), context);
            if let Some((existing, first_req)) = resolved.get(&(name.clone(), context)) {
                if req.matches(&existing.version) {
                    continue; // this tree already walked the package, children drop
                }
                return Err(Error::DependencyConflict {
                    name,
                    context,
                    first: first_req.clone(),
                    second: req_text,
                });
            }
        }

        /* a registry edge shadowed onto a member still states a version
        requirement, honor it like the dedup gate would have. workspace
        specifiers' own req is "*", so only converted edges can trip this */
        if matches!(source, DownloadSource::Workspace { .. }) && !req.matches(&version) {
            return Err(Error::DependencyConflict {
                name,
                context,
                first: "*".to_string(),
                second: req_text,
            });
        }

        // the walk is real, commit its breadcrumbs and queue the edges
        walked_at
            .entry((name.clone(), context))
            .or_insert(alias_path.clone());
        overrides_matched.extend(pending_matched);
        discovered.extend(pending_discovered);
        for (child_name, child_request, child_path) in children {
            queue.push_back(QueueEntry {
                name: child_name,
                request: child_request,
                link: None,
                alias_path: child_path,
                context: Some(context),
            });
        }

        resolved.insert(
            (name.clone(), context),
            (
                ResolvedInstall {
                    name,
                    version,
                    environment,
                    context,
                    source,
                    index_url,
                    link,
                    redirects,
                },
                req_text,
            ),
        );
    }

    /* an override that never met its edge would otherwise be silently
    dead. two distinct reasons deserve distinct messages. a path that
    exists but wasn't walked, its package's edges were enumerated under an
    earlier discovery path since root aliases seed alphabetically, and a
    path that simply never appeared, a typo or a dependency that moved on. */
    let mut unmatched: Vec<&Vec<String>> = overrides
        .keys()
        .filter(|path| !overrides_matched.contains(*path))
        .collect();
    unmatched.sort();
    for path in unmatched {
        let parent = &path[..path.len() - 1];
        let elsewhere = discovered
            .get(parent)
            .and_then(|parent_name| {
                /* the tree this path belongs to is its root alias's. a walk
                of the same package in another context is a different tree */
                let context = seed_contexts.get(path.first()?)?;
                walked_at.get(&(parent_name.clone(), *context))
            })
            .filter(|walked| walked.as_slice() != parent);
        warnings.push(match elsewhere {
            Some(walked) => format!(
                "warning: override '{}' could not apply: that package's dependencies were walked via '{}' first; address the edge there",
                path.join("."),
                walked.join(".")
            ),
            None => format!(
                "warning: override '{}' matched no dependency; check the alias path",
                path.join(".")
            ),
        });
    }

    Ok(resolved.into_values().map(|(install, _)| install).collect())
}

/** what an overridden edge asks for instead, the root manifest's own
dependency when the override is an alias, or the inline specifier. Either
way index keys resolve against the ROOT manifest's [indices], the author
of the override is the one naming the index. */
fn overridden_edge(
    replacement: &Override,
    path: &[String],
    manifest: &Manifest,
) -> Result<(String, Request), Error> {
    let dependency = match replacement {
        Override::Alias(alias) => {
            manifest
                .dependencies
                .get(alias)
                .ok_or_else(|| Error::OverrideAliasMissing {
                    path: path.join("."),
                    alias: alias.clone(),
                })?
        }
        Override::Specifier(dependency) => dependency,
    };
    Ok((
        dependency_name(dependency).to_lowercase(),
        request_for(dependency, manifest)?,
    ))
}

/** a seed's context is its own resolved environment, else the project's
`[target]` one. placement is decided at resolve time since children queue
with it, so unlike a package's own environment it can never wait for the
archive to be extracted and inspected. */
fn seed_context(
    environment: Option<Environment>,
    prefer: Option<Environment>,
    name: &str,
) -> Result<Environment, Error> {
    environment
        .or(prefer)
        .ok_or_else(|| Error::UnknownPackageEnvironment(name.to_string()))
}

fn dependency_name(dependency: &Dependency) -> &str {
    match dependency {
        Dependency::Registry { name, .. } => name,
        Dependency::Workspace { workspace, .. } => workspace,
    }
}

/** Queue entry for a dependency of `owner`. Index keys resolve against the
owner's [indices], so a member's deps use the member's. */
fn request_for(dependency: &Dependency, owner: &Manifest) -> Result<Request, Error> {
    Ok(match dependency {
        Dependency::Registry { version, index, .. } => Request::Registry {
            req_text: version.clone(),
            index_url: owner.index_url(index.as_deref())?.to_string(),
        },
        Dependency::Workspace { .. } => Request::Workspace,
    })
}

/** The workspace this project resolves members from, itself when it declares
members, otherwise the nearest ancestor that claims it. Memoized so glob
walks and manifest reads don't repeat per dependency. */
fn workspace_context<'memo>(
    memo: &'memo mut Option<Option<Workspace>>,
    manifest: &Manifest,
    project_dir: &Path,
) -> Result<Option<&'memo Workspace>, Error> {
    if memo.is_none() {
        let workspace = if manifest.workspace_members().is_empty() {
            workspace::containing(project_dir)?
        } else {
            Some(Workspace::open(project_dir)?)
        };
        *memo = Some(workspace);
    }
    Ok(memo.as_ref().expect("just memoized").as_ref())
}

/// Opens each index at most once per run, so each gets refreshed once.
fn open_index<'a>(
    indices: &'a mut HashMap<String, Index>,
    url: &str,
    refresh: Refresh,
    ttl_skipped: &mut bool,
) -> Result<&'a Index, Error> {
    if !indices.contains_key(url) {
        let index = Index::open(url, refresh)?;
        *ttl_skipped |= index.ttl_skipped();
        indices.insert(url.to_string(), index);
    }
    Ok(&indices[url])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, file: &str, contents: &str) {
        let path = dir.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn workspace_dependencies_resolve_to_local_members() {
        let base = std::env::temp_dir().join("lpm-test-resolver-workspace");
        let _ = fs::remove_dir_all(&base);

        write(
            &base,
            "lpm.toml",
            "[package]\nname = \"acme/root\"\nversion = \"0.0.0\"\nprivate = true\n\n\
             [target]\nenvironment = \"shared\"\nworkspace = [\"packages/*\"]\n",
        );
        write(
            &base.join("packages/core"),
            "lpm.toml",
            "[package]\nname = \"acme/core\"\nversion = \"1.2.0\"\n\n\
             [target]\nenvironment = \"shared\"\n",
        );
        write(
            &base.join("packages/extra"),
            "lpm.toml",
            "[package]\nname = \"acme/extra\"\nversion = \"0.3.0\"\n\n\
             [target]\nenvironment = \"shared\"\n\n\
             [dependencies]\ncore = { workspace = \"acme/core\", version = \"^\" }\n",
        );

        /* Resolving from a member, its workspace dep and that dep's own
        workspace dep all land in the install set, linked in place. */
        let member_dir = base.join("packages/extra");
        let manifest = Manifest::load_from(&member_dir.join("lpm.toml")).unwrap();
        let installs = resolve(&manifest, &member_dir, Refresh::Never, &mut Vec::new()).unwrap();

        assert_eq!(installs.len(), 1);
        let core = &installs[0];
        assert_eq!(core.name, "acme/core");
        assert_eq!(core.version, semver::Version::new(1, 2, 0));
        assert_eq!(core.environment, Some(Environment::Shared));
        assert_eq!(core.context, Environment::Shared);
        assert_eq!(core.link.as_deref(), Some("core"));
        assert!(matches!(
            &core.source,
            DownloadSource::Workspace { path } if path == "../core"
        ));

        /* Resolving from the root, members resolve through the root's own
        member list, no ancestor needed, and members' transitive
        workspace deps come along. */
        let root_manifest_text = "[package]\nname = \"acme/root\"\nversion = \"0.0.0\"\nprivate = true\n\n\
             [target]\nenvironment = \"shared\"\nworkspace = [\"packages/*\"]\n\n\
             [dependencies]\nextra = { workspace = \"acme/extra\" }\n";
        write(&base, "lpm.toml", root_manifest_text);
        let manifest = Manifest::load_from(&base.join("lpm.toml")).unwrap();
        let installs = resolve(&manifest, &base, Refresh::Never, &mut Vec::new()).unwrap();
        let names: Vec<_> = installs
            .iter()
            .map(|install| install.name.as_str())
            .collect();
        assert_eq!(names, ["acme/core", "acme/extra"]);
        assert!(matches!(
            &installs[1].source,
            DownloadSource::Workspace { path } if path == "packages/extra"
        ));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn seed_contexts_fall_back_to_the_target_environment() {
        // own environment first, [target] second, then a hard error
        assert_eq!(
            seed_context(Some(Environment::Server), Some(Environment::Shared), "a/b").unwrap(),
            Environment::Server
        );
        assert_eq!(
            seed_context(None, Some(Environment::Lune), "a/b").unwrap(),
            Environment::Lune
        );
        assert!(matches!(
            seed_context(None, None, "a/b"),
            Err(Error::UnknownPackageEnvironment(name)) if name == "a/b"
        ));
    }

    /** the self-contained-roots contract, against a real local git index.
    a server seed drags its whole subtree into the server context, shared
    deps included, the same package under two roots resolves once
    per root, and cross-context version divergence is legal where a
    same-context one conflicts. */
    #[test]
    fn contexts_follow_the_dependency_root() {
        /// removes the fixture dirs even when an assertion panics.
        struct Cleanup(Vec<std::path::PathBuf>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for dir in &self.0 {
                    let _ = fs::remove_dir_all(dir);
                }
            }
        }

        let origin = std::env::temp_dir().join("lpm-test-contexts-origin");
        let _ = fs::remove_dir_all(&origin);
        fs::create_dir_all(origin.join("acme")).unwrap();
        let url = origin.to_string_lossy().replace('\\', "/");
        let cache = crate::registry::index::cache_dir(&url).unwrap();
        let _ = fs::remove_dir_all(&cache);
        let _cleanup = Cleanup(vec![origin.clone(), cache.clone()]);

        let entry = |name: &str, version: &str, realm: &str, dependencies: &str| {
            format!(
                "{{\"package\":{{\"name\":\"{name}\",\"version\":\"{version}\",\"realm\":\"{realm}\",\"registry\":\"\"}},\"dependencies\":{{{dependencies}}}}}\n"
            )
        };
        fs::write(
            origin.join("config.json"),
            r#"{"api":"https://example.com"}"#,
        )
        .unwrap();
        fs::write(
            origin.join("acme/svc"),
            entry(
                "acme/svc",
                "1.0.0",
                "server",
                r#""lib":"acme/lib@^1.0.0","pin":"acme/pin@^1.0.0""#,
            ) + &entry("acme/svc", "2.0.0", "server", ""),
        )
        .unwrap();
        fs::write(
            origin.join("acme/lib"),
            entry("acme/lib", "1.0.0", "shared", ""),
        )
        .unwrap();
        fs::write(
            origin.join("acme/pin"),
            entry("acme/pin", "1.0.0", "shared", "") + &entry("acme/pin", "2.0.0", "shared", ""),
        )
        .unwrap();
        for args in [
            vec!["init"],
            vec!["add", "."],
            vec!["commit", "-m", "fixture"],
        ] {
            let mut full = vec![
                "-C",
                origin.to_str().unwrap(),
                "-c",
                "user.name=lpm-test",
                "-c",
                "user.email=lpm-test@localhost",
            ];
            full.extend(args);
            crate::sys::git::run(&full).unwrap();
        }

        let resolve_with = |dependencies: &str| {
            let manifest: Manifest = toml::from_str(&format!(
                "[package]\nname = \"acme/consumer\"\nversion = \"0.1.0\"\n\n\
                 [indices]\ndefault = \"{url}\"\n\n[dependencies]\n{dependencies}"
            ))
            .unwrap();
            resolve(&manifest, &origin, Refresh::Never, &mut Vec::new())
        };

        /* svc, a server package, pulls lib and pin into the server context.
        lib and pin are ALSO direct shared deps, so both exist twice, once
        per root, and pin's versions may even diverge across them */
        let installs = resolve_with(
            "svc = { name = \"acme/svc\", version = \"^1\" }\n\
             lib = { name = \"acme/lib\", version = \"^1\" }\n\
             pin = { name = \"acme/pin\", version = \"^2\" }\n",
        )
        .unwrap();
        let summary: Vec<String> = installs
            .iter()
            .map(|install| {
                format!(
                    "{}@{} {}/{}",
                    install.name,
                    install.version,
                    install.context,
                    install.link.as_deref().unwrap_or("-")
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                "acme/lib@1.0.0 shared/lib",
                "acme/lib@1.0.0 server/-",
                "acme/pin@2.0.0 shared/pin",
                "acme/pin@1.0.0 server/-",
                "acme/svc@1.0.0 server/svc",
            ]
        );
        // every install keeps its own environment for nested-folder naming
        let server_lib = installs
            .iter()
            .find(|install| install.name == "acme/lib" && install.context == Environment::Server)
            .unwrap();
        assert_eq!(server_lib.environment, Some(Environment::Shared));

        // within one context, disagreeing requirements still conflict
        let conflict = resolve_with(
            "svc = { name = \"acme/svc\", version = \"^1\" }\n\
             direct = { name = \"acme/svc\", version = \"^2\" }\n",
        )
        .unwrap_err();
        assert!(matches!(
            conflict,
            Error::DependencyConflict {
                context: Environment::Server,
                ..
            }
        ));

        let _ = fs::remove_dir_all(&cache);
    }

    /** a workspace member owns its name in every context. a server-tree
    registry edge naming it binds to the local member at the member's
    version, never to the published copy, unless its requirement can't
    accept the member, which is a conflict, not a silent split. */
    #[test]
    fn workspace_members_shadow_registry_copies_in_every_context() {
        /// removes the fixture dirs even when an assertion panics.
        struct Cleanup(Vec<std::path::PathBuf>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for dir in &self.0 {
                    let _ = fs::remove_dir_all(dir);
                }
            }
        }

        let origin = std::env::temp_dir().join("lpm-test-shadow-origin");
        let project = std::env::temp_dir().join("lpm-test-shadow-project");
        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&project);
        fs::create_dir_all(origin.join("acme")).unwrap();
        let url = origin.to_string_lossy().replace('\\', "/");
        let cache = crate::registry::index::cache_dir(&url).unwrap();
        let _ = fs::remove_dir_all(&cache);
        let _cleanup = Cleanup(vec![origin.clone(), project.clone(), cache.clone()]);

        let entry = |name: &str, version: &str, realm: &str, dependencies: &str| {
            format!(
                "{{\"package\":{{\"name\":\"{name}\",\"version\":\"{version}\",\"realm\":\"{realm}\",\"registry\":\"\"}},\"dependencies\":{{{dependencies}}}}}\n"
            )
        };
        fs::write(
            origin.join("config.json"),
            r#"{"api":"https://example.com"}"#,
        )
        .unwrap();
        // the published copy of the member's name, at an older version
        fs::write(
            origin.join("acme/core"),
            entry("acme/core", "1.0.0", "shared", ""),
        )
        .unwrap();
        fs::write(
            origin.join("acme/svc"),
            entry(
                "acme/svc",
                "1.0.0",
                "server",
                r#""core":"acme/core@^1.0.0""#,
            ),
        )
        .unwrap();
        fs::write(
            origin.join("acme/strict"),
            entry(
                "acme/strict",
                "1.0.0",
                "server",
                r#""core":"acme/core@^9.0.0""#,
            ),
        )
        .unwrap();
        for args in [
            vec!["init"],
            vec!["add", "."],
            vec!["commit", "-m", "fixture"],
        ] {
            let mut full = vec![
                "-C",
                origin.to_str().unwrap(),
                "-c",
                "user.name=lpm-test",
                "-c",
                "user.email=lpm-test@localhost",
            ];
            full.extend(args);
            crate::sys::git::run(&full).unwrap();
        }

        write(
            &project.join("packages/core"),
            "lpm.toml",
            "[package]\nname = \"acme/core\"\nversion = \"1.2.0\"\n\n\
             [target]\nenvironment = \"shared\"\n",
        );
        let resolve_with = |server_dep: &str| {
            write(
                &project,
                "lpm.toml",
                &format!(
                    "[package]\nname = \"acme/root\"\nversion = \"0.0.0\"\nprivate = true\n\n\
                     [target]\nenvironment = \"shared\"\nworkspace = [\"packages/*\"]\n\n\
                     [indices]\ndefault = \"{url}\"\n\n\
                     [dependencies]\ncore = {{ workspace = \"acme/core\" }}\n\
                     {server_dep} = {{ name = \"acme/{server_dep}\", version = \"^1\" }}\n"
                ),
            );
            let manifest = Manifest::load_from(&project.join("lpm.toml")).unwrap();
            resolve(&manifest, &project, Refresh::Never, &mut Vec::new())
        };

        /* svc's server-tree edge to acme/core binds to the member. same
        version as the shared copy, 1.2.0 not the registry's 1.0.0,
        workspace source, no top-level link */
        let installs = resolve_with("svc").unwrap();
        let cores: Vec<_> = installs
            .iter()
            .filter(|install| install.name == "acme/core")
            .collect();
        assert_eq!(cores.len(), 2);
        for core in &cores {
            assert_eq!(core.version, semver::Version::new(1, 2, 0));
            assert!(matches!(core.source, DownloadSource::Workspace { .. }));
        }
        assert_eq!(cores[0].context, Environment::Shared);
        assert_eq!(cores[0].link.as_deref(), Some("core"));
        assert_eq!(cores[1].context, Environment::Server);
        assert_eq!(cores[1].link, None);

        // an edge whose requirement the member can't satisfy is a conflict
        assert!(matches!(
            resolve_with("strict").unwrap_err(),
            Error::DependencyConflict {
                context: Environment::Server,
                ..
            }
        ));
    }

    /** [overrides] end to end, against a real local git index. a specifier
    override redirects an edge to another package, an alias override reuses
    the root's own dependency, a dangling alias errors, and a pathless key
    errors. Refresh::Never keeps every open on the local clone. */
    #[test]
    fn overrides_redirect_transitive_edges() {
        /// removes the fixture dirs even when an assertion panics.
        struct Cleanup(Vec<std::path::PathBuf>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for dir in &self.0 {
                    let _ = fs::remove_dir_all(dir);
                }
            }
        }

        let origin = std::env::temp_dir().join("lpm-test-overrides-origin");
        let _ = fs::remove_dir_all(&origin);
        fs::create_dir_all(origin.join("acme")).unwrap();
        let url = origin.to_string_lossy().replace('\\', "/");
        let cache = crate::registry::index::cache_dir(&url).unwrap();
        let _ = fs::remove_dir_all(&cache);
        let _cleanup = Cleanup(vec![origin.clone(), cache.clone()]);

        let entry = |name: &str, version: &str, dependencies: &str| {
            format!(
                "{{\"package\":{{\"name\":\"{name}\",\"version\":\"{version}\",\"realm\":\"shared\",\"registry\":\"\"}},\"dependencies\":{{{dependencies}}}}}\n"
            )
        };
        fs::write(
            origin.join("config.json"),
            r#"{"api":"https://example.com"}"#,
        )
        .unwrap();
        fs::write(
            origin.join("acme/foo"),
            entry("acme/foo", "1.0.0", r#""bar":"acme/bar@^1.0.0""#),
        )
        .unwrap();
        fs::write(
            origin.join("acme/bar"),
            entry("acme/bar", "1.0.0", "") + &entry("acme/bar", "2.0.0", ""),
        )
        .unwrap();
        fs::write(origin.join("acme/qux"), entry("acme/qux", "1.0.0", "")).unwrap();
        crate::sys::git::run(&[
            "-C",
            origin.to_str().unwrap(),
            "-c",
            "user.name=lpm-test",
            "-c",
            "user.email=lpm-test@localhost",
            "init",
        ])
        .unwrap();
        crate::sys::git::run(&["-C", origin.to_str().unwrap(), "add", "."]).unwrap();
        crate::sys::git::run(&[
            "-C",
            origin.to_str().unwrap(),
            "-c",
            "user.name=lpm-test",
            "-c",
            "user.email=lpm-test@localhost",
            "commit",
            "-m",
            "fixture",
        ])
        .unwrap();

        let resolve_with = |extra: &str, warnings: &mut Vec<String>| {
            let manifest: Manifest = toml::from_str(&format!(
                "[package]\nname = \"acme/consumer\"\nversion = \"0.1.0\"\n\n\
                 [indices]\ndefault = \"{url}\"\n\n{extra}"
            ))
            .unwrap();
            resolve(&manifest, &origin, Refresh::Never, warnings)
        };
        let summary = |installs: &[ResolvedInstall]| -> Vec<String> {
            installs
                .iter()
                .map(|install| {
                    format!(
                        "{}@{} as {}",
                        install.name,
                        install.version,
                        // transitives carry no link, "-" marks them
                        install.link.as_deref().unwrap_or("-")
                    )
                })
                .collect()
        };

        // no overrides, foo brings its declared bar, linkless because transitive
        let plain = resolve_with(
            "[dependencies]\nfoo = { name = \"acme/foo\", version = \"^1\" }\n",
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            summary(&plain),
            ["acme/bar@1.0.0 as -", "acme/foo@1.0.0 as foo"]
        );

        /* a specifier override swaps the edge for a different package, and
        the dependent records the redirect so the linker can honor it */
        let swapped = resolve_with(
            "[dependencies]\nfoo = { name = \"acme/foo\", version = \"^1\" }\n\n\
             [overrides]\n\"foo.bar\" = { name = \"acme/qux\", version = \"^1\" }\n",
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            summary(&swapped),
            ["acme/foo@1.0.0 as foo", "acme/qux@1.0.0 as -"]
        );
        let foo = swapped
            .iter()
            .find(|install| install.name == "acme/foo")
            .unwrap();
        assert_eq!(
            foo.redirects.get("bar").map(String::as_str),
            Some("acme/qux")
        );

        /* an alias override defers to the root's own [dependencies] entry.
        one bar in the set, at the root's version, under the root's link */
        let aliased = resolve_with(
            "[dependencies]\nfoo = { name = \"acme/foo\", version = \"^1\" }\n\
             Bar = { name = \"acme/bar\", version = \"^2.0.0\" }\n\n\
             [overrides]\n\"foo.bar\" = \"Bar\"\n",
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            summary(&aliased),
            ["acme/bar@2.0.0 as Bar", "acme/foo@1.0.0 as foo"]
        );

        /* an override addressed through the SECOND parent of a shared
        package can't apply, edges are walked once under the first
        discovery path, and the warning says that, not "typo" */
        let mut warnings = Vec::new();
        let deep = resolve_with(
            "[dependencies]\naaa = { name = \"acme/foo\", version = \"^1\" }\n\
             zzz = { name = \"acme/foo\", version = \"^1\" }\n\n\
             [overrides]\n\"zzz.bar\" = { name = \"acme/qux\", version = \"^1\" }\n",
            &mut warnings,
        )
        .unwrap();
        assert_eq!(
            summary(&deep),
            ["acme/bar@1.0.0 as -", "acme/foo@1.0.0 as aaa"]
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("walked via 'aaa'"), "{warnings:?}");

        // a plain typo'd path gets the plain message
        let mut warnings = Vec::new();
        resolve_with(
            "[dependencies]\nfoo = { name = \"acme/foo\", version = \"^1\" }\n\n\
             [overrides]\n\"foo.typo\" = { name = \"acme/qux\", version = \"^1\" }\n",
            &mut warnings,
        )
        .unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("matched no dependency"),
            "{warnings:?}"
        );

        /* eager validation. a dangling alias, a pathless key, interior
        whitespace, a duplicate expanded path, and an unknown [indices]
        key all fail before any edge is walked */
        let overrides_error = |overrides: &str| {
            resolve_with(
                &format!(
                    "[dependencies]\nfoo = {{ name = \"acme/foo\", version = \"^1\" }}\n\n\
                     [overrides]\n{overrides}\n"
                ),
                &mut Vec::new(),
            )
            .unwrap_err()
        };
        assert!(matches!(
            overrides_error("\"foo.bar\" = \"nope\""),
            Error::OverrideAliasMissing { .. }
        ));
        assert!(matches!(
            overrides_error("\"foo\" = { name = \"acme/qux\", version = \"^1\" }"),
            Error::OverrideBadPath(_)
        ));
        assert!(matches!(
            overrides_error("\"foo .bar\" = { name = \"acme/qux\", version = \"^1\" }"),
            Error::OverrideBadPath(_)
        ));
        assert!(matches!(
            overrides_error(
                "\"foo.bar\" = { name = \"acme/qux\", version = \"^1\" }\n\
                 \"foo.bar, foo.other\" = { name = \"acme/bar\", version = \"^1\" }"
            ),
            Error::OverrideDuplicatePath(_)
        ));
        assert!(matches!(
            overrides_error(
                "\"foo.bar\" = { name = \"acme/qux\", version = \"^1\", index = \"nope\" }"
            ),
            Error::UnknownIndex(_)
        ));
    }

    #[test]
    fn workspace_dependency_outside_a_workspace_fails() {
        let base = std::env::temp_dir().join("lpm-test-resolver-no-workspace");
        let _ = fs::remove_dir_all(&base);
        write(
            &base,
            "lpm.toml",
            "[package]\nname = \"acme/lonely\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ncore = { workspace = \"acme/core\" }\n",
        );

        let manifest = Manifest::load_from(&base.join("lpm.toml")).unwrap();
        assert!(matches!(
            resolve(&manifest, &base, Refresh::Never, &mut Vec::new()),
            Err(Error::NotInWorkspace(_))
        ));

        let _ = fs::remove_dir_all(&base);
    }
}
