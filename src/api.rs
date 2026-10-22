/*! The calls other tools make. Each one works on the current directory,
like the embr command of the same name, and prints what that command
prints. */

use crate::cli::{Cli, Commands};
use crate::commands::{add, install, publish};
use crate::project::lockfile::Lockfile;
use crate::project::manifest::Manifest;
use crate::registry::index::DownloadSource;
use clap::{CommandFactory, FromArgMatches};
use std::ffi::OsString;
use std::path::PathBuf;

pub use crate::cli::Outcome;
pub use crate::error::Error;
pub use crate::project::manifest::{Environment, Language};
pub use crate::registry::resolver::Allow;
pub use crate::ui::{Palette, set_palette};

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
        prompt: false,
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

/// a tool that runs embr's commands under its own name, as `alloy yank`.
pub struct Host {
    /// the program name in usage lines and help: `alloy`
    pub name: &'static str,
    /// what add and install take. None takes everything, as embr does
    pub allow: Option<Allow>,
    /// the compiler publish builds with, see [`publish`]
    pub compiler: Option<PathBuf>,
}

/** runs one embr command from `args`, the words after the host's own name,
such as `["yank", "acme/thing@1.0.0"]`. help and usage errors print in the
palette and end the process, as they do in embr. the host prints an error
the command returns, and decides what follows each [`Outcome`]. call
[`set_palette`] first to paint the output in the host's colours. */
pub fn run(host: &Host, args: impl IntoIterator<Item = OsString>) -> Result<Outcome, Error> {
    let Some(mut command) = parse(host.name, args).unwrap_or_else(|error| error.exit()) else {
        // hosts route by command name, so a bare call means nothing to run
        return Ok(Outcome::Done);
    };
    prepare(host, &mut command);
    crate::cli::dispatch(command)
}

/// embr's command line, named for the host so usage reads `alloy yank ...`.
fn parse(
    name: &'static str,
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<Commands>, clap::Error> {
    let words = std::iter::once(OsString::from(name)).chain(args);
    let matches = Cli::command()
        .name(name)
        .bin_name(name)
        .try_get_matches_from(words)?;
    Ok(Cli::from_arg_matches(&matches)?.command)
}

/// what a host adds: its allow-list, add without the index prompt, and its compiler.
fn prepare(host: &Host, command: &mut Commands) {
    match command {
        Commands::Add(args) => {
            args.allow = host.allow;
            args.prompt = false;
        }
        Commands::Install(args) => args.allow = host.allow,
        Commands::Publish(args) => args.compiler = host.compiler.clone(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host {
            name: "alloy",
            allow: Some(Allow {
                host: "alloy",
                summary: "Alloy packages",
                languages: &[Language::Alloy],
                environments: &[],
            }),
            compiler: Some(PathBuf::from("/bin/alloy")),
        }
    }

    fn command(words: &[&str]) -> Commands {
        let host = host();
        let mut command = parse(host.name, words.iter().map(OsString::from))
            .unwrap()
            .unwrap();
        prepare(&host, &mut command);
        command
    }

    #[test]
    fn a_host_runs_embr_commands_under_its_own_rules() {
        let Commands::Add(add) = command(&["add", "chief/core"]) else {
            panic!("add parses as add");
        };
        assert!(add.allow.is_some(), "add takes the host's allow-list");
        assert!(!add.prompt, "a host never waits on the index prompt");

        let Commands::Install(install) = command(&["install"]) else {
            panic!("install parses as install");
        };
        assert!(install.allow.is_some());

        let Commands::Publish(publish) = command(&["publish", "--dry-run"]) else {
            panic!("publish parses as publish");
        };
        assert!(publish.dry_run);
        assert_eq!(publish.compiler, Some(PathBuf::from("/bin/alloy")));

        // scripts, yanks, and the rest pass through as embr parses them
        assert!(matches!(command(&["run", "lint"]), Commands::Run(_)));
        assert!(matches!(
            command(&["yank", "a/b@1.0.0", "--undo"]),
            Commands::Yank(_)
        ));
    }

    #[test]
    fn usage_errors_name_the_host() {
        let error = parse("alloy", ["yank".into()]).unwrap_err().to_string();
        assert!(error.contains("alloy yank"), "{error}");
    }
}
