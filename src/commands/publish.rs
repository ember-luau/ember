use crate::error::Error;
use crate::project::manifest::{Environment, Manifest, split_package_name};
use crate::registry::pack;
use crate::ui;
use clap::Args;
use std::path::Path;

// TODO(api): everything past packing was removed. The old flow uploaded the
// tarball as a GitHub release asset on the package's own repo, then forked the
// index repo, wrote the entry at <scope>/<name> and opened a PR (that entry
// generation lived in publish/index_entry.rs, now deleted). What has to come
// back against the lpm API:
//   1. authenticate the user (auth.rs still has the GitHub device flow and the
//      ~/.lpm/credentials.toml store if the API can reuse a GitHub token)
//   2. upload `archive` under `asset_name`
//   3. tell the API which environment/version/dependencies the entry carries
//      (all of it is on `manifest`)
//   4. report where the package landed instead of the PR url
// Until then `run` packs, validates, and stops at PublishUnavailable.

#[derive(Args, Debug)]
pub struct PublishArgs {
    /// Show what would be published without uploading anything
    #[arg(long)]
    pub dry_run: bool,
    /// Index key from [indices] to publish to (default index otherwise)
    #[arg(short, long)]
    pub index: Option<String>,
}

pub fn run(args: PublishArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    let environment = publish_environment(&manifest)?;
    let (scope, name) = split_package_name(&manifest.package.name)?;
    let version = semver::Version::parse(&manifest.package.version)?;

    // Packing is unchanged: whatever the API ends up accepting, this is the
    // archive it gets.
    let root = Path::new(".");
    let files = pack::packed_files(root, &manifest)?;
    let archive = ui::with_spinner("Packing package", || pack::pack(root, &manifest))?;

    let asset_name = format!("{scope}_{name}-{version}.tar.gz");

    if args.dry_run {
        println!(
            "Would publish {}@{version} ({environment}) as:",
            manifest.package.name
        );
        println!("  {asset_name} ({} bytes)", archive.len());
        for file in &files {
            println!("  {}", file.display());
        }
        if let Some(index) = &args.index {
            println!("Target index: {index}");
        }
        return Ok(());
    }

    // TODO(api): upload `archive` here.
    Err(Error::PublishUnavailable)
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
