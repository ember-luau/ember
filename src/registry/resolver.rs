use crate::error::Error;
use crate::project::manifest::{
    Dependency, Environment, Language, Manifest, Override, dependency_target, override_paths,
    parse_version_req, takes,
};
use crate::project::workspace::{self, Workspace};
use crate::registry::index::{DownloadSource, Index, Refresh, ResolvedPackage};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;

/** what a project may install, set by its own environment and language,
so runtimes and languages do not reach into each other. a project takes
packages in its own language or in plain Luau, the base every language
reads. it takes packages for its own environment or portable Luau, and a
Roblox server takes shared Roblox packages too, wally's realm rule. */
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allow {
    pub environment: Environment,
    pub language: Language,
    /** the rule for a dev dependency. only this project's tests and tools
    use it, so it may be in any language. the environment rule stays. */
    pub any_language: bool,
}

impl Allow {
    /// the rule `manifest`'s own target sets. a manifest with no [target] only consumes, and sets none.
    pub fn for_project(manifest: &Manifest) -> Option<Allow> {
        let target = manifest.target()?;
        Some(Allow {
            environment: target.environment,
            language: target.language,
            any_language: false,
        })
    }

    /** whether a package in `language` for `environment` may install into
    the tree rooted at `tree`. None skips the environment: a direct
    dependency whose `target` says the crossing is on purpose. */
    pub fn admits(
        &self,
        language: Language,
        environment: Option<Environment>,
        tree: Option<Environment>,
    ) -> bool {
        (self.any_language || self.language.accepts(language))
            && match (tree, environment) {
                (Some(tree), Some(environment)) => tree.accepts(environment),
                _ => true,
            }
    }

    /// the refusal for `package`, a resolved `name@version`.
    pub fn refusal(
        &self,
        package: String,
        language: Language,
        environment: Option<Environment>,
    ) -> Error {
        let what = match environment {
            Some(environment) => format!("a {} package for {environment}", language.title()),
            None => format!("a {} package with no environment", language.title()),
        };
        /* a language cannot be crossed, a project cannot run what it does
        not read. an environment can, on purpose, the way a Lute project
        might vendor Roblox code it never runs */
        let what = if self.any_language || self.language.accepts(language) {
            format!("{what}. Set `target` on the dependency to take it on purpose")
        } else {
            what
        };
        Error::NotAllowed {
            project: format!("{} project for {}", self.language.title(), self.environment),
            takes: self.takes(),
            package,
            what,
        }
    }

    /// "Alloy or Luau packages for roblox or portable Luau", for the refusal.
    fn takes(&self) -> String {
        let languages = match self.language {
            Language::Luau => "Luau".to_string(),
            language => format!("{} or Luau", language.title()),
        };
        let mut environments = vec![self.environment.to_string()];
        if matches!(self.environment, Environment::Server | Environment::Lune) {
            environments.push(Environment::Roblox.to_string());
        }
        if self.environment != Environment::Luau {
            environments.push("portable Luau".to_string());
        }
        let environments = match environments.as_slice() {
            [one] => one.clone(),
            [first, second] => format!("{first} or {second}"),
            [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
            [] => unreachable!("a project has an environment"),
        };
        format!("{languages} packages for {environments}")
    }
}

/** the versions an existing lockfile holds, by lowercase package name.
a yanked version stays installable while it is in here. */
pub type Pinned = HashMap<String, Vec<semver::Version>>;

/// A package ready to download, the flattened result of resolution.
#[derive(Debug)]
pub struct ResolvedInstall {
    pub name: String,
    pub version: semver::Version,
    /// None means the environment must be detected from the extracted files.
    pub environment: Option<Environment>,
    /** The environment root this package installs under, the environment of
    the direct dependency whose subtree discovered it. Every root is
    self-contained, a server package's roblox dependencies live under
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
    /** only optional edges reach this package. install skips it, with one
    line, when its download fails. */
    pub optional: bool,
}

/** the nearest optional edge above a queued dependency, or the dependency
itself: its alias path from the root and its package. a failure below it
leaves the edge out, see [`resolve_skipping`]. */
type OptionalEdge = (Vec<String>, String);

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

/// What one specifier asks for: where it comes from, and its `target`, if any.
#[derive(Clone)]
struct Ask {
    request: Request,
    /** the specifier's parsed `target` override, see [`Dependency::target`].
    read only where the entry is a seed, i.e. a direct dependency of the
    manifest being installed, since that is the only kind whose tree is
    still open. an entry that reaches the queue with a context already
    inherited ignores it, and so `target` on a specifier embr only ever sees
    transitively -- an `[overrides]` value, or a workspace member's own
    dependency resolved from the root -- does nothing there. */
    target: Option<Environment>,
}

/// One dependency waiting to resolve.
struct QueueEntry {
    name: String,
    ask: Ask,
    /// manifest alias for direct deps, their top-level link. None for transitives.
    link: Option<String>,
    /// alias path from the root, for [overrides] matching and diagnostics.
    alias_path: Vec<String>,
    /** the inherited install root. None only for seeds, whose own resolve
    decides it. by the time children are queued it's always known. */
    context: Option<Environment>,
    /// who wanted this, for conflict diagnostics only.
    required_by: Requirer,
    /// None when no optional edge leads here.
    optional: Option<OptionalEdge>,
}

/** Who asked for a package. Only ever read to name the two sides of a
version conflict: the requirement texts alone say what disagrees but not
what to go and edit, and the disagreeing pair are usually a direct
dependency and something several levels below it. */
#[derive(Clone)]
enum Requirer {
    /// an entry under this project's own [dependencies].
    Manifest(String),
    /// another package's dependency: its "scope/name@version" and the alias it used.
    Package { id: String, alias: String },
}

impl Requirer {
    fn label(&self) -> String {
        match self {
            Requirer::Manifest(alias) => format!("ember.toml (as '{alias}')"),
            Requirer::Package { id, alias } => format!("{id} (as '{alias}')"),
        }
    }
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
    pinned: &Pinned,
    allow: Option<&Allow>,
    warnings: &mut Vec<String>,
) -> Result<Vec<ResolvedInstall>, Error> {
    let mut ttl_skipped = false;
    match resolve_skipping(
        manifest,
        project_dir,
        refresh,
        pinned,
        allow,
        &mut ttl_skipped,
        warnings,
    ) {
        /* an index whose pull was skipped by the TTL can be stale, and most
        resolver errors don't say which index they came from, so any
        failure after a skip earns one full forced refresh and a re-run.
        a skipped optional dependency counts as a failure, since a stale
        copy can lack the version it needs. the second outcome, good or
        bad, is the one that stands. */
        result if refresh == Refresh::Ttl && ttl_skipped && !matches!(result, Ok((_, false))) => {
            let mut ignored = false;
            warnings.clear();
            resolve_skipping(
                manifest,
                project_dir,
                Refresh::Force,
                pinned,
                allow,
                &mut ignored,
                warnings,
            )
            .map(|(installs, _)| installs)
        }
        result => result.map(|(installs, _)| installs),
    }
}

/** [`resolve_once`] until a run ends with no failure under an optional
edge. such a failure leaves that edge out, and the next run starts from
the top, so the optional package goes with all that it alone brought in.
each skip gives one line. the bool says whether any edge went out. */
fn resolve_skipping(
    manifest: &Manifest,
    project_dir: &Path,
    refresh: Refresh,
    pinned: &Pinned,
    allow: Option<&Allow>,
    ttl_skipped: &mut bool,
    warnings: &mut Vec<String>,
) -> Result<(Vec<ResolvedInstall>, bool), Error> {
    let mut indices: HashMap<String, Index> = HashMap::new();
    let mut excluded: HashSet<Vec<String>> = HashSet::new();
    let mut skips = Vec::new();
    loop {
        let mut failing = None;
        let mut run = Vec::new();
        let result = resolve_once(
            manifest,
            project_dir,
            refresh,
            pinned,
            allow,
            &mut indices,
            &excluded,
            ttl_skipped,
            &mut failing,
            &mut run,
        );
        match (result, failing) {
            (Err(error), Some((path, name))) if excluded.insert(path.clone()) => {
                skips.push(format!(
                    "skipped optional dependency '{}' ({name}): {error}",
                    path.join(".")
                ));
            }
            (result, _) => {
                let skipped = !skips.is_empty();
                warnings.append(&mut skips);
                warnings.append(&mut run);
                return result.map(|installs| (installs, skipped));
            }
        }
    }
}

/** one resolve of the graph. `excluded` holds the alias paths of the
optional edges to leave out. on an error, `failing` names the optional
edge that the error happened under, or stays None. */
#[allow(clippy::too_many_arguments)]
fn resolve_once(
    manifest: &Manifest,
    project_dir: &Path,
    refresh: Refresh,
    pinned: &Pinned,
    allow: Option<&Allow>,
    indices: &mut HashMap<String, Index>,
    excluded: &HashSet<Vec<String>>,
    ttl_skipped: &mut bool,
    failing: &mut Option<OptionalEdge>,
    warnings: &mut Vec<String>,
) -> Result<Vec<ResolvedInstall>, Error> {
    let prefer_environment = manifest.target().map(|target| target.environment);
    let language = manifest
        .target()
        .map(|target| target.language)
        .unwrap_or_default();
    /* Only walk up looking for a claiming workspace root once a workspace
    dep actually appears. */
    let mut workspace_memo: Option<Option<Workspace>> = None;

    /* [overrides] expanded and validated up front, one (name, request) per
    alias path. a dangling alias, unknown index key, or two keys covering
    the same path all fail before any network happens, whether or not the
    path ends up matching an edge. Each edge the walk discovers carries its
    alias path from the root ("foo" -> "foo.bar" -> ...), an exact match
    rewrites that edge before it's queued. */
    let mut overrides: HashMap<Vec<String>, (String, Ask)> = HashMap::new();
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
    transitively still links under its manifest alias. dev seeds wait in
    their own queue, see below. */
    let mut dev_seeds: VecDeque<QueueEntry> = VecDeque::new();
    for (seeds, dependencies) in [
        (&mut queue, &manifest.dependencies),
        (&mut dev_seeds, &manifest.dev_dependencies),
    ] {
        for (alias, dependency) in dependencies {
            let name = dependency_name(dependency).to_lowercase();
            let alias_path = vec![alias.clone()];
            if excluded.contains(&alias_path) {
                continue;
            }
            discovered.insert(alias_path.clone(), name.clone());
            seeds.push_back(QueueEntry {
                optional: dependency
                    .optional()
                    .then(|| (alias_path.clone(), name.clone())),
                name,
                ask: request_for(alias, dependency, manifest)?,
                link: Some(alias.clone()),
                alias_path,
                context: None,
                required_by: Requirer::Manifest(alias.clone()),
            });
        }
    }
    let dev_allow = allow.map(|allow| Allow {
        any_language: true,
        ..*allow
    });
    let mut dev = false;

    /* (name, context) -> (what we resolved, the req that won, who asked for
    it, the optional edge it came under). BTreeMap keeps the install set,
    and the lockfile written from it, in name order with context breaking
    ties. one package may appear once per context, every environment root
    is self-contained, wally-style. */
    let mut resolved: BTreeMap<(String, Environment), Resolved> = BTreeMap::new();
    /* the edges of the graph and whether each is optional, from the root
    and from each walked package. what no required edge reaches is optional */
    let mut root_edges: Vec<(Key, bool)> = Vec::new();
    let mut edges: HashMap<Key, Vec<(Key, bool)>> = HashMap::new();

    loop {
        /* the dev seeds start once every [dependencies] subtree has
        resolved. a package that both reach is then already in, and it went
        through the language check */
        if queue.is_empty() && !dev_seeds.is_empty() {
            dev = true;
            queue.append(&mut dev_seeds);
        }
        let Some(QueueEntry {
            name,
            ask: Ask { request, target },
            link,
            alias_path,
            context,
            required_by,
            optional,
        }) = queue.pop_front()
        else {
            break;
        };
        // an error from here on happened under this entry's optional edge
        failing.clone_from(&optional);
        let req_text = match &request {
            Request::Registry { req_text, .. } => req_text.clone(),
            Request::Workspace => "*".to_string(),
        };
        let req = parse_version_req(&req_text)?;

        /* transitives inherit their context, so their dedup gate runs
        before any network. a seed's context is decided by its own resolve,
        so its gate has to wait until just after, below */
        if let Some(context) = context
            && let Some((existing, first_req, first_by, first_optional)) =
                resolved.get(&(name.clone(), context))
        {
            /* a prerelease here was the latest of a package with no stable
            version, so another `^` takes it too */
            if takes(&req, &existing.version, true) {
                continue;
            }
            // a required edge that conflicts with an optional one leaves the optional one out
            if failing.is_none() {
                failing.clone_from(first_optional);
            }
            return Err(Error::DependencyConflict {
                name,
                context,
                first: first_req.clone(),
                first_by: first_by.label(),
                second: req_text,
                second_by: required_by.label(),
            });
        }

        /* a workspace member shadows the registry copy of its name in
        EVERY context. the member is the copy being developed, and a
        server tree quietly pulling the published version while roblox
        uses the local one would split the two. seeds all resolve before
        any transitive pops, so a seeded member is always visible here. */
        let request = match request {
            Request::Registry { .. }
                if resolved.iter().any(|((resolved_name, _), (install, ..))| {
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
        let mut children: Vec<(String, Ask, Vec<String>, String, bool)> = Vec::new();

        let (version, environment, source, index_url) = match request {
            Request::Registry { index_url, .. } => {
                let index = open_index(indices, &index_url, refresh, ttl_skipped)?;
                /* a multi-target entry picks the target of the tree it
                installs into. for a seed that tree is still undecided, so its
                `target`, then the project's own environment, stands in --
                `seed_context` below settles on exactly the same answer.
                an inherited context is NOT second-guessed by a `target`: the
                entry is going into that tree either way, and picking another
                target's archive for it would store a server build under the
                roblox root purely because some edge said so. */
                let preferred = match context {
                    Some(context) => Some(context),
                    None => target.or(prefer_environment),
                };
                let pins = pinned.get(&name).map(Vec::as_slice).unwrap_or_default();
                let preferred = preferred.map(|environment| (environment, language));
                let package = index.resolve(&name, &req, preferred, pins)?;
                /* a transitive package is judged by its tree's environment.
                a seed's `target` crosses environments on purpose, so a
                seed with one skips the environment, and its subtree is
                judged by the tree it chose */
                let tree = match context {
                    Some(context) => Some(context),
                    None if target.is_some() => None,
                    None => allow.map(|allow| allow.environment),
                };
                let rule = if dev { dev_allow.as_ref() } else { allow };
                if let Some(allow) = rule
                    && !allow.admits(package.language, package.environment, tree)
                {
                    return Err(allow.refusal(
                        format!("{name}@{}", package.version),
                        package.language,
                        package.environment,
                    ));
                }
                /* a package file's deprecations show in the install's
                notice block. Forest has no file, so it says them here */
                let deprecations = crate::registry::index::forest::is_forest(&index_url);
                warn_on_marks(&name, &package, deprecations, warnings);

                for dependency in &package.dependencies {
                    let mut child_path = alias_path.clone();
                    child_path.push(dependency.alias.clone());
                    if dependency.optional && excluded.contains(&child_path) {
                        continue;
                    }

                    let (child_name, child_ask) = match overrides.get(&child_path) {
                        Some((child_name, child_ask)) => {
                            pending_matched.push(child_path.clone());
                            redirects.insert(dependency.alias.clone(), child_name.clone());
                            (child_name.clone(), child_ask.clone())
                        }
                        None => (
                            dependency.name.clone(),
                            Ask {
                                request: Request::Registry {
                                    req_text: dependency.version_req.clone(),
                                    index_url: dependency
                                        .index_url
                                        .clone()
                                        .unwrap_or_else(|| index_url.clone()),
                                },
                                target: None,
                            },
                        ),
                    };
                    pending_discovered.push((child_path.clone(), child_name.clone()));
                    children.push((
                        child_name,
                        child_ask,
                        child_path,
                        dependency.alias.clone(),
                        dependency.optional,
                    ));
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
                let member_package = member
                    .manifest
                    .package
                    .as_ref()
                    .ok_or_else(|| Error::NoWorkspaceMember(name.clone()))?;
                let version = semver::Version::parse(&member_package.version)?;
                let environment = member
                    .manifest
                    .target()
                    .map(|target| target.environment)
                    .ok_or_else(|| Error::UnknownPackageEnvironment(name.clone()))?;

                /* The member's own deps install for the consumer too. its
                registry deps resolve against the member's [indices], but
                the consumer's [overrides] still apply to the edges. */
                for (alias, dependency) in &member.manifest.dependencies {
                    let mut child_path = alias_path.clone();
                    child_path.push(alias.clone());
                    if dependency.optional() && excluded.contains(&child_path) {
                        continue;
                    }

                    let (child_name, child_ask) = match overrides.get(&child_path) {
                        Some((child_name, child_ask)) => {
                            pending_matched.push(child_path.clone());
                            redirects.insert(alias.clone(), child_name.clone());
                            (child_name.clone(), child_ask.clone())
                        }
                        None => (
                            dependency_name(dependency).to_lowercase(),
                            request_for(alias, dependency, &member.manifest)?,
                        ),
                    };
                    pending_discovered.push((child_path.clone(), child_name.clone()));
                    children.push((
                        child_name,
                        child_ask,
                        child_path,
                        alias.clone(),
                        dependency.optional(),
                    ));
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
            None => seed_context(target, environment, prefer_environment, &name)?,
        };
        if seeded_here {
            /* recorded even when the gate below dedups, so unmatched-
            override diagnostics can still find this root's tree */
            seed_contexts.insert(alias_path[0].clone(), context);
            root_edges.push(((name.clone(), context), optional.is_some()));
            if let Some((existing, first_req, first_by, first_optional)) =
                resolved.get_mut(&(name.clone(), context))
            {
                if takes(&req, &existing.version, true) {
                    /* a dev seed that a [dependencies] subtree already
                    brought in still gets its link */
                    if existing.link.is_none() {
                        existing.link = link;
                    }
                    continue; // this tree already walked the package, children drop
                }
                if failing.is_none() {
                    failing.clone_from(first_optional);
                }
                return Err(Error::DependencyConflict {
                    name,
                    context,
                    first: first_req.clone(),
                    first_by: first_by.label(),
                    second: req_text,
                    second_by: required_by.label(),
                });
            }
        }

        /* a registry edge shadowed onto a member still states a version
        requirement, honor it like the dedup gate would have. workspace
        specifiers' own req is "*", so only converted edges can trip this */
        if matches!(source, DownloadSource::Workspace { .. }) && !req.matches(&version) {
            return Err(Error::WorkspaceMemberConflict {
                name,
                context,
                version: version.to_string(),
                req: req_text,
                required_by: required_by.label(),
            });
        }

        // the walk is real, commit its breadcrumbs and queue the edges
        walked_at
            .entry((name.clone(), context))
            .or_insert(alias_path.clone());
        overrides_matched.extend(pending_matched);
        discovered.extend(pending_discovered);
        let requirer_id = format!("{name}@{version}");
        let walked = edges.entry((name.clone(), context)).or_default();
        for (child_name, child_ask, child_path, child_alias, child_optional) in children {
            walked.push(((child_name.clone(), context), child_optional));
            queue.push_back(QueueEntry {
                optional: match child_optional {
                    true => Some((child_path.clone(), child_name.clone())),
                    false => optional.clone(),
                },
                name: child_name,
                ask: child_ask,
                link: None,
                alias_path: child_path,
                context: Some(context),
                required_by: Requirer::Package {
                    id: requirer_id.clone(),
                    alias: child_alias,
                },
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
                    optional: false,
                },
                req_text,
                required_by,
                optional,
            ),
        );
    }
    *failing = None;

    /* an override that never met its edge would otherwise be silently
    dead. two distinct reasons deserve distinct messages. a path that
    exists but wasn't walked, its package's edges were enumerated under an
    earlier discovery path since root aliases seed alphabetically, and a
    path that simply never appeared, a typo or a dependency that moved on. */
    let mut unmatched: Vec<&Vec<String>> = overrides
        .keys()
        .filter(|path| !overrides_matched.contains(*path))
        // an override below a skipped optional edge had nothing to meet
        .filter(|path| !excluded.iter().any(|edge| path.starts_with(edge)))
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

    let required = required_packages(&root_edges, &edges);
    Ok(resolved
        .into_iter()
        .map(|(key, (mut install, ..))| {
            install.optional = !required.contains(&key);
            install
        })
        .collect())
}

/// a package in one environment root, the key of the install set.
type Key = (String, Environment);

/// one entry of the install set while it resolves, see `resolved` in [`resolve_once`].
type Resolved = (ResolvedInstall, String, Requirer, Option<OptionalEdge>);

/** the packages that a path of required edges reaches from the root. an
optional package that a required one also needs is required. */
fn required_packages(
    root_edges: &[(Key, bool)],
    edges: &HashMap<Key, Vec<(Key, bool)>>,
) -> HashSet<Key> {
    let mut required = HashSet::new();
    let mut stack: Vec<&Key> = root_edges
        .iter()
        .filter(|(_, optional)| !optional)
        .map(|(key, _)| key)
        .collect();
    while let Some(key) = stack.pop() {
        if required.insert(key.clone()) {
            let children = edges.get(key).into_iter().flatten();
            stack.extend(
                children
                    .filter(|(_, optional)| !optional)
                    .map(|(child, _)| child),
            );
        }
    }
    required
}

/** what an overridden edge asks for instead, the root manifest's own
dependency when the override is an alias, or the inline specifier. Either
way index keys resolve against the ROOT manifest's [indices], the author
of the override is the one naming the index. */
fn overridden_edge(
    replacement: &Override,
    path: &[String],
    manifest: &Manifest,
) -> Result<(String, Ask), Error> {
    /* the label is what an error about this specifier tells the user to go
    and edit, so it has to follow the alias form to the [dependencies] entry
    it really borrows -- naming the override path there would send them
    looking for a dependency by that name, and there isn't one */
    let (label, dependency) = match replacement {
        Override::Alias(alias) => (
            alias.clone(),
            manifest
                .dependencies
                .get(alias)
                .ok_or_else(|| Error::OverrideAliasMissing {
                    path: path.join("."),
                    alias: alias.clone(),
                })?,
        ),
        Override::Specifier(dependency) => (path.join("."), dependency),
    };
    Ok((
        dependency_name(dependency).to_lowercase(),
        request_for(&label, dependency, manifest)?,
    ))
}

/** a seed's context is what its specifier `target`s, else its own resolved
environment, else the project's `[target]` one. placement is decided at
resolve time since children queue with it, so unlike a package's own
environment it can never wait for the archive to be extracted and
inspected. */
/** says when a picked package is yanked, and with `deprecations` when it
is deprecated. it still installs. */
fn warn_on_marks(
    name: &str,
    package: &ResolvedPackage,
    deprecations: bool,
    warnings: &mut Vec<String>,
) {
    let id = format!("{name}@{}", package.version);
    if package.yanked {
        warnings.push(format!(
            "warning: {id} is yanked; ember.lock pins it, so it stays"
        ));
    }
    if !deprecations {
        return;
    }
    // the reasons are registry text, and the caller prints the warnings
    if let Some(reason) = &package.package_deprecated {
        let reason = crate::ui::printable(reason);
        warnings.push(format!("warning: {name} is deprecated: {reason}"));
    }
    if let Some(reason) = &package.deprecated {
        let reason = crate::ui::printable(reason);
        warnings.push(format!("warning: {id} is deprecated: {reason}"));
    }
}

fn seed_context(
    target: Option<Environment>,
    environment: Option<Environment>,
    prefer: Option<Environment>,
    name: &str,
) -> Result<Environment, Error> {
    target
        .or(environment)
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
owner's [indices], so a member's deps use the member's. `alias` names the
entry in errors about it. */
fn request_for(alias: &str, dependency: &Dependency, owner: &Manifest) -> Result<Ask, Error> {
    let request = match dependency {
        Dependency::Registry { version, index, .. } => Request::Registry {
            req_text: version.clone(),
            index_url: owner.index_url(index.as_deref())?.to_string(),
        },
        Dependency::Workspace { .. } => Request::Workspace,
    };
    Ok(Ask {
        request,
        target: dependency_target(alias, dependency)?,
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
        let base = std::env::temp_dir().join("embr-test-resolver-workspace");
        let _ = fs::remove_dir_all(&base);

        write(
            &base,
            "ember.toml",
            "[package]\nname = \"acme/root\"\nversion = \"0.0.0\"\nprivate = true\n\n\
             [target]\nenvironment = \"roblox\"\nworkspace = [\"packages/*\"]\n",
        );
        write(
            &base.join("packages/core"),
            "ember.toml",
            "[package]\nname = \"acme/core\"\nversion = \"1.2.0\"\n\n\
             [target]\nenvironment = \"roblox\"\n",
        );
        write(
            &base.join("packages/extra"),
            "ember.toml",
            "[package]\nname = \"acme/extra\"\nversion = \"0.3.0\"\n\n\
             [target]\nenvironment = \"roblox\"\n\n\
             [dependencies]\ncore = { workspace = \"acme/core\", version = \"^\" }\n",
        );

        /* Resolving from a member, its workspace dep and that dep's own
        workspace dep all land in the install set, linked in place. */
        let member_dir = base.join("packages/extra");
        let manifest = Manifest::load_from(&member_dir.join("ember.toml")).unwrap();
        let installs = resolve(
            &manifest,
            &member_dir,
            Refresh::Never,
            &Pinned::new(),
            None,
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(installs.len(), 1);
        let core = &installs[0];
        assert_eq!(core.name, "acme/core");
        assert_eq!(core.version, semver::Version::new(1, 2, 0));
        assert_eq!(core.environment, Some(Environment::Roblox));
        assert_eq!(core.context, Environment::Roblox);
        assert_eq!(core.link.as_deref(), Some("core"));
        assert!(matches!(
            &core.source,
            DownloadSource::Workspace { path } if path == "../core"
        ));

        /* Resolving from the root, members resolve through the root's own
        member list, no ancestor needed, and members' transitive
        workspace deps come along. */
        let root_manifest_text = "[package]\nname = \"acme/root\"\nversion = \"0.0.0\"\nprivate = true\n\n\
             [target]\nenvironment = \"roblox\"\nworkspace = [\"packages/*\"]\n\n\
             [dependencies]\nextra = { workspace = \"acme/extra\" }\n";
        write(&base, "ember.toml", root_manifest_text);
        let manifest = Manifest::load_from(&base.join("ember.toml")).unwrap();
        let installs = resolve(
            &manifest,
            &base,
            Refresh::Never,
            &Pinned::new(),
            None,
            &mut Vec::new(),
        )
        .unwrap();
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
        /* the specifier's own `target` first, then the package's own
        environment, then [target], then a hard error */
        assert_eq!(
            seed_context(
                Some(Environment::Server),
                Some(Environment::Roblox),
                Some(Environment::Roblox),
                "a/b"
            )
            .unwrap(),
            Environment::Server
        );
        assert_eq!(
            seed_context(
                None,
                Some(Environment::Server),
                Some(Environment::Roblox),
                "a/b"
            )
            .unwrap(),
            Environment::Server
        );
        assert_eq!(
            seed_context(None, None, Some(Environment::Lune), "a/b").unwrap(),
            Environment::Lune
        );
        assert!(matches!(
            seed_context(None, None, None, "a/b"),
            Err(Error::UnknownPackageEnvironment(name)) if name == "a/b"
        ));
    }

    /** the self-contained-roots contract, against a real local git index.
    a server seed drags its whole subtree into the server context, roblox
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

        let origin = std::env::temp_dir().join("embr-test-contexts-origin");
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
                "user.name=embr-test",
                "-c",
                "user.email=embr-test@localhost",
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
            resolve(
                &manifest,
                &origin,
                Refresh::Never,
                &Pinned::new(),
                None,
                &mut Vec::new(),
            )
        };

        /* svc, a server package, pulls lib and pin into the server context.
        lib and pin are ALSO direct roblox deps, so both exist twice, once
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
                    install.context.dir_name(),
                    install.link.as_deref().unwrap_or("-")
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                "acme/lib@1.0.0 roblox/lib",
                "acme/lib@1.0.0 server/-",
                "acme/pin@2.0.0 roblox/pin",
                "acme/pin@1.0.0 server/-",
                "acme/svc@1.0.0 server/svc",
            ]
        );
        // every install keeps its own environment for nested-folder naming
        let server_lib = installs
            .iter()
            .find(|install| install.name == "acme/lib" && install.context == Environment::Server)
            .unwrap();
        assert_eq!(server_lib.environment, Some(Environment::Roblox));

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

        /* and it names both sides. the requirement texts alone say what
        disagrees, never which entry to go and edit */
        let named = resolve_with(
            "svc = { name = \"acme/svc\", version = \"^1\" }\n\
             later = { name = \"acme/svc\", version = \"^2\" }\n",
        )
        .unwrap_err()
        .to_string();
        assert!(named.contains("acme/svc"), "{named}");
        assert!(named.contains("ember.toml (as 'svc')"), "{named}");
        assert!(named.contains("ember.toml (as 'later')"), "{named}");

        let _ = fs::remove_dir_all(&cache);
    }

    /** a `target` on a [dependencies] entry moves that dependency, and its
    whole subtree, into the tree it names -- a Roblox package pulled in
    server-side only. the package keeps its own environment, which is what
    names the folder its dependents link it through. */
    #[test]
    fn dependency_targets_move_a_seed_into_another_tree() {
        /// removes the fixture dirs even when an assertion panics.
        struct Cleanup(Vec<std::path::PathBuf>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for dir in &self.0 {
                    let _ = fs::remove_dir_all(dir);
                }
            }
        }

        let origin = std::env::temp_dir().join("embr-test-dep-target-origin");
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
            origin.join("acme/core"),
            entry("acme/core", "1.0.0", r#""lib":"acme/lib@^1.0.0""#),
        )
        .unwrap();
        fs::write(
            origin.join("acme/lib"),
            entry("acme/lib", "1.0.0", "") + &entry("acme/lib", "2.0.0", ""),
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
                "user.name=embr-test",
                "-c",
                "user.email=embr-test@localhost",
            ];
            full.extend(args);
            crate::sys::git::run(&full).unwrap();
        }

        let resolve_with = |dependencies: &str| {
            let manifest: Manifest = toml::from_str(&format!(
                "[package]\nname = \"acme/game\"\nversion = \"0.1.0\"\n\n\
                 [target]\nenvironment = \"roblox\"\n\n\
                 [indices]\ndefault = \"{url}\"\n\n[dependencies]\n{dependencies}"
            ))
            .unwrap();
            resolve(
                &manifest,
                &origin,
                Refresh::Never,
                &Pinned::new(),
                None,
                &mut Vec::new(),
            )
        };

        // without a target, a roblox package lands in the roblox tree
        let plain = resolve_with("Core = { name = \"acme/core\", version = \"^1\" }\n").unwrap();
        assert!(
            plain
                .iter()
                .all(|install| install.context == Environment::Roblox)
        );

        /* with one, the seed AND everything it drags in move to the server
        tree, while each package's own environment is untouched */
        let targeted = resolve_with(
            "Core = { name = \"acme/core\", version = \"^1\", target = \"server\" }\n",
        )
        .unwrap();
        assert_eq!(targeted.len(), 2);
        for install in &targeted {
            assert_eq!(install.context, Environment::Server, "{}", install.name);
            assert_eq!(install.environment, Some(Environment::Roblox));
        }

        /* the same package under two trees is two installs, which is what
        makes "server-only copy plus the roblox one" expressible at all */
        let both = resolve_with(
            "Core = { name = \"acme/core\", version = \"^1\" }\n\
             ServerCore = { name = \"acme/core\", version = \"^1\", target = \"server\" }\n",
        )
        .unwrap();
        let contexts: Vec<Environment> = both
            .iter()
            .filter(|install| install.name == "acme/core")
            .map(|install| install.context)
            .collect();
        assert_eq!(contexts, [Environment::Roblox, Environment::Server]);

        // a target that names no environment is refused before any network
        assert!(matches!(
            resolve_with("Core = { name = \"acme/core\", version = \"^1\", target = \"sever\" }\n"),
            Err(Error::DependencyTargetInvalid { .. })
        ));

        /* an entry whose tree is already inherited ignores `target` outright
        rather than half-honoring it. an [overrides] specifier is the
        reachable case: acme/core's `lib` edge is redirected, and the
        replacement lands in acme/core's tree like any other transitive
        would -- it may not be dragged into the server root, where nothing
        that requires it lives */
        let overridden = resolve_with(
            "Core = { name = \"acme/core\", version = \"^1\" }\n\n\
             [overrides]\n\
             \"Core.lib\" = { name = \"acme/lib\", version = \"^2\", target = \"server\" }\n",
        )
        .unwrap();
        let lib = overridden
            .iter()
            .find(|install| install.name == "acme/lib")
            .unwrap();
        assert_eq!(lib.context, Environment::Roblox);
        assert_eq!(lib.version, semver::Version::new(2, 0, 0));

        /* and the error about a bad one names the [dependencies] entry to go
        and edit, not the override path, which is nobody's dependency */
        let mislabeled = resolve_with(
            "Core = { name = \"acme/core\", version = \"^1\" }\n\
             Lib = { name = \"acme/lib\", version = \"^2\", target = \"sever\" }\n\n\
             [overrides]\n\"Core.lib\" = \"Lib\"\n",
        )
        .unwrap_err();
        assert!(
            matches!(&mislabeled, Error::DependencyTargetInvalid { alias, .. } if alias == "Lib"),
            "{mislabeled:?}"
        );

        /* separating the two into different trees also separates their
        version requirements: acme/core wants lib ^1.0.0 and the manifest
        wants ^2, which conflicts only while they share a tree */
        let conflict = resolve_with(
            "Core = { name = \"acme/core\", version = \"^1\" }\n\
             Lib = { name = \"acme/lib\", version = \"^2\" }\n",
        )
        .unwrap_err()
        .to_string();
        // the transitive side names the package that asked, not just its req
        assert!(
            conflict.contains("acme/core@1.0.0 (as 'lib')"),
            "{conflict}"
        );
        assert!(conflict.contains("ember.toml (as 'Lib')"), "{conflict}");
        assert!(conflict.contains("in the roblox tree"), "{conflict}");

        assert!(
            resolve_with(
                "Core = { name = \"acme/core\", version = \"^1\", target = \"server\" }\n\
                 Lib = { name = \"acme/lib\", version = \"^2\" }\n",
            )
            .is_ok()
        );

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

        let origin = std::env::temp_dir().join("embr-test-shadow-origin");
        let project = std::env::temp_dir().join("embr-test-shadow-project");
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
                "user.name=embr-test",
                "-c",
                "user.email=embr-test@localhost",
            ];
            full.extend(args);
            crate::sys::git::run(&full).unwrap();
        }

        write(
            &project.join("packages/core"),
            "ember.toml",
            "[package]\nname = \"acme/core\"\nversion = \"1.2.0\"\n\n\
             [target]\nenvironment = \"roblox\"\n",
        );
        let resolve_with = |server_dep: &str| {
            write(
                &project,
                "ember.toml",
                &format!(
                    "[package]\nname = \"acme/root\"\nversion = \"0.0.0\"\nprivate = true\n\n\
                     [target]\nenvironment = \"roblox\"\nworkspace = [\"packages/*\"]\n\n\
                     [indices]\ndefault = \"{url}\"\n\n\
                     [dependencies]\ncore = {{ workspace = \"acme/core\" }}\n\
                     {server_dep} = {{ name = \"acme/{server_dep}\", version = \"^1\" }}\n"
                ),
            );
            let manifest = Manifest::load_from(&project.join("ember.toml")).unwrap();
            resolve(
                &manifest,
                &project,
                Refresh::Never,
                &Pinned::new(),
                None,
                &mut Vec::new(),
            )
        };

        /* svc's server-tree edge to acme/core binds to the member. same
        version as the roblox copy, 1.2.0 not the registry's 1.0.0,
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
        assert_eq!(cores[0].context, Environment::Roblox);
        assert_eq!(cores[0].link.as_deref(), Some("core"));
        assert_eq!(cores[1].context, Environment::Server);
        assert_eq!(cores[1].link, None);

        // an edge whose requirement the member can't satisfy is a conflict
        let conflict = resolve_with("strict").unwrap_err();
        assert!(matches!(
            conflict,
            Error::WorkspaceMemberConflict {
                context: Environment::Server,
                ..
            }
        ));
        // and says who wanted what, against the version really on disk
        let message = conflict.to_string();
        assert!(
            message.contains("acme/strict@1.0.0 (as 'core')"),
            "{message}"
        );
        assert!(message.contains("1.2.0"), "{message}");
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

        let origin = std::env::temp_dir().join("embr-test-overrides-origin");
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
            "user.name=embr-test",
            "-c",
            "user.email=embr-test@localhost",
            "init",
        ])
        .unwrap();
        crate::sys::git::run(&["-C", origin.to_str().unwrap(), "add", "."]).unwrap();
        crate::sys::git::run(&[
            "-C",
            origin.to_str().unwrap(),
            "-c",
            "user.name=embr-test",
            "-c",
            "user.email=embr-test@localhost",
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
            resolve(
                &manifest,
                &origin,
                Refresh::Never,
                &Pinned::new(),
                None,
                warnings,
            )
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

        /* an override addressed through the SECOND parent of a roblox
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
        let base = std::env::temp_dir().join("embr-test-resolver-no-workspace");
        let _ = fs::remove_dir_all(&base);
        write(
            &base,
            "ember.toml",
            "[package]\nname = \"acme/lonely\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ncore = { workspace = \"acme/core\" }\n",
        );

        let manifest = Manifest::load_from(&base.join("ember.toml")).unwrap();
        assert!(matches!(
            resolve(
                &manifest,
                &base,
                Refresh::Never,
                &Pinned::new(),
                None,
                &mut Vec::new()
            ),
            Err(Error::NotInWorkspace(_))
        ));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn yanks_and_deprecations_warn_without_failing() {
        let package = ResolvedPackage {
            version: semver::Version::new(1, 0, 0),
            environment: Some(Environment::Roblox),
            language: Language::Luau,
            yanked: true,
            deprecated: Some("leaks".to_string()),
            package_deprecated: Some("use acme/new".to_string()),
            dependencies: Vec::new(),
            source: DownloadSource::TarGz { url: String::new() },
        };
        let mut warnings = Vec::new();
        warn_on_marks("acme/thing", &package, true, &mut warnings);
        assert_eq!(
            warnings,
            [
                "warning: acme/thing@1.0.0 is yanked; ember.lock pins it, so it stays",
                "warning: acme/thing is deprecated: use acme/new",
                "warning: acme/thing@1.0.0 is deprecated: leaks",
            ]
        );
    }

    /** a deprecation reason is registry text. OSC 52 in it would set the
    clipboard, and CR with an erase sequence would hide the warning. */
    #[test]
    fn a_package_file_says_its_deprecations_in_the_notice_block_instead() {
        let package = ResolvedPackage {
            version: semver::Version::new(1, 0, 0),
            environment: Some(Environment::Roblox),
            language: Language::Luau,
            yanked: false,
            deprecated: Some("leaks".to_string()),
            package_deprecated: Some("use acme/new".to_string()),
            dependencies: Vec::new(),
            source: DownloadSource::TarGz { url: String::new() },
        };
        let mut warnings = Vec::new();
        warn_on_marks("acme/thing", &package, false, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn deprecation_reasons_reach_the_terminal_without_escapes() {
        let package = ResolvedPackage {
            version: semver::Version::new(1, 0, 0),
            environment: Some(Environment::Roblox),
            language: Language::Luau,
            yanked: false,
            deprecated: Some("\x1b]52;c;Y3VybCBldmlsfHNo\x07old".to_string()),
            package_deprecated: Some("x\r\x1b[2K\u{9b}0m\u{8}ok".to_string()),
            dependencies: Vec::new(),
            source: DownloadSource::TarGz { url: String::new() },
        };
        let mut warnings = Vec::new();
        warn_on_marks("acme/thing", &package, true, &mut warnings);
        assert_eq!(
            warnings,
            [
                r"warning: acme/thing is deprecated: x\u{d}\u{1b}[2K\u{9b}0m\u{8}ok",
                r"warning: acme/thing@1.0.0 is deprecated: \u{1b}]52;c;Y3VybCBldmlsfHNo\u{7}old",
            ]
        );
    }

    #[test]
    fn projects_do_not_reach_into_each_other() {
        use Language::*;
        let rule = |environment, language| Allow {
            environment,
            language,
            any_language: false,
        };

        let alloy = rule(Environment::Roblox, Alloy);
        let tree = Some(Environment::Roblox);
        // its own language and plain Luau, for its environment or portable
        assert!(alloy.admits(Alloy, Some(Environment::Roblox), tree));
        assert!(alloy.admits(Luau, Some(Environment::Roblox), tree));
        assert!(alloy.admits(Luau, Some(Environment::Luau), tree));
        // not another runtime, and not another language
        assert!(!alloy.admits(Alloy, Some(Environment::Lune), tree));
        assert!(!alloy.admits(Loom, Some(Environment::Roblox), tree));
        assert!(!alloy.admits(Luwu, Some(Environment::Luau), tree));

        let lute = rule(Environment::Lute, Luau);
        let tree = Some(Environment::Lute);
        assert!(lute.admits(Luau, Some(Environment::Lute), tree));
        assert!(!lute.admits(Luau, Some(Environment::Roblox), tree));
        assert!(!lute.admits(Alloy, Some(Environment::Lute), tree));
        // a seed's `target` crosses the environment on purpose
        assert!(lute.admits(Luau, Some(Environment::Roblox), None));
        // Lune runs Roblox code through its DOM
        let lune = rule(Environment::Lune, Luau);
        assert!(lune.admits(Luau, Some(Environment::Roblox), Some(Environment::Lune)));

        // a Roblox server takes shared Roblox packages, wally's realm rule
        let server = rule(Environment::Server, Luau);
        assert!(server.admits(Luau, Some(Environment::Roblox), Some(Environment::Server)));

        assert_eq!(
            alloy
                .refusal("acme/fs@1.0.0".to_string(), Luau, Some(Environment::Lune))
                .to_string(),
            "This Alloy project for roblox takes Alloy or Luau packages for roblox or portable Luau; acme/fs@1.0.0 is a Luau package for lune. Set `target` on the dependency to take it on purpose"
        );
        assert_eq!(
            server
                .refusal("acme/x@1.0.0".to_string(), Loom, Some(Environment::Roblox))
                .to_string(),
            "This Luau project for roblox_server takes Luau packages for roblox_server, roblox, or portable Luau; acme/x@1.0.0 is a Loom package for roblox"
        );
    }

    /** a dev dependency may be in any language, since only this project's
    tests and tools use it. the environment rule stays as it is. */
    #[test]
    fn dev_dependencies_take_any_language_but_not_any_environment() {
        use Language::*;
        let luau = Allow {
            environment: Environment::Lune,
            language: Luau,
            any_language: false,
        };
        let dev = Allow {
            any_language: true,
            ..luau
        };
        let tree = Some(Environment::Lune);
        assert!(!luau.admits(Alloy, Some(Environment::Lune), tree));
        for language in [Luau, Alloy, Loom, Luwu] {
            assert!(
                dev.admits(language, Some(Environment::Lune), tree),
                "{language}"
            );
        }
        assert!(!dev.admits(Alloy, Some(Environment::Lute), tree));
        // so the refusal points at `target`, the way out for an environment
        assert!(
            dev.refusal(
                "alloy/lest@0.2.0".to_string(),
                Alloy,
                Some(Environment::Lute)
            )
            .to_string()
            .ends_with("Set `target` on the dependency to take it on purpose")
        );
    }

    /** the root's dev dependencies install, with their links. a workspace
    member's dev dependencies stay out when the root installs it, as the dev
    dependencies of any dependency do. */
    #[test]
    fn dev_dependencies_install_for_the_root_only() {
        let base = std::env::temp_dir().join("embr-test-resolver-dev");
        let _ = fs::remove_dir_all(&base);
        let member = |name: &str, tables: &str| {
            write(
                &base.join("packages").join(name),
                "ember.toml",
                &format!(
                    "[package]\nname = \"acme/{name}\"\nversion = \"1.0.0\"\n\n\
                     [target]\nenvironment = \"roblox\"\n\n{tables}"
                ),
            );
        };
        member(
            "app",
            "[dependencies]\ncore = { workspace = \"acme/core\" }\n\n\
             [dev-dependencies]\nspy = { workspace = \"acme/spy\" }\n",
        );
        member("core", "");
        member("lest", "");
        member("spy", "");
        write(
            &base,
            "ember.toml",
            "[target]\nenvironment = \"roblox\"\nworkspace = [\"packages/*\"]\n\n\
             [dependencies]\napp = { workspace = \"acme/app\" }\n\n\
             [dev-dependencies]\nlest = { workspace = \"acme/lest\" }\n\
             kore = { workspace = \"acme/core\" }\n",
        );

        let manifest = Manifest::load_from(&base.join("ember.toml")).unwrap();
        let installs = resolve(
            &manifest,
            &base,
            Refresh::Never,
            &Pinned::new(),
            None,
            &mut Vec::new(),
        )
        .unwrap();
        let summary: Vec<String> = installs
            .iter()
            .map(|install| {
                format!(
                    "{} {}",
                    install.name,
                    install.link.as_deref().unwrap_or("-")
                )
            })
            .collect();
        /* acme/spy is a dev dependency of app, so it stays out. acme/core
        came in through app first, and still links as the dev alias `kore` */
        assert_eq!(
            summary,
            ["acme/app app", "acme/core kore", "acme/lest lest"]
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// a pesde-format git index, as the ember index is, removed on drop.
    struct PesdeOrigin {
        url: String,
        dirs: Vec<std::path::PathBuf>,
    }

    impl PesdeOrigin {
        /** `files` holds (path, text) pairs. an entry names its download, so
        the config needs no api. */
        fn new(name: &str, files: &[(&str, String)]) -> Self {
            let origin = std::env::temp_dir().join(format!("embr-test-{name}-origin"));
            let _ = fs::remove_dir_all(&origin);
            write(&origin, "config.toml", "");
            for (path, text) in files {
                write(&origin, path, text);
            }
            for args in [
                vec!["init"],
                vec!["add", "."],
                vec!["commit", "-m", "fixture"],
            ] {
                let mut full = vec![
                    "-C",
                    origin.to_str().unwrap(),
                    "-c",
                    "user.name=embr-test",
                    "-c",
                    "user.email=embr-test@localhost",
                ];
                full.extend(args);
                crate::sys::git::run(&full).unwrap();
            }
            let url = origin.to_string_lossy().replace('\\', "/");
            let cache = crate::registry::index::cache_dir(&url).unwrap();
            let _ = fs::remove_dir_all(&cache);
            PesdeOrigin {
                url,
                dirs: vec![origin, cache],
            }
        }

        /// resolves `tables` of a Roblox project against this index, with its rule.
        fn resolve(&self, tables: &str) -> (Result<Vec<ResolvedInstall>, Error>, Vec<String>) {
            let manifest: Manifest = toml::from_str(&format!(
                "[target]\nenvironment = \"roblox\"\n\n[indices]\ndefault = \"{}\"\n\n{tables}",
                self.url
            ))
            .unwrap();
            let mut warnings = Vec::new();
            let result = resolve(
                &manifest,
                &self.dirs[0],
                Refresh::Never,
                &Pinned::new(),
                Allow::for_project(&manifest).as_ref(),
                &mut warnings,
            );
            (result, warnings)
        }
    }

    impl Drop for PesdeOrigin {
        fn drop(&mut self) {
            for dir in &self.dirs {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }

    /// one entry of an ember index file, with extra `tables` under its key.
    fn ember_entry(name: &str, version: &str, target: &str, tables: &str) -> String {
        let key = format!("{version} {target}");
        let tables = tables.replace("[.", &format!("[\"{key}\"."));
        format!(
            "[\"{key}\"]\nname = \"{name}\"\nversion = \"{version}\"\ntarget = \"{target}\"\n\
             download = \"https://cdn.example/{name}/{version}.tar.gz\"\n{tables}\n"
        )
    }

    /// `name@version context link optional` for each install.
    fn summary(installs: &[ResolvedInstall]) -> Vec<String> {
        installs
            .iter()
            .map(|install| {
                format!(
                    "{}@{} {} {}{}",
                    install.name,
                    install.version,
                    install.context.as_str(),
                    install.link.as_deref().unwrap_or("-"),
                    if install.optional { " optional" } else { "" }
                )
            })
            .collect()
    }

    /** an optional dependency installs when it resolves. when it does not,
    or a dependency of it does not, or the project's rule refuses it, the
    install skips it with one line and goes on. a peer never installs. */
    #[test]
    fn optional_dependencies_install_when_they_resolve_and_skip_when_not() {
        let origin = PesdeOrigin::new(
            "optional",
            &[
                (
                    "acme/ui",
                    ember_entry(
                        "acme/ui",
                        "1.0.0",
                        "roblox",
                        "[.dependencies.core]\nname = \"acme/core\"\nversion = \"^1.0.0\"\n\
                         [.optional-dependencies.trace]\nname = \"acme/trace\"\nversion = \"^1.0.0\"\noptional = true\n\
                         [.optional-dependencies.ghost]\nname = \"acme/ghost\"\nversion = \"^1.0.0\"\noptional = true\n\
                         [.peer-dependencies.React]\nname = \"jsdotlua/react\"\nversion = \"^17.0.0\"\n",
                    ),
                ),
                ("acme/core", ember_entry("acme/core", "1.0.0", "roblox", "")),
                (
                    "acme/trace",
                    ember_entry("acme/trace", "1.0.0", "roblox", ""),
                ),
                (
                    "acme/broken",
                    ember_entry(
                        "acme/broken",
                        "1.0.0",
                        "roblox",
                        "[.dependencies.gone]\nname = \"acme/gone\"\nversion = \"^1.0.0\"\n",
                    ),
                ),
                (
                    "acme/srv",
                    ember_entry("acme/srv", "1.0.0", "roblox_server", ""),
                ),
                (
                    "acme/runner",
                    ember_entry("acme/runner", "1.0.0", "lune", ""),
                ),
                (
                    "jsdotlua/react",
                    ember_entry("jsdotlua/react", "17.0.2", "roblox", ""),
                ),
            ],
        );

        let (installs, warnings) = origin.resolve(
            "[dependencies]\n\
             ui = { name = \"acme/ui\", version = \"^1\" }\n\
             signal = { name = \"acme/signal\", version = \"^1\", optional = true }\n\
             broken = { name = \"acme/broken\", version = \"^1\", optional = true }\n\
             srv = { name = \"acme/srv\", version = \"^1\", optional = true, target = \"roblox_server\" }\n\
             runner = { name = \"acme/runner\", version = \"^1\", optional = true }\n",
        );
        assert_eq!(
            summary(&installs.unwrap()),
            [
                "acme/core@1.0.0 roblox -",
                "acme/srv@1.0.0 roblox_server srv optional",
                "acme/trace@1.0.0 roblox - optional",
                "acme/ui@1.0.0 roblox ui",
            ]
        );
        let skipped: Vec<&str> = warnings
            .iter()
            .filter_map(|line| line.strip_prefix("skipped optional dependency "))
            .collect();
        assert_eq!(skipped.len(), 4, "{warnings:?}");
        for (edge, why) in [
            ("'broken' (acme/broken)", "acme/gone"),
            ("'runner' (acme/runner)", "a Luau package for lune"),
            ("'signal' (acme/signal)", "acme/signal was not found"),
            ("'ui.ghost' (acme/ghost)", "acme/ghost was not found"),
        ] {
            assert!(
                skipped
                    .iter()
                    .any(|line| line.starts_with(edge) && line.contains(why)),
                "{edge}: {warnings:?}"
            );
        }

        // an optional package that a required edge also reaches is required
        let (installs, warnings) = origin.resolve(
            "[dependencies]\n\
             ui = { name = \"acme/ui\", version = \"^1\" }\n\
             trace = { name = \"acme/trace\", version = \"^1\" }\n",
        );
        let installs = installs.unwrap();
        let trace = installs.iter().find(|i| i.name == "acme/trace").unwrap();
        assert!(!trace.optional);
        assert_eq!(trace.link.as_deref(), Some("trace"));
        assert_eq!(warnings.len(), 1, "{warnings:?}");

        // a required dependency that does not resolve still fails the install
        let (installs, _) = origin
            .resolve("[dependencies]\nsignal = { name = \"acme/signal\", version = \"^1\" }\n");
        assert!(matches!(installs, Err(Error::PackageNotFound { .. })));
    }

    /** a version conflict between an optional subtree and a required one
    leaves the optional edge out, whichever side the walk meets first. */
    #[test]
    fn an_optional_subtree_that_conflicts_is_left_out() {
        let lib = |version: &str| ember_entry("acme/lib", version, "roblox", "");
        let needs = |name: &str, req: &str| {
            ember_entry(
                name,
                "1.0.0",
                "roblox",
                &format!("[.dependencies.lib]\nname = \"acme/lib\"\nversion = \"{req}\"\n"),
            )
        };
        let origin = PesdeOrigin::new(
            "optional-conflict",
            &[
                ("acme/lib", lib("1.0.0") + &lib("2.0.0")),
                ("acme/a", needs("acme/a", "^1.0.0")),
                ("acme/b", needs("acme/b", "^2.0.0")),
            ],
        );
        for tables in [
            "[dependencies]\na = { name = \"acme/a\", version = \"^1\" }\n\
             b = { name = \"acme/b\", version = \"^1\", optional = true }\n",
            // the optional side first, so the conflict shows on the required edge
            "[dependencies]\nz = { name = \"acme/a\", version = \"^1\" }\n\
             b = { name = \"acme/b\", version = \"^1\", optional = true }\n",
        ] {
            let (installs, warnings) = origin.resolve(tables);
            let installs = summary(&installs.unwrap());
            assert!(
                installs.contains(&"acme/lib@1.0.0 roblox -".to_string()),
                "{installs:?}"
            );
            assert!(
                !installs.iter().any(|i| i.starts_with("acme/b")),
                "{installs:?}"
            );
            assert!(
                warnings[0].starts_with("skipped optional dependency 'b' (acme/b)"),
                "{warnings:?}"
            );
        }
    }

    #[test]
    fn every_project_with_a_target_sets_a_rule() {
        let alloy: Manifest =
            toml::from_str("[target]\nenvironment = \"roblox\"\nlanguage = \"alloy\"\n").unwrap();
        assert_eq!(
            Allow::for_project(&alloy),
            Some(Allow {
                environment: Environment::Roblox,
                language: Language::Alloy,
                any_language: false,
            })
        );
        let consumer: Manifest = toml::from_str("[dependencies]\n").unwrap();
        assert_eq!(Allow::for_project(&consumer), None);
    }
}
