use crate::error::Error;
use crate::net::{auth, provenance, registry};
use crate::project::hooks::{self, Lifecycle};
use crate::project::manifest::{
    DEFAULT_INDEX_NAME, DEFAULT_INDEX_URL, Dependency, Environment, Language, Manifest,
    is_github_username, split_package_name, workspace_version_req,
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

    /* read off the manifest before publish_project consumes it. `prepublish`
    is the hook for building whatever ships, so it runs before the archive is
    packed -- including on --dry-run, since the point of a dry run is to see
    the archive a real publish would produce */
    let lifecycle = Lifecycle::of(&manifest, hooks::PUBLISH);
    lifecycle.before()?;

    /* provenance. in a GitHub Actions job with id-token permissions this
    fetches an OIDC token the registry can verify the build from. anywhere
    else, or on any failure, it's just None and nothing changes. fetched
    once so a workspace publish doesn't re-request per member */
    let oidc_token = if args.dry_run {
        None
    } else {
        provenance::actions_token()
    };
    let oidc_token = oidc_token.as_deref();

    if manifest.workspace_members().is_empty() {
        publish_project(manifest, args.dry_run, oidc_token)?;
        return lifecycle.after();
    }

    /* a workspace root publishes the whole workspace in pesde's order,
    root first, then every member. one package failing doesn't stop the
    rest, the command fails at the end instead */
    let workspace = Workspace::open(Path::new("."))?;
    let mut failed = Vec::new();

    let root_name = manifest.name().unwrap_or("the workspace root").to_string();
    if let Err(error) = publish_project(manifest, args.dry_run, oidc_token) {
        ui::print_error(&format!("{root_name}: {error}"));
        failed.push(root_name);
    }

    for member in &workspace.members {
        if member.dir == workspace.root {
            continue;
        }
        let name = member.label();
        let result = workspace::in_dir(&member.dir, || {
            let manifest = Manifest::load()?;
            let lifecycle = Lifecycle::of(&manifest, hooks::PUBLISH);
            lifecycle.before()?;
            publish_project(manifest, args.dry_run, oidc_token)?;
            lifecycle.after()
        });
        if let Err(error) = result {
            ui::print_error(&format!("{name}: {error}"));
            failed.push(name);
        }
    }

    if !failed.is_empty() {
        return Err(Error::WorkspacePublishFailed(failed));
    }

    // the root's `postpublish` waits for the whole workspace to be out
    lifecycle.after()
}

/** publishes the project in the current directory. private packages and
main-less workspace roots say so and skip instead of failing. that's how
a container root stays unpublished while its members go out. */
fn publish_project(
    mut manifest: Manifest,
    dry_run: bool,
    oidc_token: Option<&str>,
) -> Result<(), Error> {
    /* the one command that needs a [package] table: it is the registry
    metadata, and nothing else in embr reads it. a workspace root without one
    is the same pure container as a private or main-less root, so it steps
    aside for its members rather than failing the whole publish */
    let Some(package) = &manifest.package else {
        if !manifest.workspace_members().is_empty() {
            println!("Skipping the workspace root: it declares no [package]");
            return Ok(());
        }
        return Err(Error::PackageMissing);
    };

    if package.private {
        println!(
            "Skipping {}: package is private, refusing to publish",
            package.name
        );
        return Ok(());
    }
    /* a workspace root needs no `main`, without one it's a pure container
    and only its members go out */
    let has_main = manifest
        .target
        .as_ref()
        .is_some_and(|target| target.main.is_some());
    if !manifest.workspace_members().is_empty() && !has_main {
        println!("Skipping {}: workspace root without a main", package.name);
        return Ok(());
    }

    let environment = publish_environment(&manifest)?;
    let language = manifest
        .target
        .as_ref()
        .map(|target| target.language)
        .unwrap_or_default();
    // "lune", or "lune, alloy" when the files are not plain Luau
    let label = match language {
        Language::Luau => environment.to_string(),
        language => format!("{environment}, {language}"),
    };
    /* a dry run builds too: it shows the archive a real publish would
    produce, and that archive holds the build output */
    if let Some(program) = language.builder() {
        build_to_luau(Path::new("."), &manifest, program, None)?;
    }
    validate_logo(Path::new("."), package.logo.as_deref())?;
    let package_name = package.name.clone();
    let (scope, name) = {
        let (scope, name) = split_package_name(&package_name)?;
        (scope.to_string(), name.to_string())
    };
    let version = semver::Version::parse(&package.version)?;

    /* the API appends [package] authors to the scope's owner list, each one
    can then publish to the whole scope, and 400s anything not shaped like
    a GitHub username. catch that before packing. it does NOT check the
    account exists, so a typo silently grants a stranger-to-be */
    if let Some(author) = package
        .authors
        .iter()
        .find(|author| !is_github_username(author))
    {
        return Err(Error::ManifestInvalid(format!(
            "author '{author}' is not a GitHub username; [package] authors grant publish access to your scope and must be GitHub usernames (no emails or display names)"
        )));
    }

    // the API 400s descriptions past its cap, catch that before packing too
    validate_description(package.description.as_deref())?;

    /* workspace deps become registry ones in the archive's manifest,
    the on-disk ember.toml is never touched */
    convert_workspace_dependencies(&mut manifest, Path::new("."))?;
    strip_local_tables(&mut manifest);

    let root = Path::new(".");
    let files = pack::packed_files(root, &manifest)?;
    let archive = ui::with_spinner("Packing package", || pack::pack(root, &manifest))?;

    /* the API answers 413 past its cap, failing here saves the upload
    and lets --dry-run catch it too */
    if archive.len() > registry::MAX_ARCHIVE_BYTES {
        return Err(Error::PublishTooLarge {
            size_mb: archive.len() as f64 / (1024.0 * 1024.0),
            limit_mb: (registry::MAX_ARCHIVE_BYTES / (1024 * 1024)) as u64,
        });
    }

    if dry_run {
        println!(
            "Would publish {package_name}@{version} ({label}) to {}:",
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

    /* stored credentials first. the device flow, and the index clone that
    provides its client id, only when nothing is stored yet */
    let credentials = match auth::load()? {
        Some(credentials) => credentials,
        None => auth::login(&oauth_client_id()?)?,
    };

    match upload(&credentials.token, oidc_token, &archive) {
        /* 401 means the stored token was revoked or expired, not that this
        publish is doomed. forget it, log in fresh, retry once */
        Err(Error::PublishFailed { status: 401, .. }) => {
            auth::clear()?;
            eprintln!("warning: the registry rejected the stored GitHub token; logging in again");
            let credentials = auth::login(&oauth_client_id()?)?;
            upload(&credentials.token, oidc_token, &archive)?;
        }
        other => other?,
    }

    ui::print_success(&format!("Published {package_name}@{version} ({label})"));
    println!("Install it with `embr add {package_name}`");
    Ok(())
}

/** rewrites workspace deps into registry ones, pesde's conversion exactly.
the requirement is the specifier's version type applied to the member's
current on-disk version, `^` + `1.2.3` -> `^1.2.3`, and a full requirement
passes through. the member's `default` index url gets baked in when
it points somewhere other than embr's own registry. */
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
            target,
            entry,
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
        /* `member()` matched on the [package] name, so a member without one
        was never a candidate and this cannot be the missing-table case */
        let member_package = member
            .manifest
            .package
            .as_ref()
            .ok_or_else(|| Error::NoWorkspaceMember(name.clone()))?;
        let member_version = semver::Version::parse(&member_package.version)?;

        *dependency = Dependency::Registry {
            name: name.clone(),
            version: workspace_version_req(version, &member_version)?,
            index: member
                .manifest
                .indices
                .get(DEFAULT_INDEX_NAME)
                .filter(|url| url.as_str() != DEFAULT_INDEX_URL)
                .cloned(),
            // the specifier's `target` is the consumer's choice either way
            target: target.clone(),
            /* and so is `entry`: it says which module of the member to link,
            which is the same module once it publishes */
            entry: entry.clone(),
        };
    }
    Ok(())
}

/** tables that never travel, dropped from the archive's manifest.
[overrides] is only ever consulted on the installing project, and its
alias-form values refer to aliases only this manifest has. [patches] points
at files in this repo, which the archive doesn't carry. shipping either
would just mislead readers of the packed manifest. */
fn strip_local_tables(manifest: &mut Manifest) {
    manifest.overrides.clear();
    manifest.patches.clear();
}

/// registry caps descriptions at 200 chars, counted as chars, not bytes.
fn validate_description(description: Option<&str>) -> Result<(), Error> {
    let Some(description) = description else {
        return Ok(());
    };
    let length = description.chars().count();
    if length > registry::MAX_DESCRIPTION_CHARS {
        return Err(Error::ManifestInvalid(format!(
            "description is {length} characters; the registry allows at most {}",
            registry::MAX_DESCRIPTION_CHARS
        )));
    }
    Ok(())
}

fn upload(token: &str, oidc_token: Option<&str>, archive: &[u8]) -> Result<(), Error> {
    ui::with_spinner("Uploading package", || {
        registry::publish(token, oidc_token, archive)
    })
}

/** OAuth app client id for the device flow. lives in the embr index's
config.toml, not in the binary, so it can rotate without a CLI release. */
pub(crate) fn oauth_client_id() -> Result<String, Error> {
    /* a stale oauth id is harmless, it rotates roughly never, so any
    cached copy will do. a first-ever run still clones */
    let index = Index::open(DEFAULT_INDEX_URL, crate::registry::index::Refresh::Never)?;
    index
        .github_oauth_id()
        .map(str::to_string)
        .ok_or_else(|| Error::PublishNotSupported(DEFAULT_INDEX_URL.to_string()))
}

/** an Alloy or Loom package ships its sources and the Luau that the
compiler's `build` writes from them, so a plain Luau project can use it.
the build runs here and not in a hook, so a forgotten hook cannot publish
stale output. `compiler` stands in for the program on PATH, for tests. */
fn build_to_luau(
    root: &Path,
    manifest: &Manifest,
    program: &'static str,
    compiler: Option<&Path>,
) -> Result<(), Error> {
    println!("Running {program} build");
    let failed = |reason: String| Error::LanguageBuild { program, reason };
    let status = std::process::Command::new(compiler.unwrap_or(Path::new(program)))
        .arg("build")
        .current_dir(root)
        .status()
        .map_err(|error| {
            failed(match error.kind() {
                std::io::ErrorKind::NotFound => format!("the {program} command is not on PATH"),
                _ => format!("it could not start: {error}"),
            })
        })?;
    if !status.success() {
        return Err(failed(match status.code() {
            Some(code) => format!("it exited with status {code}"),
            None => "a signal stopped it".to_string(),
        }));
    }

    let not_built = |detail: String| Error::MainNotBuilt { program, detail };
    let main = manifest
        .target
        .as_ref()
        .and_then(|target| target.main.as_deref());
    match main {
        None => Err(not_built("ember.toml has no main".to_string())),
        Some(main) if !main.ends_with(".luau") => {
            Err(not_built(format!("ember.toml says '{main}'")))
        }
        Some(main) if !root.join(main).is_file() => Err(not_built(format!(
            "'{main}' does not exist after the build"
        ))),
        Some(_) => Ok(()),
    }
}

/** checked here so a bad logo fails before the upload. the registry checks
the file's bytes again, this only catches the plain mistakes. */
fn validate_logo(root: &Path, logo: Option<&str>) -> Result<(), Error> {
    let Some(logo) = logo else {
        return Ok(());
    };
    let extension = Path::new(logo)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(extension.as_deref(), Some("png" | "jpg" | "jpeg" | "webp")) {
        return Err(Error::LogoInvalid(format!(
            "'{logo}' must be a .png, .jpg, or .webp file"
        )));
    }
    let size = std::fs::metadata(root.join(logo))
        .map_err(|_| Error::LogoInvalid(format!("'{logo}' does not exist")))?
        .len();
    if size > registry::MAX_LOGO_BYTES {
        return Err(Error::LogoInvalid(format!(
            "'{logo}' is {} KiB; the registry takes at most {} KiB",
            size.div_ceil(1024),
            registry::MAX_LOGO_BYTES / 1024
        )));
    }
    Ok(())
}

/// published packages must say where their code runs, the index entry is keyed by it.
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
        let base = std::env::temp_dir().join("embr-test-publish-convert");
        let _ = fs::remove_dir_all(&base);

        let write = |path: &str, contents: &str| {
            let path = base.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        };
        write(
            "ember.toml",
            "[package]\nname = \"acme/root\"\nversion = \"0.0.0\"\nprivate = true\n\n\
             [target]\nenvironment = \"roblox\"\nworkspace = [\"packages/*\"]\n",
        );
        write(
            "packages/core/ember.toml",
            "[package]\nname = \"acme/core\"\nversion = \"1.2.3\"\n\n[target]\nenvironment = \"roblox\"\n",
        );
        write(
            "packages/extra/ember.toml",
            "[package]\nname = \"acme/extra\"\nversion = \"0.2.0\"\n\n[target]\nenvironment = \"roblox\"\n\n\
             [dependencies]\ncore = { workspace = \"acme/core\", version = \"~\" }\n",
        );

        let member_dir = base.join("packages/extra");
        let mut manifest = Manifest::load_from(&member_dir.join("ember.toml")).unwrap();
        convert_workspace_dependencies(&mut manifest, &member_dir).unwrap();

        /* "~" + the member's on-disk 1.2.3 -> "~1.2.3", no index written
        since the member publishes to embr's own registry */
        assert!(matches!(
            &manifest.dependencies["core"],
            Dependency::Registry { name, version, index: None, .. }
                if name == "acme/core" && version == "~1.2.3"
        ));
        // the converted spec is what the packed manifest carries
        let serialized = toml::to_string(&manifest).unwrap();
        assert!(serialized.contains(r#"name = "acme/core""#));
        assert!(serialized.contains(r#"version = "~1.2.3""#));
        assert!(!serialized.contains("workspace"));

        // a member outside any workspace can't publish workspace deps
        let lonely = base.join("lonely");
        fs::create_dir_all(&lonely).unwrap();
        let mut orphaned = manifest;
        orphaned.dependencies.insert(
            "again".to_string(),
            Dependency::Workspace {
                workspace: "acme/core".to_string(),
                version: "^".to_string(),
                target: None,
                entry: None,
            },
        );
        assert!(matches!(
            convert_workspace_dependencies(&mut orphaned, &lonely),
            Err(Error::NotInWorkspace(_))
        ));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn publish_strips_overrides_and_patches() {
        let mut manifest: Manifest = toml::from_str(
            "[package]\nname = \"acme/x\"\nversion = \"1.0.0\"\n\n\
             [target]\nenvironment = \"roblox\"\n\n\
             [overrides]\n\"a.b\" = \"acme/fork\"\n\n\
             [patches]\n\"acme/dep@1.0.0\" = \"patches/acme_dep@1.0.0.patch\"\n",
        )
        .unwrap();
        assert!(!manifest.overrides.is_empty());
        assert!(!manifest.patches.is_empty());

        strip_local_tables(&mut manifest);
        let serialized = toml::to_string(&manifest).unwrap();
        assert!(!serialized.contains("overrides"), "{serialized}");
        assert!(!serialized.contains("patches"), "{serialized}");
    }

    #[test]
    fn descriptions_past_the_cap_are_rejected() {
        assert!(validate_description(None).is_ok());
        assert!(validate_description(Some("short and sweet")).is_ok());
        assert!(validate_description(Some(&"a".repeat(200))).is_ok());
        assert!(matches!(
            validate_description(Some(&"a".repeat(201))),
            Err(Error::ManifestInvalid(_))
        ));
        // chars, not bytes, 200 two-byte chars are still fine
        assert!(validate_description(Some(&"é".repeat(200))).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn built_languages_need_a_build_and_a_built_main() {
        let base = std::env::temp_dir().join("embr-test-publish-build");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("out")).unwrap();
        let manifest = |main: &str| -> Manifest {
            toml::from_str(&format!(
                "[target]\nenvironment = \"roblox\"\nlanguage = \"alloy\"\nmain = \"{main}\"\n"
            ))
            .unwrap()
        };
        let build = |main: &str, compiler: &str| {
            build_to_luau(&base, &manifest(main), "alloy", Some(Path::new(compiler)))
        };

        // `true` stands in for a build that works, `false` for one that fails
        assert!(matches!(
            build("out/init.luau", "false"),
            Err(Error::LanguageBuild { .. })
        ));
        assert!(matches!(
            build("out/init.luau", "embr-test-no-compiler"),
            Err(Error::LanguageBuild { reason, .. }) if reason.contains("PATH")
        ));
        // main must name built Luau, and the build must have written it
        assert!(matches!(
            build("src/init.aly", "true"),
            Err(Error::MainNotBuilt { .. })
        ));
        assert!(matches!(
            build("out/init.luau", "true"),
            Err(Error::MainNotBuilt { .. })
        ));
        fs::write(base.join("out/init.luau"), "return {}").unwrap();
        assert!(build("out/init.luau", "true").is_ok());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn logos_are_small_pictures_that_exist() {
        let base = std::env::temp_dir().join("embr-test-publish-logo");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("assets")).unwrap();
        fs::write(base.join("assets/logo.png"), [0u8; 16]).unwrap();
        fs::write(base.join("assets/logo.svg"), "<svg/>").unwrap();
        fs::write(
            base.join("assets/huge.webp"),
            vec![0u8; registry::MAX_LOGO_BYTES as usize + 1],
        )
        .unwrap();

        assert!(validate_logo(&base, None).is_ok());
        assert!(validate_logo(&base, Some("assets/logo.png")).is_ok());
        for (logo, says) in [
            ("assets/logo.svg", ".png, .jpg, or .webp"),
            ("assets/missing.png", "does not exist"),
            ("assets/huge.webp", "at most 512 KiB"),
        ] {
            let error = validate_logo(&base, Some(logo)).unwrap_err().to_string();
            assert!(error.contains(says), "{logo}: {error}");
        }

        let _ = fs::remove_dir_all(&base);
    }
}
