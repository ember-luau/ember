use crate::auth;
use crate::error::Error;
use crate::github::GithubAPI;
use crate::index::Index;
use crate::manifest::{Environment, Manifest, split_package_name};
use crate::publish::{index_entry, pack};
use crate::ui;
use clap::Args;
use std::path::Path;

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
    let environment = manifest
        .target
        .as_ref()
        .map(|target| target.environment)
        .ok_or_else(|| {
            Error::ManifestInvalid(
                "publishing requires a [target] section with an environment".to_string(),
            )
        })?;
    let (scope, name) = split_package_name(&manifest.package.name)?;
    let version = semver::Version::parse(&manifest.package.version)?;
    let package_repo = manifest
        .package
        .repository_slug()
        .ok_or(Error::MissingRepository)?;

    let index_url = manifest.index_url(args.index.as_deref())?.to_string();
    let index = Index::open(&index_url, true)?;
    let client_id = index
        .github_oauth_id()
        .ok_or_else(|| Error::IndexNotPublishable(index_url.clone()))?
        .to_string();
    let index_repo = index_repo_slug(&index_url)?;

    let root = Path::new(".");
    let files = pack::packed_files(root, &manifest)?;
    let archive = with_spinner("Packing package", || pack::pack(root, &manifest))?;

    if args.dry_run {
        println!("Would publish {}@{version} with:", manifest.package.name);
        for file in &files {
            println!("  {}", file.display());
        }
        println!("Index entry: \"{version} {environment}\"");
        println!("Target index: {index_url}");
        return Ok(());
    }

    let credentials = auth::ensure_logged_in(&client_id)?;
    let github = GithubAPI::with_token(&credentials.token);
    let new_scope = scope_is_new(&index, scope, &credentials.login)?;

    let asset_name = format!("{scope}_{name}-{version}.tar.gz");
    let download_url = with_spinner("Uploading release asset", || {
        upload_asset(&github, &package_repo, &version, &asset_name, &archive)
    })?;
    ui::print_success(&format!("Uploaded {asset_name} to {package_repo}"));

    let publication = Publication {
        manifest: &manifest,
        environment,
        download_url: &download_url,
        new_scope,
    };
    let pull_url = with_spinner("Opening index pull request", || {
        open_index_pull(
            &github,
            &index,
            &index_repo,
            &credentials.login,
            &publication,
        )
    })?;
    ui::print_success(&format!("Opened {pull_url}"));
    ui::print_success(&format!(
        "{}@{version} goes live once the pull request merges",
        manifest.package.name
    ));
    Ok(())
}

/// Runs `work` under a spinner, clearing it before any error surfaces.
fn with_spinner<T>(message: &str, work: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    let spinner = ui::spinner(message);
    let result = work();
    spinner.finish_and_clear();
    result
}

/// "owner/repo" of the index itself. Publishing opens a PR against the index,
/// so it must live on GitHub.
fn index_repo_slug(url: &str) -> Result<String, Error> {
    url.strip_prefix("https://github.com/")
        .map(|slug| {
            slug.trim_end_matches('/')
                .trim_end_matches(".git")
                .to_string()
        })
        .filter(|slug| slug.split('/').count() == 2)
        .ok_or_else(|| Error::IndexNotPublishable(url.to_string()))
}

/// Checks `{scope}/owners.toml` in the freshly pulled index clone. Absent
/// means the scope is unclaimed and this publish creates it; present without
/// `login` in it is a hard error (first publisher owns the scope).
fn scope_is_new(index: &Index, scope: &str, login: &str) -> Result<bool, Error> {
    let path = index.root().join(scope).join("owners.toml");
    if !path.exists() {
        return Ok(true);
    }

    let owners = index_entry::owners(&std::fs::read_to_string(&path)?);
    if owners.iter().any(|owner| owner.eq_ignore_ascii_case(login)) {
        return Ok(false);
    }
    Err(Error::ScopeOwned {
        scope: scope.to_string(),
        owner: owners
            .first()
            .cloned()
            .unwrap_or_else(|| "another user".to_string()),
    })
}

/// Uploads the archive to the `v{version}` release on the package's own repo,
/// creating the release when it doesn't exist, and returns the asset's
/// download URL (what the index entry points at).
fn upload_asset(
    github: &GithubAPI,
    repo: &str,
    version: &semver::Version,
    asset_name: &str,
    archive: &[u8],
) -> Result<String, Error> {
    let tag = format!("v{version}");
    let release = match github.release_by_tag(repo, &tag)? {
        Some(release) => release,
        None => github.create_release(repo, &tag)?,
    };

    let asset = github.upload_release_asset(repo, release.id, asset_name, archive)?;
    Ok(asset.browser_download_url)
}

/// The version being published, as the index PR needs it.
struct Publication<'a> {
    manifest: &'a Manifest,
    environment: Environment,
    download_url: &'a str,
    /// Also claim {scope}/owners.toml in this PR (first publish in the scope).
    new_scope: bool,
}

fn open_index_pull(
    github: &GithubAPI,
    index: &Index,
    index_repo: &str,
    login: &str,
    publication: &Publication<'_>,
) -> Result<String, Error> {
    let manifest = publication.manifest;
    let (scope, name) = split_package_name(&manifest.package.name)?;
    let version = &manifest.package.version;

    let fork = github.create_fork(index_repo)?;
    let sha = github.branch_sha(&fork.full_name, &fork.default_branch)?;
    let branch = format!("publish/{scope}-{name}-{version}");
    github.create_branch(&fork.full_name, &branch, &sha)?;

    // Current content comes from the freshly pulled local clone; the blob sha
    // GitHub wants for an update comes from the fork branch just cut.
    let path = format!("{scope}/{name}");
    let cached = index.root().join(scope).join(name);
    let existing = cached
        .exists()
        .then(|| std::fs::read_to_string(&cached))
        .transpose()?;
    let previous_sha = github
        .get_file(&fork.full_name, &path, Some(&branch))?
        .map(|file| file.sha);

    let entry = index_entry::updated_package_file(
        existing.as_deref(),
        manifest,
        publication.environment,
        publication.download_url,
    )?;
    let message = format!("Publish {}@{version}", manifest.package.name);
    github.put_file(
        &fork.full_name,
        &path,
        &branch,
        &message,
        entry.as_bytes(),
        previous_sha.as_deref(),
    )?;

    if publication.new_scope {
        github.put_file(
            &fork.full_name,
            &format!("{scope}/owners.toml"),
            &branch,
            &format!("Claim scope {scope} for {login}"),
            index_entry::owners_file(login).as_bytes(),
            None,
        )?;
    }

    let base = github.get_repo(index_repo)?;
    let mut body = format!(
        "Publishes `{}` v{version}.\n\nAsset: {}",
        manifest.package.name, publication.download_url
    );
    if publication.new_scope {
        body.push_str(&format!(
            "\n\nFirst publish under `{scope}`; adds its owners file."
        ));
    }
    let pull = github.create_pull(
        index_repo,
        &base.default_branch,
        &format!("{login}:{branch}"),
        &message,
        &body,
    )?;
    Ok(pull.html_url)
}
