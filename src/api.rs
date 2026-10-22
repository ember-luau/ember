/*! The calls other tools make. Each one works on the current directory,
like the embr command of the same name, and prints what that command
prints. */

use crate::commands::{add, install, publish};
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
