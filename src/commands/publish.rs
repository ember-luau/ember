use crate::error::Error;
use crate::net::{auth, registry};
use crate::project::manifest::{
    DEFAULT_INDEX_NAME, DEFAULT_INDEX_URL, Dependency, Environment, Manifest, is_github_username,
    split_package_name, workspace_version_req,
};
use crate::project::workspace::{self, Workspace};
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

    // A plain project just publishes itself.
    if manifest.workspace_members().is_empty() {
        return publish_project(manifest, args.dry_run);
    }

    // A workspace root publishes the whole workspace: the root project
    // first, then every member — pesde's order. One package failing doesn't
    // stop the rest; the command fails at the end instead.
    let workspace = Workspace::open(Path::new("."))?;
    let mut failed = Vec::new();

    let root_name = manifest.package.name.clone();
    if let Err(error) = publish_project(manifest, args.dry_run) {
        ui::print_error(&format!("{root_name}: {error}"));
        failed.push(root_name);
    }

    for member in &workspace.members {
        if member.dir == workspace.root {
            continue;
        }
        let name = member.manifest.package.name.clone();
        let result = workspace::in_dir(&member.dir, || {
            publish_project(Manifest::load()?, args.dry_run)
        });
        if let Err(error) = result {
            ui::print_error(&format!("{name}: {error}"));
            failed.push(name);
        }
    }

    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::WorkspacePublishFailed(failed))
    }
}

/// Publishes the project in the current directory. Private packages and
/// main-less workspace roots say so and skip instead of failing — that is
/// how a container root stays unpublished while its members go out.
fn publish_project(mut manifest: Manifest, dry_run: bool) -> Result<(), Error> {
    if manifest.package.private {
        println!(
            "Skipping {}: package is private, refusing to publish",
            manifest.package.name
        );
        return Ok(());
    }
    // A workspace root needs no `main`; without one it is a pure container
    // and only its members go out.
    let has_main = manifest
        .target
        .as_ref()
        .is_some_and(|target| target.main.is_some());
    if !manifest.workspace_members().is_empty() && !has_main {
        println!(
            "Skipping {}: workspace root without a main",
            manifest.package.name
        );
        return Ok(());
    }

    let environment = publish_environment(&manifest)?;
    let (scope, name) = {
        let (scope, name) = split_package_name(&manifest.package.name)?;
        (scope.to_string(), name.to_string())
    };
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

    // Workspace dependencies become registry ones in the archive's manifest;
    // the on-disk lpm.toml is never touched.
    convert_workspace_dependencies(&mut manifest, Path::new("."))?;

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

    if dry_run {
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

/// Rewrites workspace dependencies into registry ones, pesde's conversion
/// exactly: the requirement is the specifier's version type applied to the
/// member's current on-disk version (`^` + `1.2.3` → `^1.2.3`; a full
/// requirement passes through), and the member's `default` index URL is
/// baked in when it points somewhere other than lpm's own registry.
fn convert_workspace_dependencies(
    manifest: &mut Manifest,
    project_dir: &Path,
) -> Result<(), Error> {
    if !manifest
        .dependencies
        .values()
        .any(|dependency| matches!(dependency, Dependency::Workspace { .. }))
    {
        return Ok(());
    }

    let workspace = if manifest.workspace_members().is_empty() {
        workspace::containing(project_dir)?
    } else {
        Some(Workspace::open(project_dir)?)
    };

    for (alias, dependency) in manifest.dependencies.iter_mut() {
        let Dependency::Workspace {
            workspace: name,
            version,
        } = dependency
        else {
            continue;
        };
        let workspace = workspace
            .as_ref()
            .ok_or_else(|| Error::NotInWorkspace(alias.clone()))?;
        let member = workspace
            .member(name)
            .ok_or_else(|| Error::NoWorkspaceMember(name.clone()))?;
        let member_version = semver::Version::parse(&member.manifest.package.version)?;

        *dependency = Dependency::Registry {
            name: name.clone(),
            version: workspace_version_req(version, &member_version)?,
            index: member
                .manifest
                .indices
                .get(DEFAULT_INDEX_NAME)
                .filter(|url| url.as_str() != DEFAULT_INDEX_URL)
                .cloned(),
        };
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn workspace_dependencies_convert_to_registry_specs() {
        let base = std::env::temp_dir().join("lpm-test-publish-convert");
        let _ = fs::remove_dir_all(&base);

        let write = |path: &str, contents: &str| {
            let path = base.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        };
        write(
            "lpm.toml",
            "[package]\nname = \"acme/root\"\nversion = \"0.0.0\"\nprivate = true\n\n\
             [target]\nenvironment = \"shared\"\nworkspace_members = [\"packages/*\"]\n",
        );
        write(
            "packages/core/lpm.toml",
            "[package]\nname = \"acme/core\"\nversion = \"1.2.3\"\n\n[target]\nenvironment = \"shared\"\n",
        );
        write(
            "packages/extra/lpm.toml",
            "[package]\nname = \"acme/extra\"\nversion = \"0.2.0\"\n\n[target]\nenvironment = \"shared\"\n\n\
             [dependencies]\ncore = { workspace = \"acme/core\", version = \"~\" }\n",
        );

        let member_dir = base.join("packages/extra");
        let mut manifest = Manifest::load_from(&member_dir.join("lpm.toml")).unwrap();
        convert_workspace_dependencies(&mut manifest, &member_dir).unwrap();

        // "~" + the member's on-disk 1.2.3 → "~1.2.3"; no index written since
        // the member publishes to lpm's own registry.
        assert!(matches!(
            &manifest.dependencies["core"],
            Dependency::Registry { name, version, index: None }
                if name == "acme/core" && version == "~1.2.3"
        ));
        // The converted spec is what the packed manifest carries.
        let serialized = toml::to_string(&manifest).unwrap();
        assert!(serialized.contains(r#"name = "acme/core""#));
        assert!(serialized.contains(r#"version = "~1.2.3""#));
        assert!(!serialized.contains("workspace"));

        // A member that isn't in any workspace can't publish workspace deps.
        let lonely = base.join("lonely");
        fs::create_dir_all(&lonely).unwrap();
        let mut orphaned = manifest;
        orphaned.dependencies.insert(
            "again".to_string(),
            Dependency::Workspace {
                workspace: "acme/core".to_string(),
                version: "^".to_string(),
            },
        );
        assert!(matches!(
            convert_workspace_dependencies(&mut orphaned, &lonely),
            Err(Error::NotInWorkspace(_))
        ));

        let _ = fs::remove_dir_all(&base);
    }
}
