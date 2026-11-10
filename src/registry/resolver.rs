use crate::error::Error;
use crate::project::lockfile::{Lockfile, lockfile_in};
use crate::project::manifest::features::{self, Features};
use crate::project::manifest::{
    Dependency, Environment, Language, Manifest, Override, dependency_target, manifest_in,
    override_paths, parse_version_req, takes,
};
use crate::project::workspace::{self, Workspace};
use crate::registry::index::git::{self, Reference};
use crate::registry::index::{DownloadSource, Index, Refresh, ResolvedPackage};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

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

    /// "Alloy or Luau packages for roblox or portable", for the refusal.
    fn takes(&self) -> String {
        let languages = match self.language {
            Language::Luau => "Luau".to_string(),
            language => format!("{} or Luau", language.title()),
        };
        let mut environments = vec![self.environment.manifest_name().to_string()];
        if matches!(self.environment, Environment::Server | Environment::Lune) {
            environments.push(Environment::Roblox.to_string());
        }
        if self.environment != Environment::Luau {
            environments.push("portable".to_string());
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
a yanked version stays installable while it is in here, and a resolve
keeps each one that its requirements still take, see [`resolve_updating`]. */
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
    /// the archive's SHA-256 from the index entry, None when it records none.
    pub sha256: Option<String>,
    pub index_url: String,
    /** Name of the generated top-level link file, the [dependencies] alias
    for direct deps. None for transitives, only declared dependencies are
    part of the project's require surface. */
    pub link: Option<String>,
    /** Edges of THIS package that [overrides] rewrote, its declared alias ->
    the replacement's package name. The nested-link pass reads a package's
    declared dependencies back off disk by name, so a name-changing override
    must travel to the linker and the lockfile or the dependent would link
    the original, or nothing. a path or git edge without `name` lands here
    too, with the name its own manifest gave. */
    pub redirects: BTreeMap<String, String>,
    /** only optional edges reach this package. install skips it, with one
    line, when its download fails. */
    pub optional: bool,
    /** its enabled features, sorted: the union over every dependent in this
    environment root, with the features that those turn on. */
    pub features: Vec<String>,
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
    /// a path dependency: its folder, absolute and without `.` or `..`.
    Path {
        dir: PathBuf,
    },
    /// a git dependency, see [`git::locate`].
    Git {
        url: String,
        reference: Reference,
    },
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
    /// the features the specifier turns on, see [`Dependency::features`].
    features: BTreeSet<String>,
    /// whether it keeps the package's `default` features on.
    defaults: bool,
}

/** one dependency edge of a walked package: the package it names, what it
asks for, its alias path, its alias, and whether it is optional. */
#[derive(Clone)]
struct Child {
    name: String,
    ask: Ask,
    path: Vec<String>,
    alias: String,
    optional: bool,
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
    resolve_updating(manifest, project_dir, refresh, pinned, allow, &[], warnings)
}

/** [`resolve`], and the packages that `update` names leave ember.lock, as
`--refresh` makes every one of them do.

the lock policy is Cargo's. a registry package keeps the version in
`pinned` while its requirements take it. a new entry, a requirement that
no longer takes the locked version, `update` and `--refresh` resolve to the
newest version. a git package keeps its commit in the same cases. */
pub fn resolve_updating(
    manifest: &Manifest,
    project_dir: &Path,
    refresh: Refresh,
    pinned: &Pinned,
    allow: Option<&Allow>,
    update: &[String],
    warnings: &mut Vec<String>,
) -> Result<Vec<ResolvedInstall>, Error> {
    let mut ttl_skipped = false;
    /* the retry below forces a refresh of the indices, not of the git
    commits or the locked versions. only `--refresh` and `--update` move
    those */
    let pins = GitPins::read(project_dir, refresh == Refresh::Force, update);
    let keep: Pinned = if refresh == Refresh::Force {
        Pinned::new()
    } else {
        pinned
            .iter()
            .filter(|(name, _)| !update.iter().any(|named| named.eq_ignore_ascii_case(name)))
            .map(|(name, versions)| (name.clone(), versions.clone()))
            .collect()
    };
    match resolve_skipping(
        manifest,
        project_dir,
        refresh,
        pinned,
        &keep,
        allow,
        &pins,
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
                &keep,
                allow,
                &pins,
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
each skip gives one line. the bool says whether any edge went out.

a conflict on a package that `keep` holds drops that lock version and
runs again before anything is skipped. the resolver takes the first
version that an edge picks, so a kept version that a later edge refuses
must not fail the install: the newest version can take both. */
#[allow(clippy::too_many_arguments)]
fn resolve_skipping(
    manifest: &Manifest,
    project_dir: &Path,
    refresh: Refresh,
    pinned: &Pinned,
    keep: &Pinned,
    allow: Option<&Allow>,
    pins: &GitPins,
    ttl_skipped: &mut bool,
    warnings: &mut Vec<String>,
) -> Result<(Vec<ResolvedInstall>, bool), Error> {
    let mut indices: HashMap<String, Index> = HashMap::new();
    let mut excluded: HashSet<Vec<String>> = HashSet::new();
    let mut skips = Vec::new();
    let mut keep = keep.clone();
    loop {
        let mut failing = None;
        let mut run = Vec::new();
        let result = resolve_once(
            manifest,
            project_dir,
            refresh,
            pinned,
            &keep,
            allow,
            pins,
            &mut indices,
            &excluded,
            ttl_skipped,
            &mut failing,
            &mut run,
        );
        match (result, failing) {
            (Err(Error::DependencyConflict { name, .. }), _) if keep.remove(&name).is_some() => {}
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
    keep: &Pinned,
    allow: Option<&Allow>,
    pins: &GitPins,
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
            let edge = overridden_edge(value, &path, manifest, project_dir)?;
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
    // the alias path of each path or git edge without `name`, and the name it read
    let mut unnamed: Vec<(Vec<String>, String)> = Vec::new();
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
                ask: request_for(alias, dependency, manifest, Some(project_dir))?,
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
            ask:
                Ask {
                    request,
                    target,
                    features: asked,
                    defaults,
                },
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
            Request::Workspace | Request::Path { .. } | Request::Git { .. } => "*".to_string(),
        };
        let req = parse_version_req(&req_text)?;
        /* a path or git package says its own name, so it is read before the
        gates below, which go by name */
        let local = match &request {
            Request::Path { .. } | Request::Git { .. } => {
                let alias = alias_path.last().map_or("", String::as_str);
                Some(locate(&request, &name, alias, project_dir, pins)?)
            }
            Request::Registry { .. } | Request::Workspace => None,
        };
        let name = match &local {
            Some(local) => {
                if name.is_empty() && alias_path.len() > 1 {
                    unnamed.push((alias_path.clone(), local.name.clone()));
                }
                local.name.clone()
            }
            None => name,
        };

        /* transitives inherit their context, so their dedup gate runs
        before any network. a seed's context is decided by its own resolve,
        so its gate has to wait until just after, below */
        if let Some(context) = context
            && let Some(existing) = resolved.get_mut(&(name.clone(), context))
        {
            /* a prerelease here was the latest of a package with no stable
            version, so another `^` takes it too. a path or git copy must
            be the same copy. the features that this edge asks for join the
            package's, and what they turn on queues */
            if takes(&req, &existing.install.version, true) {
                let local = local.as_ref().map(|local| &local.source);
                same_source(
                    &name,
                    context,
                    local,
                    &existing.install,
                    &existing.by,
                    &required_by,
                )?;
                existing.enable(&asked, defaults, &required_by)?;
                existing.queue_edges(&mut queue, &mut edges);
                continue;
            }
            // a required edge that conflicts with an optional one leaves the optional one out
            if failing.is_none() {
                failing.clone_from(&existing.optional);
            }
            return Err(Error::DependencyConflict {
                name,
                context,
                first: existing.req.clone(),
                first_by: existing.by.label(),
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
                if resolved.iter().any(|((resolved_name, _), existing)| {
                    *resolved_name == name
                        && matches!(existing.install.source, DownloadSource::Workspace { .. })
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
        let mut children: Vec<Child> = Vec::new();

        let (version, environment, source, sha256, index_url, table) = match request {
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
                let package = keep_locked(index, &name, &req, preferred, pins, keep, package);
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
                let deprecations = crate::registry::index::forest::is_forest(&index_url)
                    || crate::registry::index::luarocks::is_luarocks(&index_url);
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
                                features: dependency.features.iter().cloned().collect(),
                                defaults: dependency.default_features,
                            },
                        ),
                    };
                    pending_discovered.push((child_path.clone(), child_name.clone()));
                    children.push(Child {
                        name: child_name,
                        ask: child_ask,
                        path: child_path,
                        alias: dependency.alias.clone(),
                        optional: dependency.optional,
                    });
                }

                (
                    package.version,
                    package.environment,
                    package.source,
                    package.sha256,
                    index_url,
                    package.features,
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
                manifest_edges(
                    &member.manifest,
                    Some(&member.dir),
                    &alias_path,
                    &overrides,
                    excluded,
                    &mut pending_matched,
                    &mut pending_discovered,
                    &mut redirects,
                    &mut children,
                )?;

                let path =
                    workspace::relative_path(&std::path::absolute(project_dir)?, &member.dir);
                (
                    version,
                    Some(environment),
                    DownloadSource::Workspace { path },
                    None,
                    "workspace".to_string(),
                    member.manifest.features.clone(),
                )
            }
            // a path or git package reads its edges from its own manifest, as a member does
            Request::Path { .. } | Request::Git { .. } => {
                let local = local.expect("a path or git request was read above");
                manifest_edges(
                    &local.manifest,
                    local.dir.as_deref(),
                    &alias_path,
                    &overrides,
                    excluded,
                    &mut pending_matched,
                    &mut pending_discovered,
                    &mut redirects,
                    &mut children,
                )?;
                let index_url = match local.source {
                    DownloadSource::Git { .. } => "git",
                    _ => "path",
                };
                // a git commit is its own integrity, and a folder has none
                (
                    local.version,
                    local.environment,
                    local.source,
                    None,
                    index_url.to_string(),
                    local.manifest.features,
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
            if let Some(existing) = resolved.get_mut(&(name.clone(), context)) {
                if takes(&req, &existing.install.version, true) {
                    let local = source.local_label().is_some().then_some(&source);
                    same_source(
                        &name,
                        context,
                        local,
                        &existing.install,
                        &existing.by,
                        &required_by,
                    )?;
                    /* a dev seed that a [dependencies] subtree already
                    brought in still gets its link */
                    if existing.install.link.is_none() {
                        existing.install.link = link;
                    }
                    // this tree already walked the package, only new features queue
                    existing.enable(&asked, defaults, &required_by)?;
                    existing.queue_edges(&mut queue, &mut edges);
                    continue;
                }
                if failing.is_none() {
                    failing.clone_from(&existing.optional);
                }
                return Err(Error::DependencyConflict {
                    name,
                    context,
                    first: existing.req.clone(),
                    first_by: existing.by.label(),
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
        let mut walked = Resolved {
            install: ResolvedInstall {
                name: name.clone(),
                version,
                environment,
                context,
                source,
                sha256,
                index_url,
                link,
                redirects,
                optional: false,
                features: Vec::new(),
            },
            req: req_text,
            by: required_by.clone(),
            optional,
            table,
            enabled: BTreeSet::new(),
            children,
            queued: BTreeMap::new(),
        };
        walked.enable(&asked, defaults, &required_by)?;
        walked.queue_edges(&mut queue, &mut edges);
        resolved.insert((name, context), walked);
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

    /* the parent's manifest names no package on such an edge, and the
    nested links and `embr tree` read that manifest. so the parent records
    the name in its redirects, as an [overrides] edge does. the walk queued
    the edge under no name, so the graph gets it too. a path or git entry
    is never optional */
    for (path, child) in unnamed {
        let (parent, alias) = path.split_at(path.len() - 1);
        for (key, walked) in &walked_at {
            if walked == parent
                && let Some(walked) = resolved.get_mut(key)
            {
                walked
                    .install
                    .redirects
                    .insert(alias[0].clone(), child.clone());
                edges
                    .entry(key.clone())
                    .or_default()
                    .push(((child.clone(), key.1), false));
            }
        }
    }

    let required = required_packages(&root_edges, &edges);
    Ok(resolved
        .into_iter()
        .map(|(key, walked)| {
            let mut install = walked.install;
            install.optional = !required.contains(&key);
            install.features = walked.enabled.into_iter().collect();
            install
        })
        .collect())
}

/// a package in one environment root, the key of the install set.
type Key = (String, Environment);

/// one entry of the install set while it resolves, see `resolved` in [`resolve_once`].
struct Resolved {
    install: ResolvedInstall,
    /// the requirement that picked the version, and who asked, for conflicts.
    req: String,
    by: Requirer,
    /// the optional edge that the package came under, None for a required one.
    optional: Option<OptionalEdge>,
    /// the package's [features] table.
    table: Features,
    /// its enabled features so far, the union over its dependents.
    enabled: BTreeSet<String>,
    /// every edge of the package, the gated ones too.
    children: Vec<Child>,
    /// the aliases queued so far, each with the features asked of it.
    queued: BTreeMap<String, BTreeSet<String>>,
}

impl Resolved {
    /** turns on the `asked` features, and the defaults when `defaults`.
    `by` asked, for the error when the package has no such feature. */
    fn enable(
        &mut self,
        asked: &BTreeSet<String>,
        defaults: bool,
        by: &Requirer,
    ) -> Result<(), Error> {
        let enabled = features::enable(&self.table, asked.iter().map(String::as_str), defaults)
            .map_err(|feature| {
                let known: Vec<&str> = self
                    .table
                    .keys()
                    .map(String::as_str)
                    .filter(|name| *name != features::DEFAULT)
                    .collect();
                Error::FeatureUnknown {
                    package: format!("{}@{}", self.install.name, self.install.version),
                    feature,
                    by: by.label(),
                    known: match known.as_slice() {
                        [] => "it has no features".to_string(),
                        names => format!("its features are {}", names.join(", ")),
                    },
                }
            })?;
        self.enabled.extend(enabled);
        Ok(())
    }

    /** queues each edge that the enabled features turn on, the first time
    it is on and again when it is asked for more features. a gated edge
    that no enabled feature lists stays out. a queued edge that is already
    resolved joins its features in the dedup gate. */
    fn queue_edges(
        &mut self,
        queue: &mut VecDeque<QueueEntry>,
        edges: &mut HashMap<Key, Vec<(Key, bool)>>,
    ) {
        let aliases: BTreeSet<&str> = self.children.iter().map(|c| c.alias.as_str()).collect();
        let is_alias = |alias: &str| aliases.contains(alias);
        let gated = features::gated(&self.table, &is_alias);
        let (on, asked) = features::turned_on(&self.table, &self.enabled, &is_alias);
        let key = (self.install.name.clone(), self.install.context);
        let id = format!("{}@{}", self.install.name, self.install.version);
        for child in &self.children {
            if gated.contains(&child.alias) && !on.contains(&child.alias) {
                continue;
            }
            let mut wants = child.ask.features.clone();
            wants.extend(asked.get(&child.alias).into_iter().flatten().cloned());
            let queued = self.queued.get(&child.alias);
            if queued.is_some_and(|queued| wants.is_subset(queued)) {
                continue;
            }
            if queued.is_none() {
                let edge = ((child.name.clone(), key.1), child.optional);
                edges.entry(key.clone()).or_default().push(edge);
            }
            self.queued
                .entry(child.alias.clone())
                .or_default()
                .extend(wants.iter().cloned());
            queue.push_back(QueueEntry {
                optional: match child.optional {
                    true => Some((child.path.clone(), child.name.clone())),
                    false => self.optional.clone(),
                },
                name: child.name.clone(),
                ask: Ask {
                    features: wants,
                    ..child.ask.clone()
                },
                link: None,
                alias_path: child.path.clone(),
                context: Some(key.1),
                required_by: Requirer::Package {
                    id: id.clone(),
                    alias: child.alias.clone(),
                },
            });
        }
    }
}

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
    project_dir: &Path,
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
        Override::Specifier(dependency) => (path.join("."), &**dependency),
    };
    Ok((
        dependency_name(dependency).to_lowercase(),
        request_for(&label, dependency, manifest, Some(project_dir))?,
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
    dependency.package()
}

/** `newest`, or the version that `keep` locks for `name` when `req` still
takes it. a locked prerelease counts as `newest` does: only while no
stable version is open. a locked version that the index no longer gives
for this target falls back to `newest`. */
fn keep_locked(
    index: &Index,
    name: &str,
    req: &semver::VersionReq,
    preferred: Option<(Environment, Language)>,
    pins: &[semver::Version],
    keep: &Pinned,
    newest: ResolvedPackage,
) -> ResolvedPackage {
    let no_stable = !newest.version.pre.is_empty();
    let Some(locked) = keep
        .get(name)
        .into_iter()
        .flatten()
        .filter(|version| takes(req, version, no_stable))
        .max()
    else {
        return newest;
    };
    if *locked == newest.version {
        return newest;
    }
    let exact = semver::VersionReq {
        comparators: vec![semver::Comparator {
            op: semver::Op::Exact,
            major: locked.major,
            minor: Some(locked.minor),
            patch: Some(locked.patch),
            pre: locked.pre.clone(),
        }],
    };
    index
        .resolve(name, &exact, preferred, pins)
        .unwrap_or(newest)
}

/** Queue entry for a dependency of `owner`. Index keys resolve against the
owner's [indices], so a member's deps use the member's. `alias` names the
entry in errors about it. */
fn request_for(
    alias: &str,
    dependency: &Dependency,
    owner: &Manifest,
    owner_dir: Option<&Path>,
) -> Result<Ask, Error> {
    let request = match dependency {
        Dependency::Registry { version, index, .. } => Request::Registry {
            req_text: version.clone(),
            index_url: owner.index_url(index.as_deref())?.to_string(),
        },
        Dependency::Workspace { .. } => Request::Workspace,
        // a path is relative to the manifest that names it
        Dependency::Path { path, .. } => {
            let owner_dir = owner_dir.ok_or_else(|| Error::GitPathDependency(alias.to_string()))?;
            Request::Path {
                dir: workspace::lexical(&std::path::absolute(owner_dir.join(path))?),
            }
        }
        Dependency::Git {
            git,
            rev,
            tag,
            branch,
            ..
        } => Request::Git {
            url: git.clone(),
            reference: Reference::of(rev.as_deref(), tag.as_deref(), branch.as_deref()),
        },
    };
    Ok(Ask {
        request,
        target: dependency_target(alias, dependency)?,
        features: dependency.features().iter().cloned().collect(),
        defaults: dependency.default_features(),
    })
}

/** queues the edges of a package that embr reads from its own manifest: a
workspace member, a path folder or a git commit. its registry entries
resolve against its own [indices] and its paths against `owner_dir`, but
the project's [overrides] still apply to the edges. */
#[allow(clippy::too_many_arguments)]
fn manifest_edges(
    owner: &Manifest,
    owner_dir: Option<&Path>,
    alias_path: &[String],
    overrides: &HashMap<Vec<String>, (String, Ask)>,
    excluded: &HashSet<Vec<String>>,
    matched: &mut Vec<Vec<String>>,
    discovered: &mut Vec<(Vec<String>, String)>,
    redirects: &mut BTreeMap<String, String>,
    children: &mut Vec<Child>,
) -> Result<(), Error> {
    for (alias, dependency) in &owner.dependencies {
        let mut child_path = alias_path.to_vec();
        child_path.push(alias.clone());
        if dependency.optional() && excluded.contains(&child_path) {
            continue;
        }

        let (child_name, child_ask) = match overrides.get(&child_path) {
            Some((child_name, child_ask)) => {
                matched.push(child_path.clone());
                redirects.insert(alias.clone(), child_name.clone());
                (child_name.clone(), child_ask.clone())
            }
            None => (
                dependency_name(dependency).to_lowercase(),
                request_for(alias, dependency, owner, owner_dir)?,
            ),
        };
        discovered.push((child_path.clone(), child_name.clone()));
        children.push(Child {
            name: child_name,
            ask: child_ask,
            path: child_path,
            alias: alias.clone(),
            optional: dependency.optional(),
        });
    }
    Ok(())
}

/** the commits that ember.lock holds for git dependencies, by URL and the
`branch` or `tag` asked for. a branch or tag resolves to its pin while the
pin is there, so it moves only with `relock`, which `--refresh` sets.
`found` keeps what each URL and reference gave, so one resolve fetches a
repository once, however many packages name it. */
struct GitPins {
    locked: HashMap<(String, Option<String>, Option<String>), String>,
    relock: bool,
    found: std::cell::RefCell<HashMap<(String, Reference), (String, String)>>,
}

impl GitPins {
    /// the pins of the lock in `project_dir`, without those of the packages that `update` names.
    fn read(project_dir: &Path, relock: bool, update: &[String]) -> Self {
        let lock = std::fs::read_to_string(lockfile_in(project_dir))
            .ok()
            .and_then(|text| toml::from_str::<Lockfile>(&text).ok());
        let locked = lock
            .into_iter()
            .flat_map(|lock| lock.packages)
            .filter(|package| {
                !update
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&package.name))
            })
            .filter_map(|package| match package.source {
                DownloadSource::Git {
                    url,
                    rev,
                    branch,
                    tag,
                } => Some(((url, branch, tag), rev)),
                _ => None,
            })
            .collect();
        GitPins {
            locked,
            relock,
            found: Default::default(),
        }
    }

    /// the commit and the ember.toml text of `url` at `reference`, see [`git::locate`].
    fn locate(&self, url: &str, reference: &Reference) -> Result<(String, String), Error> {
        let key = (url.to_string(), reference.clone());
        if let Some(found) = self.found.borrow().get(&key) {
            return Ok(found.clone());
        }
        let found = git::locate(url, reference, self.get(url, reference), self.relock)?;
        self.found.borrow_mut().insert(key, found.clone());
        Ok(found)
    }

    /// the locked commit of `url` at `reference`. a `rev` is its own pin.
    fn get(&self, url: &str, reference: &Reference) -> Option<&str> {
        if matches!(reference, Reference::Rev(_)) {
            return None;
        }
        let (branch, tag) = reference.locked();
        self.locked
            .get(&(url.to_string(), branch, tag))
            .map(String::as_str)
    }
}

/// a path or git package, read before its walk, see [`locate`].
struct Local {
    name: String,
    /// its [package] version. a folder without one reads as 0.0.0.
    version: semver::Version,
    environment: Option<Environment>,
    source: DownloadSource,
    manifest: Manifest,
    /// the folder that its own path dependencies resolve against. None for git.
    dir: Option<PathBuf>,
}

/** reads the manifest of a path folder or of a git commit. `named` is the
entry's own `name`, lowercase, or empty. the package's [package] name wins,
and the two must agree when both are there. */
fn locate(
    request: &Request,
    named: &str,
    alias: &str,
    project_dir: &Path,
    pins: &GitPins,
) -> Result<Local, Error> {
    let (manifest, source, dir, at) = match request {
        Request::Path { dir } => {
            let file = manifest_in(dir);
            let path = workspace::relative_path(&std::path::absolute(project_dir)?, dir);
            if !file.exists() {
                return Err(Error::PathDependencyMissing {
                    alias: alias.to_string(),
                    path,
                });
            }
            let at = format!("{path}/{}", crate::project::manifest::MANIFEST_FILE);
            (
                Manifest::load_from(&file)?,
                DownloadSource::Path { path },
                Some(dir.clone()),
                at,
            )
        }
        Request::Git { url, reference } => {
            let (rev, text) = pins.locate(url, reference)?;
            let (branch, tag) = reference.locked();
            let at = format!("{url} at {}", rev.get(..7).unwrap_or(&rev));
            let source = DownloadSource::Git {
                url: url.clone(),
                rev,
                branch,
                tag,
            };
            (Manifest::parse(&text)?, source, None, at)
        }
        Request::Registry { .. } | Request::Workspace => {
            unreachable!("only path and git requests are read here")
        }
    };
    let name = match manifest.name().map(str::to_lowercase) {
        Some(own) if named.is_empty() || named == own => own,
        Some(own) => {
            return Err(Error::LocalNameMismatch {
                alias: alias.to_string(),
                named: named.to_string(),
                at,
                own,
            });
        }
        None if !named.is_empty() => named.to_string(),
        None => {
            return Err(Error::LocalNameMissing {
                alias: alias.to_string(),
                at,
            });
        }
    };
    let version = match manifest.package.as_ref() {
        Some(package) => semver::Version::parse(&package.version)?,
        None => semver::Version::new(0, 0, 0),
    };
    let environment = manifest.target().map(|target| target.environment);
    // a folder links in place, so nothing can read its environment off an archive
    if environment.is_none() && dir.is_some() {
        return Err(Error::UnknownPackageEnvironment(name));
    }
    Ok(Local {
        name,
        version,
        environment,
        source,
        manifest,
        dir,
    })
}

/** a path or git package that meets a copy already in its environment
root: the same source is the same copy, another one is a conflict, since
the root keeps one copy of a package. `local` is the new source, None for a
registry or workspace request, which takes the copy that is there. */
fn same_source(
    name: &str,
    context: Environment,
    local: Option<&DownloadSource>,
    existing: &ResolvedInstall,
    first_by: &Requirer,
    required_by: &Requirer,
) -> Result<(), Error> {
    let Some(local) = local.filter(|local| **local != existing.source) else {
        return Ok(());
    };
    Err(Error::SourceConflict {
        name: name.to_string(),
        context,
        first: existing
            .source
            .local_label()
            .unwrap_or_else(|| existing.index_url.clone()),
        first_by: first_by.label(),
        second: local.local_label().unwrap_or_default(),
        second_by: required_by.label(),
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
            features: Default::default(),
            source: DownloadSource::TarGz { url: String::new() },
            sha256: None,
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
            features: Default::default(),
            source: DownloadSource::TarGz { url: String::new() },
            sha256: None,
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
            features: Default::default(),
            source: DownloadSource::TarGz { url: String::new() },
            sha256: None,
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
            "This Alloy project for roblox takes Alloy or Luau packages for roblox or portable; acme/fs@1.0.0 is a Luau package for lune. Set `target` on the dependency to take it on purpose"
        );
        assert_eq!(
            server
                .refusal("acme/x@1.0.0".to_string(), Loom, Some(Environment::Roblox))
                .to_string(),
            "This Luau project for roblox_server takes Luau packages for roblox_server, roblox, or portable; acme/x@1.0.0 is a Loom package for roblox"
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

    /** a plain resolve keeps each locked version that the requirements
    take. a requirement past it, `--update` and `--refresh` move it, and a
    kept version that a later edge refuses gives way to the newest. */
    #[test]
    fn a_resolve_keeps_the_locked_versions() {
        let fuel = |version: &str| ember_entry("acme/fuel", version, "roblox", "");
        let needs_fuel = |name: &str, req: &str| {
            ember_entry(
                name,
                "1.0.0",
                "roblox",
                &format!("[.dependencies.fuel]\nname = \"acme/fuel\"\nversion = \"{req}\"\n"),
            )
        };
        let origin = PesdeOrigin::new(
            "keep-locked",
            &[
                ("acme/fuel", format!("{}{}", fuel("0.2.0"), fuel("0.2.1"))),
                ("acme/rocket", needs_fuel("acme/rocket", "^0.2")),
                ("acme/jet", needs_fuel("acme/jet", "^0.2.1")),
            ],
        );
        let pinned: Pinned =
            [("acme/fuel".to_string(), vec![semver::Version::new(0, 2, 0)])].into();
        let fuel_of = |tables: &str, refresh: Refresh, update: &[String]| {
            let manifest: Manifest = toml::from_str(&format!(
                "[target]\nenvironment = \"roblox\"\n\n[indices]\ndefault = \"{}\"\n\n{tables}",
                origin.url
            ))
            .unwrap();
            let installs = resolve_updating(
                &manifest,
                &origin.dirs[0],
                refresh,
                &pinned,
                None,
                update,
                &mut Vec::new(),
            )
            .unwrap();
            let fuel = installs
                .iter()
                .find(|install| install.name == "acme/fuel")
                .unwrap();
            fuel.version.to_string()
        };
        let direct = |req: &str| {
            format!("[dependencies]\nfuel = {{ name = \"acme/fuel\", version = \"{req}\" }}\n")
        };

        assert_eq!(fuel_of(&direct("^0.2"), Refresh::Never, &[]), "0.2.0");
        assert_eq!(fuel_of(&direct("^0.2.1"), Refresh::Never, &[]), "0.2.1");
        assert_eq!(
            fuel_of(&direct("^0.2"), Refresh::Never, &["ACME/fuel".to_string()]),
            "0.2.1"
        );
        assert_eq!(fuel_of(&direct("^0.2"), Refresh::Force, &[]), "0.2.1");
        // a transitive keeps its version too
        let rocket = "[dependencies]\nrocket = { name = \"acme/rocket\", version = \"^1\" }\n";
        assert_eq!(fuel_of(rocket, Refresh::Never, &[]), "0.2.0");
        /* `early` keeps 0.2.0, and `late` needs ^0.2.1. the conflict drops
        the lock version, and the newest takes both edges */
        let both = "[dependencies]\nearly = { name = \"acme/rocket\", version = \"^1\" }\n\
                    late = { name = \"acme/jet\", version = \"^1\" }\n";
        assert_eq!(fuel_of(both, Refresh::Never, &[]), "0.2.1");
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

    /// `name [features]` for each install, and the features in brackets only when some are on.
    fn with_features(installs: &[ResolvedInstall]) -> Vec<String> {
        installs
            .iter()
            .map(|install| match install.features.as_slice() {
                [] => install.name.clone(),
                features => format!("{} [{}]", install.name, features.join(", ")),
            })
            .collect()
    }

    /// `name source link` for each install, the source as `local_label` writes it.
    fn sources(installs: &[ResolvedInstall]) -> Vec<String> {
        installs
            .iter()
            .map(|install| {
                format!(
                    "{} {} {}",
                    install.name,
                    install.source.local_label().unwrap_or_default(),
                    install.link.as_deref().unwrap_or("-")
                )
            })
            .collect()
    }

    /// the index for the feature tests: acme/ui gates acme/icons behind `pretty`, its default.
    fn feature_origin(name: &str) -> PesdeOrigin {
        let needs_ui = |name: &str, features: &str| {
            ember_entry(
                name,
                "1.0.0",
                "roblox",
                &format!(
                    "[.dependencies.ui]\nname = \"acme/ui\"\nversion = \"^1.0.0\"\n\
                     features = [{features}]\ndefault-features = false\n"
                ),
            )
        };
        PesdeOrigin::new(
            name,
            &[
                (
                    "acme/ui",
                    ember_entry(
                        "acme/ui",
                        "1.0.0",
                        "roblox",
                        "[.dependencies.core]\nname = \"acme/core\"\nversion = \"^1.0.0\"\n\
                         [.dependencies.theme]\nname = \"acme/theme\"\nversion = \"^1.0.0\"\n\
                         [.feature-dependencies.icons]\nname = \"acme/icons\"\nversion = \"^1.0.0\"\n\
                         [.features]\ndefault = [\"pretty\"]\npretty = [\"icons\"]\n\
                         dark = [\"theme/dark\"]\nfast = []\n",
                    ),
                ),
                ("acme/core", ember_entry("acme/core", "1.0.0", "roblox", "")),
                (
                    "acme/icons",
                    ember_entry("acme/icons", "1.0.0", "roblox", ""),
                ),
                (
                    "acme/theme",
                    ember_entry("acme/theme", "1.0.0", "roblox", "[.features]\ndark = []\n"),
                ),
                ("acme/a", needs_ui("acme/a", "\"dark\"")),
                ("acme/b", needs_ui("acme/b", "\"fast\"")),
                (
                    "acme/wide",
                    ember_entry(
                        "acme/wide",
                        "1.0.0",
                        "roblox",
                        "[.feature-dependencies.ghost]\nname = \"acme/ghost\"\nversion = \"^1.0.0\"\noptional = true\n\
                         [.features]\nextra = [\"ghost\"]\n",
                    ),
                ),
            ],
        )
    }

    /** the defaults are on unless a dependent turns them off, and a gated
    dependency installs only when an enabled feature lists it. a feature
    can turn on a feature of a dependency. */
    #[test]
    fn features_gate_dependencies_and_the_defaults_can_be_off() {
        let origin = feature_origin("features-defaults");

        let (installs, _) =
            origin.resolve("[dependencies]\nui = { name = \"acme/ui\", version = \"^1\" }\n");
        assert_eq!(
            with_features(&installs.unwrap()),
            ["acme/core", "acme/icons", "acme/theme", "acme/ui [pretty]"]
        );

        let (installs, _) = origin.resolve(
            "[dependencies]\nui = { name = \"acme/ui\", version = \"^1\", default-features = false }\n",
        );
        assert_eq!(
            with_features(&installs.unwrap()),
            ["acme/core", "acme/theme", "acme/ui"]
        );

        let (installs, _) = origin.resolve(
            "[dependencies]\nui = { name = \"acme/ui\", version = \"^1\", features = [\"dark\"], default-features = false }\n",
        );
        assert_eq!(
            with_features(&installs.unwrap()),
            ["acme/core", "acme/theme [dark]", "acme/ui [dark]"]
        );
    }

    /** the enabled features of a package are the union over its dependents
    in one environment root. a dependent that the walk meets after the
    package resolved still adds its features, and what they turn on. */
    #[test]
    fn features_are_the_union_over_every_dependent() {
        let origin = feature_origin("features-union");
        let (installs, _) = origin.resolve(
            "[dependencies]\na = { name = \"acme/a\", version = \"^1\" }\n\
             b = { name = \"acme/b\", version = \"^1\" }\n",
        );
        assert_eq!(
            with_features(&installs.unwrap()),
            [
                "acme/a",
                "acme/b",
                "acme/core",
                "acme/theme [dark]",
                "acme/ui [dark, fast]"
            ]
        );

        // the project keeps the defaults on, so acme/icons comes back
        let (installs, _) = origin.resolve(
            "[dependencies]\na = { name = \"acme/a\", version = \"^1\" }\n\
             ui = { name = \"acme/ui\", version = \"^1\" }\n",
        );
        assert_eq!(
            with_features(&installs.unwrap()),
            [
                "acme/a",
                "acme/core",
                "acme/icons",
                "acme/theme [dark]",
                "acme/ui [dark, pretty]"
            ]
        );
    }

    /** a feature that the package does not have fails the resolve and
    names the features it has. a gated dependency that is also optional
    skips with one line when its feature is on and it does not resolve. */
    #[test]
    fn an_unknown_feature_fails_and_an_optional_gated_dependency_skips() {
        let origin = feature_origin("features-unknown");
        let (installs, _) = origin.resolve(
            "[dependencies]\nui = { name = \"acme/ui\", version = \"^1\", features = [\"icns\"] }\n",
        );
        assert_eq!(
            installs.unwrap_err().to_string(),
            "acme/ui@1.0.0 has no feature 'icns', which ember.toml (as 'ui') asks for; its features are dark, fast, pretty"
        );

        let (installs, warnings) =
            origin.resolve("[dependencies]\nwide = { name = \"acme/wide\", version = \"^1\" }\n");
        assert_eq!(with_features(&installs.unwrap()), ["acme/wide"]);
        assert!(warnings.is_empty(), "{warnings:?}");

        let (installs, warnings) = origin.resolve(
            "[dependencies]\nwide = { name = \"acme/wide\", version = \"^1\", features = [\"extra\"] }\n",
        );
        assert_eq!(with_features(&installs.unwrap()), ["acme/wide [extra]"]);
        assert!(
            warnings[0].starts_with("skipped optional dependency 'wide.ghost' (acme/ghost)"),
            "{warnings:?}"
        );
    }

    fn resolve_in(
        project: &Path,
        tables: &str,
        refresh: Refresh,
    ) -> Result<Vec<ResolvedInstall>, Error> {
        let manifest: Manifest =
            toml::from_str(&format!("[target]\nenvironment = \"roblox\"\n\n{tables}")).unwrap();
        resolve(
            &manifest,
            project,
            refresh,
            &Pinned::new(),
            None,
            &mut Vec::new(),
        )
    }

    /** a path dependency reads the folder's ember.toml, and its own path
    dependencies resolve against that folder. an edit there shows on the
    next resolve. the entry's `name` must agree with the folder's. */
    #[test]
    fn a_path_dependency_reads_its_folder() {
        let base = std::env::temp_dir().join("embr-test-resolver-path");
        let _ = fs::remove_dir_all(&base);
        let project = base.join("game");
        fs::create_dir_all(&project).unwrap();
        let package = |name: &str, extra: &str| {
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.2.0\"\n\n\
                 [target]\nenvironment = \"roblox\"\n\n{extra}"
            )
        };
        write(
            &base.join("libs/lib"),
            "ember.toml",
            &package(
                "acme/lib",
                "[dependencies]\nutil = { path = \"../util\" }\n",
            ),
        );
        write(
            &base.join("libs/util"),
            "ember.toml",
            &package("acme/util", ""),
        );
        write(
            &base.join("libs/other"),
            "ember.toml",
            &package("acme/other", ""),
        );

        let tables = "[dependencies]\nlib = { path = \"../libs/./lib\" }\n";
        let installs = resolve_in(&project, tables, Refresh::Never).unwrap();
        assert_eq!(
            sources(&installs),
            [
                "acme/lib path ../libs/lib lib",
                "acme/util path ../libs/util -"
            ]
        );
        assert_eq!(installs[0].index_url, "path");
        assert_eq!(installs[0].environment, Some(Environment::Roblox));
        // the folder's edge names no package, so lib records the name it read
        assert_eq!(installs[0].redirects["util"], "acme/util");
        assert!(!installs[1].optional);

        // the folder is the source, so its edit shows at once
        write(
            &base.join("libs/lib"),
            "ember.toml",
            &package(
                "acme/lib",
                "[dependencies]\nother = { path = \"../other\" }\n",
            ),
        );
        let installs = resolve_in(&project, tables, Refresh::Never).unwrap();
        assert_eq!(
            sources(&installs),
            [
                "acme/lib path ../libs/lib lib",
                "acme/other path ../libs/other -"
            ]
        );

        let error = resolve_in(
            &project,
            "[dependencies]\nlib = { path = \"../libs/lib\", name = \"acme/else\" }\n",
            Refresh::Never,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Dependency 'lib' names acme/else, but ../libs/lib/ember.toml is acme/lib"
        );
        let error = resolve_in(
            &project,
            "[dependencies]\nlib = { path = \"../none\" }\n",
            Refresh::Never,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Dependency 'lib' points at ../none, which has no ember.toml"
        );
        // one folder spelled two ways is one source, two folders of one package are two
        let one = "[dependencies]\nlib = { path = \"../libs/lib\" }\n\
                   util = { path = \"../libs/other/../util\" }\n";
        assert!(resolve_in(&project, one, Refresh::Never).is_ok());
        write(
            &base.join("libs/copy"),
            "ember.toml",
            &package("acme/other", ""),
        );
        let two = "[dependencies]\nlib = { path = \"../libs/lib\" }\n\
                   copy = { path = \"../libs/copy\" }\n";
        let error = resolve_in(&project, two, Refresh::Never).unwrap_err();
        assert_eq!(
            error.to_string(),
            "acme/other comes from two sources in the roblox tree: path ../libs/copy (from ember.toml (as 'copy')) and path ../libs/other (from acme/lib@0.2.0 (as 'other')); use one of them"
        );

        // the folder's own [features] gate its edges, and its defaults are on
        let gated = |default: &str| {
            package(
                "acme/lib",
                &format!(
                    "[dependencies]\nutil = {{ path = \"../util\" }}\n\n\
                     [features]\ndefault = [{default}]\npretty = [\"util\"]\n"
                ),
            )
        };
        write(&base.join("libs/lib"), "ember.toml", &gated("\"pretty\""));
        let installs = resolve_in(&project, tables, Refresh::Never).unwrap();
        assert_eq!(installs[0].features, ["pretty"]);
        assert_eq!(installs[1].name, "acme/util");
        write(&base.join("libs/lib"), "ember.toml", &gated(""));
        let installs = resolve_in(&project, tables, Refresh::Never).unwrap();
        assert_eq!(sources(&installs), ["acme/lib path ../libs/lib lib"]);

        // the entry picks the folder's features, as on a registry entry
        let picked = "[dependencies]
lib = { path = \"../libs/lib\", features = [\"pretty\"] }\n";
        let installs = resolve_in(&project, picked, Refresh::Never).unwrap();
        assert_eq!(installs[0].features, ["pretty"]);
        assert_eq!(installs[1].name, "acme/util");
        write(&base.join("libs/lib"), "ember.toml", &gated("\"pretty\""));
        let off = "[dependencies]
lib = { path = \"../libs/lib\", default-features = false }\n";
        let installs = resolve_in(&project, off, Refresh::Never).unwrap();
        assert_eq!(sources(&installs), ["acme/lib path ../libs/lib lib"]);
        let _ = fs::remove_dir_all(&base);
    }

    /** a git dependency resolves to a commit. ember.lock pins a branch, so
    a new commit there does not move it until a refresh. a `rev` stays where
    it points. */
    #[test]
    fn a_git_dependency_locks_its_commit() {
        let remote = crate::registry::index::git::fixture::Remote::new("resolver");
        let manifest = |version: &str| {
            format!(
                "[package]\nname = \"acme/gitlib\"\nversion = \"{version}\"\n\n\
                 [target]\nenvironment = \"roblox\"\n"
            )
        };
        let first = remote.commit(&[("ember.toml", &manifest("1.0.0"))]);
        let project = std::env::temp_dir().join("embr-test-resolver-git");
        let _ = fs::remove_dir_all(&project);
        fs::create_dir_all(&project).unwrap();

        let branch = format!(
            "[dependencies]\nlib = {{ git = \"{}\", branch = \"main\" }}\n",
            remote.url
        );
        let installs = resolve_in(&project, &branch, Refresh::Ttl).unwrap();
        assert_eq!(installs[0].name, "acme/gitlib");
        assert_eq!(installs[0].version.to_string(), "1.0.0");
        assert_eq!(installs[0].index_url, "git");
        assert_eq!(
            installs[0].source,
            DownloadSource::Git {
                url: remote.url.clone(),
                rev: first.clone(),
                branch: Some("main".to_string()),
                tag: None,
            }
        );

        // the lock that install would write pins the branch
        let lock = Lockfile::new(vec![crate::project::lockfile::LockedPackage {
            name: "acme/gitlib".to_string(),
            version: "1.0.0".to_string(),
            environment: Environment::Roblox,
            context: None,
            link: Some("lib".to_string()),
            index: "git".to_string(),
            redirects: BTreeMap::new(),
            patch: None,
            optional: false,
            features: Vec::new(),
            source: installs[0].source.clone(),
            sha256: None,
        }]);
        fs::write(project.join("ember.lock"), toml::to_string(&lock).unwrap()).unwrap();

        let second = remote.commit(&[("ember.toml", &manifest("1.1.0"))]);
        let installs = resolve_in(&project, &branch, Refresh::Ttl).unwrap();
        assert!(
            matches!(&installs[0].source, DownloadSource::Git { rev, .. } if *rev == first),
            "{:?}",
            installs[0].source
        );
        // `--update` with its name moves it, and so does `--refresh`
        let manifest: Manifest =
            toml::from_str(&format!("[target]\nenvironment = \"roblox\"\n\n{branch}")).unwrap();
        let update = ["ACME/gitlib".to_string()];
        let installs = resolve_updating(
            &manifest,
            &project,
            Refresh::Ttl,
            &Pinned::new(),
            None,
            &update,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(matches!(&installs[0].source, DownloadSource::Git { rev, .. } if *rev == second));
        let installs = resolve_in(&project, &branch, Refresh::Force).unwrap();
        assert!(matches!(&installs[0].source, DownloadSource::Git { rev, .. } if *rev == second));
        assert_eq!(installs[0].version.to_string(), "1.1.0");

        // a rev is its own pin, whatever the branch does
        let pinned = format!(
            "[dependencies]\nlib = {{ git = \"{}\", rev = \"{}\" }}\n",
            remote.url,
            &first[..10]
        );
        let installs = resolve_in(&project, &pinned, Refresh::Force).unwrap();
        assert!(
            matches!(&installs[0].source, DownloadSource::Git { rev, branch: None, .. } if *rev == first)
        );

        // the entry picks the commit's features, as on a registry entry
        let featured = remote.commit(&[(
            "ember.toml",
            "[package]\nname = \"acme/gitlib\"\nversion = \"1.2.0\"\n\n\
             [target]\nenvironment = \"roblox\"\n\n[features]\nfast = []\n",
        )]);
        let picked = format!(
            "[dependencies]\nlib = {{ git = \"{}\", rev = \"{featured}\", features = [\"fast\"] }}\n",
            remote.url
        );
        let installs = resolve_in(&project, &picked, Refresh::Force).unwrap();
        assert_eq!(installs[0].features, ["fast"]);
        let _ = fs::remove_dir_all(&project);
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
