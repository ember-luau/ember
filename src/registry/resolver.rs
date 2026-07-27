use crate::error::Error;
use crate::project::manifest::{
    Dependency, Environment, Manifest, Override, override_paths, parse_version_req,
    split_package_name,
};
use crate::project::workspace::{self, Workspace};
use crate::registry::index::{DownloadSource, Index, Refresh};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;

/// A package ready to download: the flattened result of resolution.
#[derive(Debug)]
pub struct ResolvedInstall {
    pub name: String,
    pub version: semver::Version,
    /// None means the environment must be detected from the extracted files.
    pub environment: Option<Environment>,
    pub source: DownloadSource,
    pub index_url: String,
    /** Name of the generated link file: the [dependencies] alias for direct
    deps, the package's short name for transitive ones. */
    pub link: String,
    /** Edges of THIS package that [overrides] rewrote: its declared alias ->
    the replacement's package name. The nested-link pass reads a package's
    declared dependencies back off disk by name, so a name-changing override
    must travel to the linker (and the lockfile) or the dependent would link
    the original — or nothing. */
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
    the specifier is ignored locally; you get the member's current version
    (the req only matters when publishing). */
    Workspace,
}

/** Resolves the manifest's dependency graph breadth-first. Transitive deps
(cross-manager ones too, e.g. a pesde package pulling a wally one) flatten
into one install set, deduped by package name; a requirement that rejects the
already-chosen version is a hard error. Workspace deps resolve to sibling
projects on disk and bring their own deps into the same set. */
pub fn resolve(
    manifest: &Manifest,
    project_dir: &Path,
    refresh: Refresh,
    warnings: &mut Vec<String>,
) -> Result<Vec<ResolvedInstall>, Error> {
    let mut ttl_skipped = false;
    match resolve_once(manifest, project_dir, refresh, &mut ttl_skipped, warnings) {
        /* an index whose pull was skipped by the TTL can be stale, and most
        resolver errors don't say which index they came from — so any
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
    alias path: a dangling alias, unknown index key, or two keys covering
    the same path all fail before any network happens, whether or not the
    path ends up matching an edge. Each edge the walk discovers carries its
    alias path from the root ("foo" -> "foo.bar" -> ...); an exact match
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
    /* enough breadcrumbs to say WHY an override never fired: every queued
    edge path and its package, and the one path each package was walked
    under (children are only enumerated on first discovery) */
    let mut discovered: HashMap<Vec<String>, String> = HashMap::new();
    let mut walked_at: HashMap<String, Vec<String>> = HashMap::new();

    let mut queue: VecDeque<(String, Request, Option<String>, Vec<String>)> = VecDeque::new();

    /* Seed all direct deps before any transitive one is discovered: first
    entry per name wins, so a package that also shows up transitively
    still links under its manifest alias. */
    for (alias, dependency) in &manifest.dependencies {
        let name = dependency_name(dependency).to_lowercase();
        discovered.insert(vec![alias.clone()], name.clone());
        queue.push_back((
            name,
            request_for(dependency, manifest)?,
            Some(alias.clone()),
            vec![alias.clone()],
        ));
    }

    /* name -> (what we resolved, the req that won). BTreeMap keeps the
    install set, and the lockfile written from it, in name order. */
    let mut resolved: BTreeMap<String, (ResolvedInstall, String)> = BTreeMap::new();

    while let Some((name, request, link, alias_path)) = queue.pop_front() {
        let req_text = match &request {
            Request::Registry { req_text, .. } => req_text.clone(),
            Request::Workspace => "*".to_string(),
        };
        let req = parse_version_req(&req_text)?;

        if let Some((existing, first_req)) = resolved.get(&name) {
            if req.matches(&existing.version) {
                continue;
            }
            return Err(Error::DependencyConflict {
                name,
                first: first_req.clone(),
                second: req_text,
            });
        }

        walked_at.entry(name.clone()).or_insert(alias_path.clone());
        let mut redirects: BTreeMap<String, String> = BTreeMap::new();

        let install = match request {
            Request::Registry { index_url, .. } => {
                let index = open_index(&mut indices, &index_url, refresh, ttl_skipped)?;
                let package = index.resolve(&name, &req, prefer_environment)?;

                for dependency in &package.dependencies {
                    let mut child_path = alias_path.clone();
                    child_path.push(dependency.alias.clone());

                    let (child_name, child_request) = match overrides.get(&child_path) {
                        Some((child_name, child_request)) => {
                            overrides_matched.insert(child_path.clone());
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
                    discovered.insert(child_path.clone(), child_name.clone());
                    queue.push_back((child_name, child_request, None, child_path));
                }

                ResolvedInstall {
                    name: name.clone(),
                    version: package.version,
                    environment: package.environment,
                    source: package.source,
                    index_url,
                    link: String::new(),
                    redirects: BTreeMap::new(),
                }
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

                /* The member's own deps install for the consumer too; its
                registry deps resolve against the member's [indices], but
                the consumer's [overrides] still apply to the edges. */
                for (alias, dependency) in &member.manifest.dependencies {
                    let mut child_path = alias_path.clone();
                    child_path.push(alias.clone());

                    let (child_name, child_request) = match overrides.get(&child_path) {
                        Some((child_name, child_request)) => {
                            overrides_matched.insert(child_path.clone());
                            redirects.insert(alias.clone(), child_name.clone());
                            (child_name.clone(), child_request.clone())
                        }
                        None => (
                            dependency_name(dependency).to_lowercase(),
                            request_for(dependency, &member.manifest)?,
                        ),
                    };
                    discovered.insert(child_path.clone(), child_name.clone());
                    queue.push_back((child_name, child_request, None, child_path));
                }

                let path =
                    workspace::relative_path(&std::path::absolute(project_dir)?, &member.dir);
                ResolvedInstall {
                    name: name.clone(),
                    version,
                    environment: Some(environment),
                    source: DownloadSource::Workspace { path },
                    index_url: "workspace".to_string(),
                    link: String::new(),
                    redirects: BTreeMap::new(),
                }
            }
        };

        let link = link.unwrap_or_else(|| {
            split_package_name(&name)
                .map(|(_, short)| short.to_string())
                .unwrap_or_else(|_| name.replace('/', "_"))
        });
        resolved.insert(
            name,
            (
                ResolvedInstall {
                    link,
                    redirects,
                    ..install
                },
                req_text,
            ),
        );
    }

    /* an override that never met its edge would otherwise be silently
    dead. two distinct reasons deserve distinct messages: a path that
    exists but wasn't walked (its package's edges were enumerated under an
    earlier discovery path — root aliases seed alphabetically), and a path
    that simply never appeared (a typo, or a dependency that moved on). */
    let mut unmatched: Vec<&Vec<String>> = overrides
        .keys()
        .filter(|path| !overrides_matched.contains(*path))
        .collect();
    unmatched.sort();
    for path in unmatched {
        let parent = &path[..path.len() - 1];
        let elsewhere = discovered
            .get(parent)
            .and_then(|parent_name| walked_at.get(parent_name))
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

/** what an overridden edge asks for instead: the root manifest's own
dependency when the override is an alias, or the inline specifier. Either
way index keys resolve against the ROOT manifest's [indices] — the author
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

/** The workspace this project resolves members from: itself when it declares
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

/// Opens each index at most once per run (so each gets refreshed once).
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

        /* Resolving from a member: its workspace dep and that dep's own
        workspace dep all land in the install set, linked in place. */
        let member_dir = base.join("packages/extra");
        let manifest = Manifest::load_from(&member_dir.join("lpm.toml")).unwrap();
        let installs = resolve(&manifest, &member_dir, Refresh::Never, &mut Vec::new()).unwrap();

        assert_eq!(installs.len(), 1);
        let core = &installs[0];
        assert_eq!(core.name, "acme/core");
        assert_eq!(core.version, semver::Version::new(1, 2, 0));
        assert_eq!(core.environment, Some(Environment::Shared));
        assert_eq!(core.link, "core");
        assert!(matches!(
            &core.source,
            DownloadSource::Workspace { path } if path == "../core"
        ));

        /* Resolving from the root: members resolve through the root's own
        member list (no ancestor needed), and members' transitive
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

    /** [overrides] end to end, against a real local git index: a specifier
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
                .map(|install| format!("{}@{} as {}", install.name, install.version, install.link))
                .collect()
        };

        // no overrides: foo brings its declared bar
        let plain = resolve_with(
            "[dependencies]\nfoo = { name = \"acme/foo\", version = \"^1\" }\n",
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            summary(&plain),
            ["acme/bar@1.0.0 as bar", "acme/foo@1.0.0 as foo"]
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
            ["acme/foo@1.0.0 as foo", "acme/qux@1.0.0 as qux"]
        );
        let foo = swapped
            .iter()
            .find(|install| install.name == "acme/foo")
            .unwrap();
        assert_eq!(
            foo.redirects.get("bar").map(String::as_str),
            Some("acme/qux")
        );

        /* an alias override defers to the root's own [dependencies] entry:
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
        package can't apply — edges are walked once, under the first
        discovery path — and the warning says that, not "typo" */
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
            ["acme/bar@1.0.0 as bar", "acme/foo@1.0.0 as aaa"]
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

        /* eager validation: a dangling alias, a pathless key, interior
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
