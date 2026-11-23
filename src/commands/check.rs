/*! `embr check`: whether ember.toml is valid and ember.lock is its lock.
it reads the two files, installs nothing, and reaches no network, so a CI
job can run it before the install. */

use crate::cli::Outcome;
use crate::error::Error;
use crate::project::lockfile::{Lockfile, lockfile_path};
use crate::project::manifest::Manifest;
use crate::project::workspace::{self, Workspace};
use crate::ui;
use clap::Args;
use std::path::Path;

#[derive(Args, Debug)]
pub struct CheckArgs {
    /// Print the result as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

/** why ember.lock in the current folder is not the lock of `manifest`,
None when it is. the rule is the one of `install --locked`, and a lock
with no record of what ember.toml asked for fails here, because nothing
shows that it matches. */
fn mismatch(manifest: &Manifest) -> Result<Option<&'static str>, Error> {
    if !lockfile_path().exists() {
        return Ok(Some("ember.lock is missing"));
    }
    // a lock that does not parse is a failure of its own, with its own text
    let lock = Lockfile::load()?;
    Ok(match &lock.manifest_hash {
        _ if lock.version < 2 => Some("ember.lock is from before version 2 of the format"),
        None => Some("ember.lock does not record what ember.toml asked for"),
        Some(hash) if *hash != manifest.dependency_hash() => {
            Some("ember.toml asks for other dependencies than the ones ember.lock was written for")
        }
        Some(_) => None,
    })
}

/** exit 0 when each ember.toml loads and each ember.lock matches it, for
the project and for each member of its workspace. an ember.toml that does
not load is the error of any command, exit 1. a lock that does not match
is exit 3. */
pub fn run(args: CheckArgs) -> Result<Outcome, Error> {
    let manifest = Manifest::load()?;
    let mut problems = Vec::new();
    if let Some(reason) = mismatch(&manifest)? {
        problems.push(reason.to_string());
    }
    if !manifest.workspace_members().is_empty() {
        let workspace = Workspace::open(Path::new("."))?;
        for member in &workspace.members {
            if member.dir == workspace.root {
                continue;
            }
            let reason = workspace::in_dir(&member.dir, || mismatch(&Manifest::load()?))?;
            if let Some(reason) = reason {
                problems.push(format!("{}: {reason}", member.label()));
            }
        }
    }

    if args.json {
        ui::print_json(&serde_json::json!({
            "version": ui::JSON_VERSION,
            "ok": problems.is_empty(),
            "problems": problems,
        }))?;
        return Ok(if problems.is_empty() {
            Outcome::Quiet
        } else {
            Outcome::Exit(3)
        });
    }
    if problems.is_empty() {
        ui::print_success("ember.toml is valid, and ember.lock matches it");
        return Ok(Outcome::Done);
    }
    Err(Error::LockMismatch(format!(
        "{}; run `embr install` and commit ember.lock",
        problems.join("\n")
    )))
}
