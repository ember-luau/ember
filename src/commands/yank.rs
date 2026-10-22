use crate::error::Error;
use crate::net::{auth, registry};
use crate::project::manifest::{Environment, split_package_name};
use crate::ui;
use clap::Args;

#[derive(Args, Debug)]
pub struct YankArgs {
    /// The version to yank, as scope/name@version
    pub package: String,
    /// Yank only the archive for this environment, not the whole version
    #[arg(long, value_name = "ENV")]
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
    let environment = args
        .environment
        .as_deref()
        .map(Environment::from_embr)
        .transpose()?;

    let credentials = match auth::load()? {
        Some(credentials) => credentials,
        None => auth::login(&crate::commands::publish::oauth_client_id()?)?,
    };
    let send = |token: &str| {
        ui::with_spinner("Contacting the registry", || {
            registry::yank(token, name, &version, environment, args.undo)
        })
    };
    match send(&credentials.token) {
        // same as publish: a revoked token earns one fresh login
        Err(Error::YankFailed { status: 401, .. }) => {
            auth::clear()?;
            eprintln!("warning: the registry rejected the stored GitHub token; logging in again");
            let credentials = auth::login(&crate::commands::publish::oauth_client_id()?)?;
            send(&credentials.token)?;
        }
        other => other?,
    }

    let what = match environment {
        Some(environment) => format!("{name}@{version} ({environment})"),
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
