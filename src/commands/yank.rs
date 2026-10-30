use crate::error::Error;
use crate::net::{auth, registry};
use crate::project::manifest::{Environment, split_package_name};
use crate::ui;
use clap::Args;

#[derive(Args, Debug)]
pub struct YankArgs {
    /// The version to yank, as scope/name@version
    pub package: String,
    /// Yank only this target, such as `roblox` or `roblox-alloy`, not the whole version
    #[arg(long, value_name = "ID")]
    pub target: Option<String>,
    /// The Luau target of this environment, the spelling before --target
    #[arg(long, value_name = "ENV", hide = true, conflicts_with = "target")]
    pub environment: Option<String>,
    /// Restore a yanked version
    #[arg(long)]
    pub undo: bool,
}

/** marks a published version so fresh installs skip it. the archive stays,
and a lockfile that pins the version keeps installing it, cargo's rule. */
pub fn run(args: YankArgs) -> Result<(), Error> {
    let (name, version) = args
        .package
        .split_once('@')
        .ok_or_else(|| Error::YankSpec(args.package.clone()))?;
    split_package_name(name)?;
    let version = semver::Version::parse(version)?;
    let target = target_id(args.target, args.environment.as_deref())?;

    as_owner(|token| {
        ui::with_spinner("Contacting the registry", || {
            registry::yank(token, name, &version, target.as_deref(), args.undo)
        })
    })?;

    let what = match &target {
        Some(target) => format!("{name}@{version} ({target})"),
        None => format!("{name}@{version}"),
    };
    if args.undo {
        ui::print_success(&format!("Restored {what}"));
    } else {
        ui::print_success(&format!("Yanked {what}"));
        println!("New installs skip it. Projects whose ember.lock pins it keep it.");
    }
    Ok(())
}

/** the target ID that `--target` or the hidden `--environment` names. an ID
goes to the registry as it is, to match an index key's target part exactly.
an environment means its Luau target, so `--environment shared` is `roblox`. */
pub(crate) fn target_id(
    target: Option<String>,
    environment: Option<&str>,
) -> Result<Option<String>, Error> {
    match (target, environment) {
        (Some(target), _) => Ok(Some(target)),
        (None, Some(environment)) => Ok(Some(Environment::from_embr(environment)?.to_string())),
        (None, None) => Ok(None),
    }
}

/** runs `send` with EMBER_TOKEN or the stored login, logging in first when
there is neither. a 401 means the token was revoked, not that the call is
doomed, so it earns one fresh login and one retry, as publish does. */
pub(crate) fn as_owner(send: impl Fn(&str) -> Result<(), Error>) -> Result<(), Error> {
    let token = match auth::token()? {
        Some(token) => token,
        None => auth::login()?.token,
    };
    match send(&token) {
        // CI has no one to approve a login
        Err(Error::OwnerActionFailed { status: 401, .. }) if auth::env_token().is_some() => {
            Err(Error::TokenRejected)
        }
        Err(Error::OwnerActionFailed { status: 401, .. }) => {
            auth::clear()?;
            eprintln!("warning: the registry rejected the stored token; logging in again");
            let credentials = auth::login()?;
            send(&credentials.token)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_come_from_an_id_or_an_environment() {
        let id = |target: Option<&str>, environment: Option<&str>| {
            target_id(target.map(str::to_string), environment)
        };
        assert_eq!(id(None, None).unwrap(), None);
        // an ID passes through exactly, old keys like `shared` included
        assert_eq!(
            id(Some("roblox-alloy"), None).unwrap().as_deref(),
            Some("roblox-alloy")
        );
        assert_eq!(id(Some("shared"), None).unwrap().as_deref(), Some("shared"));
        // an environment names its Luau target
        assert_eq!(id(None, Some("lune")).unwrap().as_deref(), Some("lune"));
        assert_eq!(id(None, Some("shared")).unwrap().as_deref(), Some("roblox"));
        assert!(id(None, Some("mars")).is_err());
    }

    #[test]
    fn yank_keeps_the_hidden_environment_flag() {
        use clap::Parser;
        let parse = |words: &[&str]| crate::cli::Cli::try_parse_from(words).map(|cli| cli.command);
        let Ok(Some(crate::cli::Commands::Yank(yank))) =
            parse(&["embr", "yank", "a/b@1.0.0", "--environment", "lune"])
        else {
            panic!("yank parses as yank");
        };
        assert_eq!(yank.environment.as_deref(), Some("lune"));
        // one or the other, never both
        assert!(
            parse(&[
                "embr",
                "yank",
                "a/b@1.0.0",
                "--environment",
                "lune",
                "--target",
                "lune"
            ])
            .is_err()
        );
    }
}
