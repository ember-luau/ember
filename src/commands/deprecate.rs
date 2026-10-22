use crate::commands::yank::as_owner;
use crate::error::Error;
use crate::net::registry;
use crate::project::manifest::{Environment, split_package_name};
use crate::ui;
use clap::Args;

/// The registry's cap on a deprecation reason.
const MAX_REASON_CHARS: usize = 300;

#[derive(Args, Debug)]
pub struct DeprecateArgs {
    /// The package, as scope/name, or one version, as scope/name@version
    pub package: String,
    /// Why, and what to use instead. Needed unless --undo
    pub reason: Option<String>,
    /// Deprecate only the archive for this environment of the version
    #[arg(long, value_name = "ENV")]
    pub environment: Option<String>,
    /// Remove the deprecation
    #[arg(long)]
    pub undo: bool,
}

/** marks a package, or one version of it, as deprecated, pesde's way. it
still installs, and every resolve that picks it prints the reason. */
pub fn run(args: DeprecateArgs) -> Result<(), Error> {
    let (name, version) = match args.package.split_once('@') {
        Some((name, version)) => (name, Some(semver::Version::parse(version)?)),
        None => (args.package.as_str(), None),
    };
    split_package_name(name)?;
    let environment = args
        .environment
        .as_deref()
        .map(Environment::from_embr)
        .transpose()?;
    if environment.is_some() && version.is_none() {
        return Err(Error::DeprecateArgs(
            "--environment narrows one version; write the package as scope/name@version",
        ));
    }
    let reason = match (args.undo, args.reason.as_deref().map(str::trim)) {
        (true, _) => None,
        (false, Some(reason)) if !reason.is_empty() => {
            if reason.chars().count() > MAX_REASON_CHARS {
                return Err(Error::DeprecateArgs(
                    "the reason is longer than the registry's 300 characters",
                ));
            }
            Some(reason)
        }
        (false, _) => {
            return Err(Error::DeprecateArgs(
                "say why, and what to use instead: embr deprecate scope/name \"<reason>\"",
            ));
        }
    };

    as_owner(|token| {
        ui::with_spinner("Contacting the registry", || {
            registry::deprecate(token, name, version.as_ref(), environment, reason)
        })
    })?;

    let what = match (&version, environment) {
        (Some(version), Some(environment)) => format!("{name}@{version} ({environment})"),
        (Some(version), None) => format!("{name}@{version}"),
        (None, _) => name.to_string(),
    };
    if args.undo {
        ui::print_success(&format!("{what} is no longer deprecated"));
    } else {
        ui::print_success(&format!("Deprecated {what}"));
        println!("It still installs. Each install that resolves it prints the reason.");
    }
    Ok(())
}
