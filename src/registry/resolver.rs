use crate::error::Error;
use crate::project::manifest::{
    Dependency, Environment, Manifest, parse_version_req, split_package_name,
};
use crate::project::workspace::{self, Workspace};
use crate::registry::index::{DownloadSource, Index};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

/// A package ready to download: the flattened result of resolution.
pub struct ResolvedInstall {
    pub name: String,
    pub version: semver::Version,
    /// None means the environment must be detected from the extracted files.
    pub environment: Option<Environment>,
    pub source: DownloadSource,
    pub index_url: String,
    /// Name of the generated link file: the [dependencies] alias for direct
    /// dependencies, the package's short name for transitive ones.
    pub link: String,
}

/// Where a queued dependency comes from.
enum Request {
    /// Resolve in a git index.
    Registry { req_text: String, index_url: String },
    /// Resolve to a member of this project's workspace. Like pesde, any
    /// `version` on the specifier is ignored locally — the member's current
    /// version is what you get (it only matters when publishing).
    Workspace,
}

/// Resolves the manifest's dependency graph breadth-first. Transitive
/// dependencies (including cross-manager ones, e.g. a pesde package pulling a
/// wally package) all flatten into one install set, deduped by package name;
/// a requirement that rejects the already-chosen version is a hard error.
/// Workspace dependencies resolve to sibling projects on disk and carry their
/// own dependencies into the same set.
pub fn resolve(
    manifest: &Manifest,
    project_dir: &Path,
    refresh: bool,
) -> Result<Vec<ResolvedInstall>, Error> {
    let prefer_environment = manifest.target.as_ref().map(|target| target.environment);
    let mut indices: HashMap<String, Index> = HashMap::new();
    // The workspace is only discovered (walking up for a claiming root) when
    // a workspace dependency actually appears.
    let mut workspace_memo: Option<Option<Workspace>> = None;

    let mut queue: VecDeque<(String, Request, Option<String>)> = VecDeque::new();

    // Seeding all direct dependencies before any transitive one is discovered
    // matters: the first entry per name wins, so a package that also shows up
    // transitively still links under its manifest alias.
    for (alias, dependency) in &manifest.dependencies {
        queue.push_back((
            dependency_name(dependency).to_lowercase(),
            request_for(dependency, manifest)?,
            Some(alias.clone()),
        ));
    }

    // name -> (what we resolved, the requirement that won). A BTreeMap keeps
    // the install set (and the lockfile written from it) in name order.
    let mut resolved: BTreeMap<String, (ResolvedInstall, String)> = BTreeMap::new();

    while let Some((name, request, link)) = queue.pop_front() {
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

        let install = match request {
            Request::Registry { index_url, .. } => {
                let index = open_index(&mut indices, &index_url, refresh)?;
                let package = index.resolve(&name, &req, prefer_environment)?;

                for dependency in &package.dependencies {
                    queue.push_back((
                        dependency.name.clone(),
                        Request::Registry {
                            req_text: dependency.version_req.clone(),
                            index_url: dependency
                                .index_url
                                .clone()
                                .unwrap_or_else(|| index_url.clone()),
                        },
                        None,
                    ));
                }

                ResolvedInstall {
                    name: name.clone(),
                    version: package.version,
                    environment: package.environment,
                    source: package.source,
                    index_url,
                    link: String::new(),
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

                // The member's own dependencies install for the consumer too;
                // its registry deps resolve against the member's [indices].
                for dependency in member.manifest.dependencies.values() {
                    queue.push_back((
                        dependency_name(dependency).to_lowercase(),
                        request_for(dependency, &member.manifest)?,
                        None,
                    ));
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
                }
            }
        };

        let link = link.unwrap_or_else(|| {
            split_package_name(&name)
                .map(|(_, short)| short.to_string())
                .unwrap_or_else(|_| name.replace('/', "_"))
        });
        resolved.insert(name, (ResolvedInstall { link, ..install }, req_text));
    }

    Ok(resolved.into_values().map(|(install, _)| install).collect())
}

fn dependency_name(dependency: &Dependency) -> &str {
    match dependency {
        Dependency::Registry { name, .. } => name,
        Dependency::Workspace { workspace, .. } => workspace,
    }
}

/// Builds the queue entry for a dependency of `owner` (index keys resolve
/// against the owner's [indices], so a member's deps use the member's).
fn request_for(dependency: &Dependency, owner: &Manifest) -> Result<Request, Error> {
    Ok(match dependency {
        Dependency::Registry { version, index, .. } => Request::Registry {
            req_text: version.clone(),
            index_url: owner.index_url(index.as_deref())?.to_string(),
        },
        Dependency::Workspace { .. } => Request::Workspace,
    })
}

/// The workspace this project resolves members from: itself when it declares
/// members, otherwise the nearest ancestor that claims it. Memoized — glob
/// walks and manifest reads shouldn't repeat per dependency.
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
    refresh: bool,
) -> Result<&'a Index, Error> {
    if !indices.contains_key(url) {
        indices.insert(url.to_string(), Index::open(url, refresh)?);
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
             [target]\nenvironment = \"shared\"\nworkspace_members = [\"packages/*\"]\n",
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

        // Resolving from a member: its workspace dep and that dep's own
        // workspace dep all land in the install set, linked in place.
        let member_dir = base.join("packages/extra");
        let manifest = Manifest::load_from(&member_dir.join("lpm.toml")).unwrap();
        let installs = resolve(&manifest, &member_dir, false).unwrap();

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

        // Resolving from the root: members resolve through the root's own
        // member list (no ancestor needed), and transitive workspace deps of
        // members come along.
        let root_manifest_text = "[package]\nname = \"acme/root\"\nversion = \"0.0.0\"\nprivate = true\n\n\
             [target]\nenvironment = \"shared\"\nworkspace_members = [\"packages/*\"]\n\n\
             [dependencies]\nextra = { workspace = \"acme/extra\" }\n";
        write(&base, "lpm.toml", root_manifest_text);
        let manifest = Manifest::load_from(&base.join("lpm.toml")).unwrap();
        let installs = resolve(&manifest, &base, false).unwrap();
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
            resolve(&manifest, &base, false),
            Err(Error::NotInWorkspace(_))
        ));

        let _ = fs::remove_dir_all(&base);
    }
}
