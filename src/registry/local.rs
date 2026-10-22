/*! the local registry in ~/.ember/local. `embr publish --local` writes a
package here, and the index named `local` reads it, with no login and no
network. its index has the format of ember's own index, so the pesde reader
resolves it with no change. the archives sit beside the index. */

use crate::error::Error;
use crate::project::manifest::{Dependency, Language, Manifest, split_package_name};
use crate::sys::paths;
use std::fs;
use std::path::{Component, Path, PathBuf};

/** the name of the index. a dependency, the lockfile and `default-index`
keep this name in place of a URL, so a lockfile does not hold a home
directory. */
pub const NAME: &str = "local";

/// how the `download` of an entry names a file in the archives folder.
const SCHEME: &str = "local:";

/// the folder of index files, `<scope>/<name>` like a git index.
pub fn index_dir() -> Result<PathBuf, Error> {
    Ok(paths::local_dir()?.join("index"))
}

/** stores `archive`, the packed target that `manifest` holds, and writes
its index entry. a version that is already here gets the new archive: a
local package changes while its author works on it. */
pub fn publish(manifest: &Manifest, archive: &[u8]) -> Result<(), Error> {
    publish_in(&paths::local_dir()?, manifest, archive)
}

fn publish_in(root: &Path, manifest: &Manifest, archive: &[u8]) -> Result<(), Error> {
    let package = manifest.package.as_ref().ok_or(Error::PackageMissing)?;
    let target = manifest.target().expect("publish sets one target");
    let full_name = package.name.to_lowercase();
    let (scope, name) = split_package_name(&full_name)?;
    let id = target.id();

    let file = format!("{scope}/{name}/{}/{id}.tar.gz", package.version);
    let archive_path = root.join("archives").join(&file);
    fs::create_dir_all(archive_path.parent().expect("the path has folders"))?;
    fs::write(&archive_path, archive)?;

    let mut entry = toml::Table::new();
    entry.insert("name".into(), full_name.clone().into());
    entry.insert("version".into(), package.version.clone().into());
    entry.insert("target".into(), target.environment.as_str().into());
    if target.language != Language::Luau {
        entry.insert("language".into(), target.language.as_str().into());
    }
    if let Some(description) = &package.description {
        entry.insert("description".into(), description.clone().into());
    }
    entry.insert("download".into(), format!("{SCHEME}{file}").into());
    let dependencies = dependencies(manifest)?;
    if !dependencies.is_empty() {
        entry.insert("dependencies".into(), dependencies.into());
    }

    let index_path = root.join("index").join(scope).join(name);
    let mut entries: toml::Table = match fs::read_to_string(&index_path) {
        Ok(text) => toml::from_str(&text)?,
        Err(_) => toml::Table::new(),
    };
    entries.insert(format!("{} {id}", package.version), entry.into());
    fs::create_dir_all(index_path.parent().expect("the path has folders"))?;
    fs::write(&index_path, toml::to_string_pretty(&entries)?)?;
    Ok(())
}

/** the dependencies of an entry. an index key means nothing outside the
manifest that states it, so each one becomes its URL. a dependency on
the local index names no index, which the reader takes as this one. */
fn dependencies(manifest: &Manifest) -> Result<toml::Table, Error> {
    let mut table = toml::Table::new();
    for (alias, dependency) in &manifest.dependencies {
        // publish turned each workspace dependency into a registry one
        let Dependency::Registry {
            name,
            version,
            index,
            ..
        } = dependency
        else {
            continue;
        };
        let mut spec = toml::Table::new();
        spec.insert("name".into(), name.clone().into());
        spec.insert("version".into(), version.clone().into());
        let url = manifest.index_url(index.as_deref())?;
        if url != NAME {
            spec.insert("index".into(), url.into());
        }
        table.insert(alias.clone(), spec.into());
    }
    Ok(table)
}

/** the full name of the one local package called `short`, in any scope,
ignoring case. none, or one in several scopes, is an error that says
what to do. */
pub fn find(short: &str) -> Result<String, Error> {
    find_one_in(&index_dir()?, short)
}

fn find_one_in(index: &Path, short: &str) -> Result<String, Error> {
    let mut found = find_in(index, short);
    match found.len() {
        0 => Err(Error::LocalPackageMissing(short.to_string())),
        1 => Ok(found.remove(0)),
        _ => Err(Error::LocalPackageAmbiguous {
            name: short.to_string(),
            found,
        }),
    }
}

/// every match, sorted, so an error that lists them reads the same each time.
fn find_in(index: &Path, short: &str) -> Vec<String> {
    let short = short.trim().to_lowercase();
    let mut found: Vec<String> = fs::read_dir(index)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|scope| scope.path().join(&short).is_file())
        .map(|scope| format!("{}/{short}", scope.file_name().to_string_lossy()))
        .collect();
    found.sort();
    found
}

/** the bytes of the archive that a `local:` download names. None for any
other URL. the local file is read each time and never cached, because a
republish replaces it. */
pub fn read_archive(url: &str) -> Option<Result<Vec<u8>, Error>> {
    let file = url.strip_prefix(SCHEME)?;
    Some(read_in(
        &paths::local_dir().ok()?.join("archives"),
        url,
        file,
    ))
}

fn read_in(archives: &Path, url: &str, file: &str) -> Result<Vec<u8>, Error> {
    let fetch_error = |reason: &str| Error::IndexFetch {
        url: url.to_string(),
        reason: reason.to_string(),
    };
    // a lockfile is text a user can edit, so the path must stay in the folder
    if !Path::new(file)
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(fetch_error("the path leaves the local registry"));
    }
    fs::read(archives.join(file)).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => fetch_error(
            "the archive is not in the local registry; run `embr publish --local` in the package folder",
        ),
        _ => error.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::index::{DownloadSource, Index};

    fn manifest(text: &str) -> Manifest {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn a_local_publish_resolves_and_a_republish_replaces_it() {
        let root = std::env::temp_dir().join("embr-test-local-registry");
        let _ = fs::remove_dir_all(&root);

        let lib = manifest(
            "[package]\nname = \"me/Lib\"\nversion = \"1.0.0\"\n\n\
             [target]\nenvironment = \"roblox\"\nmain = \"src/init.luau\"\n\n\
             [indices]\nwally = \"https://github.com/UpliftGames/wally-index\"\n\n\
             [dependencies]\n\
             signal = { name = \"acme/signal\", version = \"^1\" }\n\
             util = { name = \"me/util\", version = \"^\", index = \"local\" }\n\
             promise = { name = \"evaera/promise\", version = \"^4\", index = \"wally\" }\n",
        );
        publish_in(&root, &lib, b"first").unwrap();

        // the folder holds the index and the archives, and no settings
        let mut top: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        top.sort();
        assert_eq!(top, ["archives", "index"]);

        let index = Index::local_at(root.join("index"));
        let any = semver::VersionReq::parse("*").unwrap();
        let package = index.resolve("me/lib", &any, None, &[]).unwrap();
        assert_eq!(package.version, semver::Version::new(1, 0, 0));
        let DownloadSource::TarGz { url } = &package.source else {
            panic!("a local package downloads as a tarball");
        };
        assert_eq!(url, "local:me/lib/1.0.0/roblox.tar.gz");
        let file = url.strip_prefix(SCHEME).unwrap();
        assert_eq!(
            read_in(&root.join("archives"), url, file).unwrap(),
            b"first"
        );

        /* ember's own index and wally's get their URL, and the local one
        none, which the reader takes as this index */
        let mut dependencies: Vec<_> = package
            .dependencies
            .iter()
            .map(|dep| (dep.name.as_str(), dep.index_url.as_deref()))
            .collect();
        dependencies.sort();
        assert_eq!(
            dependencies,
            [
                (
                    "acme/signal",
                    Some(crate::project::manifest::DEFAULT_INDEX_URL)
                ),
                (
                    "evaera/promise",
                    Some("https://github.com/UpliftGames/wally-index")
                ),
                ("me/util", None),
            ]
        );

        // the same version again is the edit loop: the new archive wins
        publish_in(&root, &lib, b"second").unwrap();
        assert_eq!(
            read_in(&root.join("archives"), url, file).unwrap(),
            b"second"
        );
        let (candidates, _) = index.read("me/lib").unwrap();
        assert_eq!(candidates.len(), 1);

        assert!(read_in(&root.join("archives"), "local:../x", "../x").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_a_package_by_its_name_in_every_scope() {
        let root = std::env::temp_dir().join("embr-test-local-find");
        let _ = fs::remove_dir_all(&root);
        let index = root.join("index");
        for scope in ["me", "you"] {
            fs::create_dir_all(index.join(scope)).unwrap();
            fs::write(index.join(scope).join("lib"), "").unwrap();
        }
        fs::write(index.join("me").join("other"), "").unwrap();

        assert_eq!(find_in(&index, "Lib"), ["me/lib", "you/lib"]);
        assert_eq!(find_in(&index, "other"), ["me/other"]);
        assert!(find_in(&index, "missing").is_empty());
        assert!(find_in(&root.join("nothing"), "lib").is_empty());

        // what `embr add` gets for one match, several, and none
        assert_eq!(find_one_in(&index, "other").unwrap(), "me/other");
        assert!(matches!(
            find_one_in(&index, "lib"),
            Err(Error::LocalPackageAmbiguous { found, .. }) if found == ["me/lib", "you/lib"]
        ));
        assert!(matches!(
            find_one_in(&index, "missing"),
            Err(Error::LocalPackageMissing(name)) if name == "missing"
        ));
        let _ = fs::remove_dir_all(&root);
    }
}
