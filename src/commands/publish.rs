use crate::error::Error;
use crate::net::{auth, registry};
use crate::project::manifest::{
    DEFAULT_INDEX_URL, Environment, Manifest, is_github_username, split_package_name,
};
use crate::registry::index::Index;
use crate::registry::pack;
use crate::ui;
use clap::Args;
use std::path::Path;

#[derive(Args, Debug)]
pub struct PublishArgs {
    /// Show what would be published without uploading anything
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: PublishArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    let environment = publish_environment(&manifest)?;
    let (scope, name) = split_package_name(&manifest.package.name)?;
    let version = semver::Version::parse(&manifest.package.version)?;

    // The API appends [package] authors to the scope's owner list (each one
    // can then publish to the whole scope) and 400s anything that isn't
    // shaped like a GitHub username; catch that before packing. It does NOT
    // check the account exists, so a typo silently grants a stranger-to-be.
    if let Some(author) = manifest
        .package
        .authors
        .iter()
        .find(|author| !is_github_username(author))
    {
        return Err(Error::ManifestInvalid(format!(
            "author '{author}' is not a GitHub username; [package] authors grant publish access to your scope and must be GitHub usernames (no emails or display names)"
        )));
    }

    let root = Path::new(".");
    let files = pack::packed_files(root, &manifest)?;
    let archive = ui::with_spinner("Packing package", || pack::pack(root, &manifest))?;

    // The API answers 413 past its cap; failing here saves the upload (and
    // makes --dry-run catch it too).
    if archive.len() > registry::MAX_ARCHIVE_BYTES {
        return Err(Error::PublishTooLarge {
            size_mb: archive.len() as f64 / (1024.0 * 1024.0),
            limit_mb: (registry::MAX_ARCHIVE_BYTES / (1024 * 1024)) as u64,
        });
    }

    if args.dry_run {
        println!(
            "Would publish {}@{version} ({environment}) to {}:",
            manifest.package.name,
            registry::API_URL,
        );
        println!(
            "  {scope}/{name}/{version}.tar.gz ({} bytes)",
            archive.len()
        );
        for file in &files {
            println!("  {}", file.display());
        }
        return Ok(());
    }

    // Stored credentials first; the device flow (and the index clone that
    // provides its client id) only when there's nothing stored yet.
    let credentials = match auth::load()? {
        Some(credentials) => credentials,
        None => auth::login(&oauth_client_id()?)?,
    };

    match upload(&credentials.token, &archive) {
        // A 401 means the stored token was revoked or expired, not that this
        // publish is doomed: forget it, log in fresh, and try once more.
        Err(Error::PublishFailed { status: 401, .. }) => {
            auth::clear()?;
            eprintln!("warning: the registry rejected the stored GitHub token; logging in again");
            let credentials = auth::login(&oauth_client_id()?)?;
            upload(&credentials.token, &archive)?;
        }
        other => other?,
    }

    ui::print_success(&format!(
        "Published {}@{version} ({environment})",
        manifest.package.name
    ));
    println!("Install it with `lpm add {}`", manifest.package.name);
    Ok(())
}

fn upload(token: &str, archive: &[u8]) -> Result<(), Error> {
    ui::with_spinner("Uploading package", || registry::publish(token, archive))
}

/// OAuth app client id for the device flow. It lives in the lpm index's
/// config.toml (not in the binary) so it can rotate without a CLI release.
fn oauth_client_id() -> Result<String, Error> {
    let index = Index::open(DEFAULT_INDEX_URL, true)?;
    index
        .github_oauth_id()
        .map(str::to_string)
        .ok_or_else(|| Error::PublishNotSupported(DEFAULT_INDEX_URL.to_string()))
}

/// Published packages must say where their code runs; the entry is keyed by it.
fn publish_environment(manifest: &Manifest) -> Result<Environment, Error> {
    manifest
        .target
        .as_ref()
        .map(|target| target.environment)
        .ok_or_else(|| {
            Error::ManifestInvalid(
                "publishing requires a [target] section with an environment".to_string(),
            )
        })
}
