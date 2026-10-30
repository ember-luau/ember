use crate::error::Error;
use crate::net::{auth, provenance, registry};
use crate::project::hooks::{self, Lifecycle};
use crate::project::manifest::{
    DEFAULT_INDEX_NAME, DEFAULT_INDEX_URL, Dependency, Manifest, Target, is_github_username,
    split_package_name, workspace_version_req,
};
use crate::project::package;
use crate::project::workspace::{self, Workspace};
use crate::registry::{local, pack};
use crate::ui;
use clap::Args;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Args, Debug)]
pub struct PublishArgs {
    /// Show what would be published without uploading anything
    #[arg(long)]
    pub dry_run: bool,
    /// Publish only this target, such as `roblox` or `roblox-alloy`; repeat it for more
    #[arg(long = "target", value_name = "ID")]
    pub targets: Vec<String>,
    /// Publish to the local registry in ~/.ember/local, with no login and no upload
    #[arg(long)]
    pub local: bool,
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
    let oidc_token = if args.dry_run || args.local {
        None
    } else {
        provenance::actions_token()
    };
    let oidc_token = oidc_token.as_deref();

    if manifest.workspace_members().is_empty() {
        publish_project(manifest, &args, oidc_token)?;
        return lifecycle.after();
    }

    /* a workspace root publishes the whole workspace in pesde's order,
    root first, then every member. one package failing doesn't stop the
    rest, the command fails at the end instead */
    let workspace = Workspace::open(Path::new("."))?;
    let mut failed = Vec::new();

    let root_name = manifest.name().unwrap_or("the workspace root").to_string();
    if let Err(error) = publish_project(manifest, &args, oidc_token) {
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
            publish_project(manifest, &args, oidc_token)?;
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

/** publishes the project in the current directory, one archive for each
target that `selected` names, or for each target when it names none.
private packages and main-less workspace roots say so and skip instead of
failing. that's how a container root stays unpublished while its members
go out. */
fn publish_project(
    mut manifest: Manifest,
    args: &PublishArgs,
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
        .targets()
        .iter()
        .any(|target| target.main.is_some());
    if !manifest.workspace_members().is_empty() && !has_main {
        println!("Skipping {}: workspace root without a main", package.name);
        return Ok(());
    }

    let ids = select_targets(manifest.targets(), &args.targets)?;
    let root = Path::new(".");
    /* a dry run builds too: it shows the archives a real publish would
    produce, and they hold the build output. each compiler runs once, before
    any target packs, so every archive sees the same output */
    let mut builds = BTreeMap::new();
    for target in manifest.targets() {
        if let Some(program) = target.language.builder()
            && ids.contains(&target.id())
        {
            builds
                .entry(program)
                .or_insert_with(|| run_build(root, program, None));
        }
    }
    validate_logo(root, package.logo.as_deref())?;
    let package_name = package.name.clone();
    split_package_name(&package_name)?;
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
    convert_workspace_dependencies(&mut manifest, root)?;
    strip_local_tables(&mut manifest);

    let targets: Vec<Target> = std::mem::take(&mut manifest.targets)
        .into_iter()
        .filter(|target| ids.contains(&target.id()))
        .collect();
    let count = targets.len();
    let mut token: Option<String> = None;
    let mut failed = Vec::new();
    for target in targets {
        let id = target.id();
        /* the archive's ember.toml holds this one target as a plain
        [target] table, which is all the registry accepts */
        manifest.targets = vec![target];
        let result = publish_target(
            root,
            &manifest,
            &builds,
            &format!("{package_name}@{version}"),
            args,
            oidc_token,
            &mut token,
        );
        match result {
            Ok(()) => {}
            // one target fails the way a single publish always has
            Err(error) if count == 1 => return Err(error),
            Err(error) => {
                ui::print_error(&format!("{id}: {error}"));
                failed.push(id);
            }
        }
    }

    if !args.dry_run && failed.len() < count {
        // a name with no scope is how `embr add` finds a local package
        let (_, short_name) = split_package_name(&package_name)?;
        let name = if args.local {
            short_name
        } else {
            &package_name
        };
        println!("Install it with `embr add {name}`");
    }
    if !failed.is_empty() {
        return Err(Error::TargetPublishFailed(failed));
    }
    Ok(())
}

/** the IDs of the targets that `selected` names, in manifest order, or of
every target when it names none. an ID that no target has is an error that
lists the valid ones. */
fn select_targets(targets: &[Target], selected: &[String]) -> Result<Vec<String>, Error> {
    if targets.is_empty() {
        return Err(Error::ManifestInvalid(
            "publishing requires a [target] section with an environment".to_string(),
        ));
    }
    let ids: Vec<String> = targets.iter().map(Target::id).collect();
    if let Some(unknown) = selected.iter().find(|id| !ids.contains(id)) {
        return Err(Error::UnknownTarget {
            id: unknown.clone(),
            valid: ids.join(", "),
        });
    }
    Ok(ids
        .into_iter()
        .filter(|id| selected.is_empty() || selected.contains(id))
        .collect())
}

/** packs and uploads the one target that `manifest` holds. `builds` has
the outcome of each compiler that the selected targets need. `token` is
the GitHub token, read on the first upload and kept for the next. */
fn publish_target(
    root: &Path,
    manifest: &Manifest,
    builds: &BTreeMap<&'static str, Result<(), String>>,
    package: &str,
    args: &PublishArgs,
    oidc_token: Option<&str>,
    token: &mut Option<String>,
) -> Result<(), Error> {
    let target = manifest.target().expect("publish sets one target");
    let id = target.id();
    if let Some(program) = target.language.builder() {
        if let Some(Err(reason)) = builds.get(program) {
            return Err(Error::LanguageBuild {
                program,
                reason: reason.clone(),
            });
        }
        check_built_main(root, target.main.as_deref(), program)?;
    }

    let files = pack::packed_files(root, manifest)?;
    /* binary .rbxm models ship as they are, and the registry scans them.
    it refuses .rbxmx, so this says so before any upload */
    if let Some(xml) = files.iter().find(|file| {
        file.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("rbxmx"))
    }) {
        return Err(Error::XmlModel(pack::tar_path(xml)));
    }
    let archive = ui::with_spinner(&format!("Packing {id}"), || pack::pack(root, manifest))?;

    /* the API answers 413 past its cap, failing here saves the upload
    and lets --dry-run catch it too */
    if archive.len() > registry::MAX_ARCHIVE_BYTES {
        return Err(Error::PublishTooLarge {
            size_mb: archive.len() as f64 / (1024.0 * 1024.0),
            limit_mb: (registry::MAX_ARCHIVE_BYTES / (1024 * 1024)) as u64,
        });
    }

    let entry = entry_line(root, target.main.as_deref());
    if entry.is_none() {
        eprintln!(
            "warning: {package} has no entry point and no modules to collect, so a link to it requires nothing"
        );
    }

    let destination = if args.local {
        "the local registry".to_string()
    } else {
        registry::api_url()
    };
    if args.dry_run {
        println!(
            "Would publish {package} ({id}) to {destination}, {} bytes:",
            archive.len()
        );
        for file in &files {
            println!("  {}", file.display());
        }
        if let Some(entry) = entry {
            println!("Entry: {entry}");
        }
        return Ok(());
    }

    if args.local {
        local::publish(manifest, &archive)?;
        ui::print_success(&format!("Published {package} ({id}) to the local registry"));
        return Ok(());
    }

    // EMBER_TOKEN or the stored login first. the browser login only when there is neither
    let stored = match token.take() {
        Some(stored) => stored,
        None => match auth::token()? {
            Some(stored) => stored,
            None => auth::login()?.token,
        },
    };
    let stored = match upload(&stored, oidc_token, &archive) {
        // CI has no one to approve a login
        Err(Error::PublishFailed { status: 401, .. }) if auth::env_token().is_some() => {
            return Err(Error::TokenRejected);
        }
        /* 401 means the stored token was revoked or expired, not that this
        publish is doomed. forget it, log in fresh, retry once */
        Err(Error::PublishFailed { status: 401, .. }) => {
            auth::clear()?;
            eprintln!("warning: the registry rejected the stored token; logging in again");
            let fresh = auth::login()?.token;
            upload(&fresh, oidc_token, &archive)?;
            fresh
        }
        other => {
            other?;
            stored
        }
    };
    *token = Some(stored);

    ui::print_success(&format!("Published {package} ({id})"));
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
            key: None,
        };
    }
    Ok(())
}

/** tables that never travel, dropped from the archive's manifest.
[overrides] is only ever consulted on the installing project, and its
alias-form values refer to aliases only this manifest has. [patches] points
at files in this repo, which the archive doesn't carry. shipping either
would just mislead readers of the packed manifest. a dependency's link
key stays home too: in the archive, every reader of the package gets the
link. */
fn strip_local_tables(manifest: &mut Manifest) {
    manifest.overrides.clear();
    manifest.patches.clear();
    for dependency in manifest.dependencies.values_mut() {
        if let Dependency::Registry { key, .. } = dependency {
            *key = None;
        }
    }
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

/** runs `{program} build` in `root`, the build of an Alloy or Loom
package. such a package ships its sources and the Luau that the build
writes from them, so a plain Luau project can use it. the build runs here
and not in a hook, so a forgotten hook cannot publish stale output.
`compiler` stands in for the program on PATH, for tests. Err is the reason
for an [`Error::LanguageBuild`]. */
fn run_build(root: &Path, program: &'static str, compiler: Option<&Path>) -> Result<(), String> {
    println!("Running {program} build");
    let status = std::process::Command::new(compiler.unwrap_or(Path::new(program)))
        .arg("build")
        .current_dir(root)
        .status()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("the {program} command is not on PATH"),
            _ => format!("it could not start: {error}"),
        })?;
    if !status.success() {
        return Err(match status.code() {
            Some(code) => format!("it exited with status {code}"),
            None => "a signal stopped it".to_string(),
        });
    }
    Ok(())
}

/// refuses a `main` that does not name the Luau file that `program` built.
fn check_built_main(root: &Path, main: Option<&str>, program: &'static str) -> Result<(), Error> {
    let not_built = |detail: String| Error::MainNotBuilt { program, detail };
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

/** what a link to this target requires, found the way install finds it:
`main`, then what the package ships, then a guess, then a collection. a
module shows as its file, a collection as its members. None = nothing a
link can require. */
fn entry_line(root: &Path, main: Option<&str>) -> Option<String> {
    let entry = main
        .map(package::normalize_entry)
        .or_else(|| package::entry_point(root))
        .or_else(|| package::guess_entry(root))
        .or_else(|| package::collection_root(root))?;
    let members = package::collection_members(root, &entry);
    if !members.is_empty() {
        return Some(format!(
            "collection of {} modules ({})",
            members.len(),
            members.join(", ")
        ));
    }
    let source = package::entry_source(root, &entry)?;
    Some(pack::tar_path(source.strip_prefix(root).unwrap_or(&source)))
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
    fn publish_strips_overrides_patches_and_link_keys() {
        let mut manifest: Manifest = toml::from_str(
            "[package]\nname = \"acme/x\"\nversion = \"1.0.0\"\n\n\
             [target]\nenvironment = \"roblox\"\n\n\
             [dependencies]\nfoo = { name = \"acme/foo\", version = \"^1\", key = \"0123456789abcdef0123456789abcdef\" }\n\n\
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
        assert!(!serialized.contains("0123456789abcdef"), "{serialized}");
        assert!(serialized.contains("acme/foo"), "{serialized}");
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
        let build = |compiler: &str| run_build(&base, "alloy", Some(Path::new(compiler)));
        let check = |main: Option<&str>| check_built_main(&base, main, "alloy");

        // `true` stands in for a build that works, `false` for one that fails
        assert!(build("true").is_ok());
        assert!(build("false").unwrap_err().contains("status 1"));
        assert!(build("embr-test-no-compiler").unwrap_err().contains("PATH"));
        // main must name built Luau, and the build must have written it
        for main in [None, Some("src/init.aly"), Some("out/init.luau")] {
            assert!(
                matches!(check(main), Err(Error::MainNotBuilt { .. })),
                "{main:?}"
            );
        }
        fs::write(base.join("out/init.luau"), "return {}").unwrap();
        assert!(check(Some("out/init.luau")).is_ok());

        let _ = fs::remove_dir_all(&base);
    }

    /// a dry run names the module a link requires, or the members of a collection.
    #[test]
    fn the_entry_line_names_a_module_or_a_collection() {
        let base = std::env::temp_dir().join("embr-test-publish-entry");
        let _ = fs::remove_dir_all(&base);
        for file in ["src/Signal.luau", "src/Promise.luau", "lib/init.luau"] {
            let path = base.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "return {}\n").unwrap();
        }

        assert_eq!(
            entry_line(&base, Some("lib/init.luau")).as_deref(),
            Some("lib/init.luau")
        );
        assert_eq!(
            entry_line(&base, Some("src")).as_deref(),
            Some("collection of 2 modules (Promise, Signal)")
        );
        // nothing stated, and the conventional `lib/init.luau` wins
        assert_eq!(entry_line(&base, None).as_deref(), Some("lib/init.luau"));
        fs::remove_dir_all(base.join("lib")).unwrap();
        assert_eq!(
            entry_line(&base, None).as_deref(),
            Some("collection of 2 modules (Promise, Signal)")
        );
        fs::remove_dir_all(base.join("src")).unwrap();
        assert_eq!(entry_line(&base, None), None);

        let _ = fs::remove_dir_all(&base);
    }

    /// one package for Roblox in Luau and in Alloy, and for Luau in Luwu.
    const THREE_TARGETS: &str = "[package]\nname = \"acme/multi\"\nversion = \"1.0.0\"\n\n\
        [[target]]\nenvironment = \"roblox\"\nmain = \"luau/init.luau\"\nincludes = [\"luau\"]\n\n\
        [[target]]\nenvironment = \"roblox\"\nlanguage = \"alloy\"\nmain = \"out/init.luau\"\n\
        includes = [\"src\", \"out\"]\n\n\
        [[target]]\nenvironment = \"luau\"\nlanguage = \"luwu\"\nmain = \"luwu/init.luwu\"\n\
        includes = [\"luwu\"]\n";

    /// a project dir for [`THREE_TARGETS`], the Alloy build already run.
    fn three_target_project(name: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&base);
        for file in [
            "luau/init.luau",
            "src/init.aly",
            "out/init.luau",
            "luwu/init.luwu",
            "README.md",
        ] {
            fs::create_dir_all(base.join(file).parent().unwrap()).unwrap();
            fs::write(base.join(file), "return {}").unwrap();
        }
        fs::write(base.join("ember.toml"), THREE_TARGETS).unwrap();
        base
    }

    #[test]
    fn the_target_flag_repeats() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "embr",
            "publish",
            "--target",
            "roblox",
            "--target",
            "roblox-alloy",
        ])
        .unwrap();
        let Some(crate::cli::Commands::Publish(publish)) = cli.command else {
            panic!("publish parses as publish");
        };
        assert_eq!(publish.targets, ["roblox", "roblox-alloy"]);
    }

    #[test]
    fn targets_are_picked_by_id() {
        let manifest: Manifest = toml::from_str(THREE_TARGETS).unwrap();
        let pick = |selected: &[&str]| {
            let selected: Vec<String> = selected.iter().map(|id| id.to_string()).collect();
            select_targets(manifest.targets(), &selected)
        };

        assert_eq!(pick(&[]).unwrap(), ["roblox", "roblox-alloy", "luau-luwu"]);
        assert_eq!(pick(&["roblox-alloy"]).unwrap(), ["roblox-alloy"]);
        // manifest order, whatever order the flags came in
        assert_eq!(
            pick(&["luau-luwu", "roblox"]).unwrap(),
            ["roblox", "luau-luwu"]
        );
        let error = pick(&["roblox", "lune"]).unwrap_err().to_string();
        assert!(
            error.contains("'lune'") && error.contains("roblox, roblox-alloy, luau-luwu"),
            "{error}"
        );

        let bare: Manifest =
            toml::from_str("[package]\nname = \"acme/x\"\nversion = \"1.0.0\"\n").unwrap();
        assert!(matches!(
            select_targets(bare.targets(), &[]),
            Err(Error::ManifestInvalid(_))
        ));
    }

    #[test]
    fn each_target_packs_its_own_files_and_one_target_table() {
        let base = three_target_project("embr-test-publish-targets");
        let mut manifest = Manifest::load_from(&base.join("ember.toml")).unwrap();
        let expected = [
            ("roblox", vec!["ember.toml", "luau/init.luau"]),
            (
                "roblox-alloy",
                vec!["ember.toml", "out/init.luau", "src/init.aly"],
            ),
            ("luau-luwu", vec!["ember.toml", "luwu/init.luwu"]),
        ];

        for (target, (id, files)) in std::mem::take(&mut manifest.targets)
            .into_iter()
            .zip(expected)
        {
            assert_eq!(target.id(), id);
            manifest.targets = vec![target];
            let archive = pack::pack(&base, &manifest).unwrap();

            let mut unpacked = BTreeMap::new();
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive.as_slice()));
            for entry in tar.entries().unwrap() {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap().to_string_lossy().into_owned();
                let mut contents = String::new();
                std::io::Read::read_to_string(&mut entry, &mut contents).unwrap();
                unpacked.insert(path, contents);
            }
            assert_eq!(unpacked.keys().collect::<Vec<_>>(), files, "{id}");

            // a plain [target] table, which is all the registry accepts
            let packed = &unpacked["ember.toml"];
            assert!(
                packed.contains("[target]") && !packed.contains("[[target]]"),
                "{packed}"
            );
            let reparsed: Manifest = toml::from_str(packed).unwrap();
            assert_eq!(reparsed.targets().len(), 1);
            assert_eq!(reparsed.target().unwrap().id(), id);
        }

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_failed_build_fails_only_the_targets_that_need_it() {
        let base = three_target_project("embr-test-publish-isolation");
        let mut manifest = Manifest::load_from(&base.join("ember.toml")).unwrap();
        let builds = BTreeMap::from([("alloy", Err("it exited with status 1".to_string()))]);

        for target in std::mem::take(&mut manifest.targets) {
            let id = target.id();
            manifest.targets = vec![target];
            let result = publish_target(
                &base,
                &manifest,
                &builds,
                "acme/multi@1.0.0",
                &PublishArgs {
                    dry_run: true,
                    targets: Vec::new(),
                    local: false,
                },
                None,
                &mut None,
            );
            match id.as_str() {
                "roblox-alloy" => assert!(
                    matches!(
                        result,
                        Err(Error::LanguageBuild {
                            program: "alloy",
                            ..
                        })
                    ),
                    "{result:?}"
                ),
                _ => assert!(result.is_ok(), "{id}: {result:?}"),
            }
        }

        let _ = fs::remove_dir_all(&base);
    }

    /// .rbxm models ship byte for byte for the registry to scan, and an
    /// .rbxmx stops the publish before anything is packed or uploaded.
    #[test]
    fn binary_models_ship_and_xml_models_stop_the_publish() {
        let base = std::env::temp_dir().join("embr-test-publish-models");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/Models")).unwrap();
        fs::write(
            base.join("ember.toml"),
            "[package]\nname = \"acme/rock\"\nversion = \"1.0.0\"\n\n[target]\nenvironment = \"roblox\"\n",
        )
        .unwrap();
        fs::write(base.join("src/init.luau"), "return {}\n").unwrap();
        let model = b"<roblox!\x89\xff\r\n\x1a\n\0\0 binary";
        fs::write(base.join("src/Models/Rock.rbxm"), model).unwrap();
        let manifest = Manifest::load_from(&base.join("ember.toml")).unwrap();

        let archive = pack::pack(&base, &manifest).unwrap();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive.as_slice()));
        let shipped = tar
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| entry.path().unwrap().ends_with("Rock.rbxm"))
            .map(|mut entry| {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
                bytes
            });
        assert_eq!(shipped.as_deref(), Some(&model[..]));

        fs::write(base.join("src/Models/Tree.rbxmx"), "<roblox/>").unwrap();
        let result = publish_target(
            &base,
            &manifest,
            &BTreeMap::new(),
            "acme/rock@1.0.0",
            &PublishArgs {
                dry_run: true,
                targets: Vec::new(),
                local: false,
            },
            None,
            &mut None,
        );
        let error = result.unwrap_err().to_string();
        assert!(
            error.starts_with("src/Models/Tree.rbxmx: XML models (.rbxmx) are not accepted"),
            "{error}"
        );

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
