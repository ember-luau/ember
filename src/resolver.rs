use crate::error::Error;
use crate::index::{DownloadSource, Index};
use crate::manifest::{Environment, Manifest, parse_version_req, split_package_name};
use std::collections::{BTreeMap, HashMap, VecDeque};

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

/// Resolves the manifest's dependency graph breadth-first. Transitive
/// dependencies (including cross-manager ones, e.g. a pesde package pulling a
/// wally package) all flatten into one install set, deduped by package name;
/// a requirement that rejects the already-chosen version is a hard error.
pub fn resolve(manifest: &Manifest, refresh: bool) -> Result<Vec<ResolvedInstall>, Error> {
    let prefer_environment = manifest.target.as_ref().map(|target| target.environment);
    let mut indices: HashMap<String, Index> = HashMap::new();

    // (package name, version requirement, index url, link name override)
    let mut queue: VecDeque<(String, String, String, Option<String>)> = VecDeque::new();

    // Seeding all direct dependencies before any transitive one is discovered
    // matters: the first entry per name wins, so a package that also shows up
    // transitively still links under its manifest alias.
    for (alias, dependency) in &manifest.dependencies {
        let url = manifest.index_url(dependency.index.as_deref())?;
        queue.push_back((
            dependency.name.to_lowercase(),
            dependency.version.clone(),
            url.to_string(),
            Some(alias.clone()),
        ));
    }

    // name -> (what we resolved, the requirement that won). A BTreeMap keeps
    // the install set (and the lockfile written from it) in name order.
    let mut resolved: BTreeMap<String, (ResolvedInstall, String)> = BTreeMap::new();

    while let Some((name, req_text, index_url, link)) = queue.pop_front() {
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

        let index = open_index(&mut indices, &index_url, refresh)?;
        let package = index.resolve(&name, &req, prefer_environment)?;

        for dependency in &package.dependencies {
            queue.push_back((
                dependency.name.clone(),
                dependency.version_req.clone(),
                dependency
                    .index_url
                    .clone()
                    .unwrap_or_else(|| index_url.clone()),
                None,
            ));
        }

        let link = link.unwrap_or_else(|| {
            split_package_name(&name)
                .map(|(_, short)| short.to_string())
                .unwrap_or_else(|_| name.replace('/', "_"))
        });
        resolved.insert(
            name.clone(),
            (
                ResolvedInstall {
                    name,
                    version: package.version,
                    environment: package.environment,
                    source: package.source,
                    index_url,
                    link,
                },
                req_text,
            ),
        );
    }

    Ok(resolved.into_values().map(|(install, _)| install).collect())
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
