/*! The calls other tools make. Each one works on the current directory,
like the embr command of the same name, and prints what that command
prints. */

use crate::commands::{add, install, publish};
use crate::project::lockfile::Lockfile;
use crate::project::manifest::Manifest;
use crate::registry::index::DownloadSource;
use std::path::PathBuf;

pub use crate::error::Error;
pub use crate::project::manifest::{Environment, Language};
pub use crate::registry::resolver::Allow;

/** adds `package` (`scope/name`) to ember.toml, `embr add`. it does not
install; [`install`] does. `version` is a requirement such as `^1.2`, None
meaning the latest. `allow` refuses a package the host does not take. */
pub fn add(package: &str, version: Option<&str>, allow: Option<Allow>) -> Result<(), Error> {
    add::run(add::AddArgs {
        package: package.to_string(),
        version: version.map(str::to_string),
        index: None,
        alias: None,
        refresh: false,
        allow,
    })
}

/** installs what ember.toml asks for, `embr install`. `locked` installs
exactly what ember.lock records. `allow` refuses, on every fresh resolve,
a package the host does not take. */
pub fn install(locked: bool, allow: Option<Allow>) -> Result<(), Error> {
    install::run(install::InstallArgs {
        locked,
        refresh: false,
        allow,
    })
}

/** publishes the package, `embr publish`. `compiler` builds a package
whose language compiles to Luau, so the Alloy CLI passes its own path.
None looks for the compiler on PATH. */
pub fn publish(dry_run: bool, compiler: Option<PathBuf>) -> Result<(), Error> {
    publish::run(publish::PublishArgs { dry_run, compiler })
}

/// one package that ember.lock records, and the folder ember stored it in.
pub struct Installed {
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
}

/** every downloaded package that ember.lock records, with its folder: the
packages-out folder of its context, then `.ember/<scope>_<name>`. the
Alloy CLI rebuilds Alloy packages from their sources there. workspace
members are linked in place, so they are not in the list. */
pub fn installed() -> Result<Vec<Installed>, Error> {
    let manifest = Manifest::load()?;
    let lock = Lockfile::load()?;
    Ok(lock
        .packages
        .into_iter()
        .filter(|package| !matches!(package.source, DownloadSource::Workspace { .. }))
        .map(|package| Installed {
            dir: manifest
                .packages_out(package.context())
                .join(install::STORE_DIR)
                .join(package.name.replace('/', "_")),
            name: package.name,
            version: package.version,
        })
        .collect())
}
