use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::project::lockfile::{LockedPackage, Lockfile};
use crate::project::manifest::{Environment, Manifest, Tool};
use crate::project::package;
use crate::project::requires;
use crate::project::rojo;
use crate::project::workspace::{self, Workspace};
use crate::registry::index;
use crate::registry::resolver;
use crate::tools;
use crate::ui;
use clap::Args;
use indicatif::ProgressBar;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Install exactly what lpm.lock records, without re-resolving
    #[arg(long)]
    pub locked: bool,
}

struct Job {
    name: String,
    version: String,
    environment: Option<Environment>,
    source: index::DownloadSource,
    index_url: String,
    link: String,
}

pub fn run(args: InstallArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    install_project(&args, &manifest, true)?;

    /* a workspace root installs every member too, pesde's order: root first,
    then each member. member installs never recurse further (nested
    workspaces aren't a thing) */
    if !manifest.workspace_members().is_empty() {
        let workspace = Workspace::open(Path::new("."))?;
        for member in &workspace.members {
            if member.dir == workspace.root {
                continue;
            }
            println!("Installing {}", member.manifest.package.name);
            workspace::in_dir(&member.dir, || {
                let manifest = Manifest::load()?;
                install_project(&args, &manifest, false)
            })?;
        }
    }
    Ok(())
}

/** installs one project from the current directory. global tools only
install on the primary run: workspace members share them, and repeating
the merge per member would just re-print every pin. */
fn install_project(
    args: &InstallArgs,
    manifest: &Manifest,
    include_global_tools: bool,
) -> Result<(), Error> {
    let jobs: Vec<Job> = if args.locked {
        Lockfile::load()?
            .packages
            .into_iter()
            .map(|package| Job {
                name: package.name,
                version: package.version,
                environment: Some(package.environment),
                source: package.source,
                index_url: package.index,
                link: package.link,
            })
            .collect()
    } else {
        ui::with_spinner("Resolving dependencies", || {
            resolver::resolve(manifest, Path::new("."), true)
        })?
        .into_iter()
        .map(|package| Job {
            name: package.name,
            version: package.version.to_string(),
            environment: package.environment,
            source: package.source,
            index_url: package.index_url,
            link: package.link,
        })
        .collect()
    };

    /* installs rebuild from scratch each run: every env's output folder is
    wiped even with nothing to install, so removing the last dependency
    leaves no stale packages */
    for environment in Environment::ALL {
        let out = manifest.packages_out(environment);
        if out.exists() {
            fs::remove_dir_all(&out)?;
        }
    }

    /* extraction can happen before we know the environment (and so the
    output folder), so stage in a project-local temp dir; a rename then
    moves it into place (same filesystem as the outputs) */
    let staging = Path::new(".lpm-staging").to_path_buf();
    let locked = ui::with_progress(jobs.len() as u64, |bar| {
        install_packages(manifest, jobs, &staging, bar)
    })?;

    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }

    /* second pass, now that everything is extracted: link files *inside*
    each stored package for the dependencies it declares */
    let stored: Vec<StoredPackage> = locked
        .iter()
        .map(|package| match &package.source {
            /* members link in place, so they're link targets like anything
            else but never get written into: that would dirty a source tree
            the user edits */
            index::DownloadSource::Workspace { path } => StoredPackage {
                name: package.name.to_lowercase(),
                storage: PathBuf::from(path),
                environment: package.environment,
                in_place: true,
            },
            _ => StoredPackage {
                name: package.name.to_lowercase(),
                storage: manifest
                    .packages_out(package.environment)
                    .join(".lpm")
                    .join(package.name.replace('/', "_")),
                environment: package.environment,
                in_place: false,
            },
        })
        .collect();
    link_nested_dependencies(&stored, &mut |message| eprintln!("{message}"));

    let package_count = locked.len();
    if !args.locked {
        // written even when empty, so lpm.lock always mirrors the manifest
        Lockfile::new(locked).save()?;
    }

    /* tool versions are exact pins, so no lockfile entries; normal and
    --locked runs install them the same way. global tools (~/.lpm/tools.toml)
    install here too: `tool add` never downloads, this is the one place
    every tool gets installed */
    let mut tool_jobs: Vec<(String, Tool, bool)> = manifest
        .tools
        .iter()
        .map(|(alias, tool)| (alias.clone(), tool.clone(), false))
        .collect();
    if include_global_tools {
        for (alias, tool) in tools::shim::global_tools()? {
            // same pin in both scopes only needs one install
            let duplicate = manifest.tools.get(&alias).is_some_and(|project| {
                project.repository.eq_ignore_ascii_case(&tool.repository)
                    && project.version == tool.version
            });
            if !duplicate {
                tool_jobs.push((alias, tool, true));
            }
        }
    }

    let tool_count = tool_jobs.len();
    if !tool_jobs.is_empty() {
        println!("Installing tools");
        ui::with_progress(tool_count as u64, |bar| install_tools(&tool_jobs, bar))?;
    }

    match (package_count, tool_count) {
        (0, 0) => println!("Nothing to install"),
        (p, 0) => println!("Installed {p} package{}", ui::plural(p)),
        (0, t) => println!("Installed {t} tool{}", ui::plural(t)),
        (p, t) => println!(
            "Installed {p} package{} and {t} tool{}",
            ui::plural(p),
            ui::plural(t)
        ),
    }
    Ok(())
}

/** downloads, stages, and links every package, reporting progress on `bar`.
caller owns the bar's lifecycle so it gets cleared on errors too. */
fn install_packages(
    manifest: &Manifest,
    jobs: Vec<Job>,
    staging: &Path,
    bar: &ProgressBar,
) -> Result<Vec<LockedPackage>, Error> {
    let mut locked = Vec::new();
    for job in jobs {
        bar.set_message(job.name.clone());

        /* workspace members link in place (no download, no copy under .lpm/)
        so edits to the member are picked up without reinstalling, like
        pesde's symlinks */
        if let index::DownloadSource::Workspace { path } = &job.source {
            let member_dir = Path::new(path);
            let environment = job
                .environment
                .ok_or_else(|| Error::UnknownPackageEnvironment(job.name.clone()))?;
            let out = manifest.packages_out(environment);
            fs::create_dir_all(&out)?;

            match package::entry_point(member_dir) {
                Some(entry) => {
                    let mut require = workspace::relative_path(&out, member_dir);
                    if !entry.is_empty() {
                        require = format!("{require}/{entry}");
                    }
                    if !require.starts_with("..") {
                        require = format!("./{require}");
                    }
                    let types = link_types(member_dir, &entry, &job.name, &mut bar_warn(bar));
                    let link_path = out.join(format!("{}.luau", job.link));
                    fs::write(&link_path, package::link_contents_at(&require, &types))?;
                }
                None => warn_no_entry(&job.name, bar),
            }

            ui::bar_println(
                bar,
                &ui::success_line(&format!(
                    "{}@{} → {}/{} (workspace)",
                    job.name, job.version, environment, job.link
                )),
            );
            bar.inc(1);
            locked.push(LockedPackage {
                name: job.name,
                version: job.version,
                environment,
                link: job.link,
                index: job.index_url,
                source: job.source,
            });
            continue;
        }

        if staging.exists() {
            fs::remove_dir_all(staging)?;
        }
        index::download(&job.source, staging)?;
        package::flatten_single_dir(staging)?;

        /* indices usually know the environment; otherwise ask the files
        (lpm.toml -> pesde.toml -> wally.toml) */
        let environment = match job.environment {
            Some(environment) => environment,
            None => package::environment(staging)
                .ok_or_else(|| Error::UnknownPackageEnvironment(job.name.clone()))?,
        };

        /* real contents live under <out>/.lpm/<scope>_<name>/; a
        <out>/<link>.luau file re-exports the entry point so consumers
        can `require(".../<link>")` */
        let folder = job.name.replace('/', "_");
        let out = manifest.packages_out(environment);
        let storage = out.join(".lpm").join(&folder);
        fs::create_dir_all(storage.parent().expect("storage dir has a parent"))?;
        if storage.exists() {
            fs::remove_dir_all(&storage)?;
        }
        fs::rename(staging, &storage)?;

        /* a project file the package ships would mount it under its own
        name and tree, which our link files (and the package's own relative
        requires) don't spell; make it mirror the disk instead. after
        entry_point, so the entry is read from what the package shipped */
        let entry = package::entry_point(&storage);
        rojo::mirror_disk_layout(&storage, &mut bar_warn(bar));

        match entry {
            Some(entry) => {
                /* packages from any index can talk roblox instance paths
                (require(script.Parent.X)), wally stuff especially but ports
                published to pesde or our index too; rewrite what we can into
                string requires so they work without an instance tree. string
                require packages come through unchanged */
                requires::rewrite_instance_requires(&storage, &entry)?;
                let types = link_types(&storage, &entry, &job.name, &mut bar_warn(bar));
                let link_path = out.join(format!("{}.luau", job.link));
                fs::write(&link_path, package::link_contents(&folder, &entry, &types))?;
            }
            None => warn_no_entry(&job.name, bar),
        }

        ui::bar_println(
            bar,
            &ui::success_line(&format!(
                "{}@{} → {}/{}",
                job.name, job.version, environment, job.link
            )),
        );
        bar.inc(1);
        locked.push(LockedPackage {
            name: job.name,
            version: job.version,
            environment,
            link: job.link,
            index: job.index_url,
            source: job.source,
        });
    }
    Ok(locked)
}

/** one installed package, as the nested-link pass sees it. */
struct StoredPackage {
    /// lowercased "scope/name", the form dependency declarations resolve by
    name: String,
    /// where the contents live: <out>/.lpm/<scope>_<name>, or a member's own directory
    storage: PathBuf,
    environment: Environment,
    /// a workspace member: fine to link *to*, never written into
    in_place: bool,
}

/** link files *inside* a stored package for its own declared dependencies,
at `<storage>/packages/<env>/<alias>.luau`. that folder name is the default
layout, literally: published code was compiled against it (see Chief's
build), so a consumer's `[config]` output customization deliberately doesn't
apply inside packages. (a package published from a project that customized
*its* output dirs would want those names instead; nothing has needed that
yet.) each link requires the dependency's store entry directly.

nothing here is fatal: the install has already downloaded and extracted
everything, so a package that can't be linked warns and is skipped rather
than taking the whole run (and the lockfile write that follows) with it. */
fn link_nested_dependencies(packages: &[StoredPackage], warn: &mut impl FnMut(String)) {
    let by_name: HashMap<&str, &StoredPackage> = packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect();
    // exported types depend only on the dependency, so parse each once
    let mut types_cache: HashMap<String, Vec<String>> = HashMap::new();

    for package in packages.iter().filter(|package| !package.in_place) {
        for (alias, dependency) in package::declared_dependencies(&package.storage) {
            /* aliases are TOML keys from a *downloaded* manifest, and TOML
            keys can be quoted anything: a "../.." or absolute one would put
            the link outside the package (or outside the project) */
            if !is_plain_file_name(&alias) {
                warn(format!(
                    "warning: {} declares dependency {dependency} under the unusable alias '{alias}'; no nested link generated",
                    package.name
                ));
                continue;
            }
            let Some(dep) = by_name.get(dependency.as_str()) else {
                warn(format!(
                    "warning: {} declares dependency {dependency} which is not installed; no nested link generated",
                    package.name
                ));
                continue;
            };
            let Some(entry) = package::entry_point(&dep.storage) else {
                warn(format!(
                    "warning: could not find an entry point for {dependency}; no nested link generated in {}",
                    package.name
                ));
                continue;
            };

            let link_dir = package
                .storage
                .join("packages")
                .join(dep.environment.dir_name());
            if let Err(error) = fs::create_dir_all(&link_dir) {
                warn(format!(
                    "warning: could not create {} ({error}); no nested link generated for {dependency}",
                    link_dir.display()
                ));
                continue;
            }

            /* relative_path is lexical, so both sides go through
            std::path::absolute first: that drops the "." component a
            `[config]` dir like "./packages/shared" would otherwise
            contribute, and gives absolute out dirs a common prefix to
            measure from */
            let from = std::path::absolute(&link_dir).unwrap_or_else(|_| link_dir.clone());
            let to = std::path::absolute(&dep.storage).unwrap_or_else(|_| dep.storage.clone());
            let mut require = workspace::relative_path(&from, &to);
            if !entry.is_empty() {
                require = format!("{require}/{entry}");
            }
            if !require.starts_with("..") {
                require = format!("./{require}");
            }

            let types = types_cache
                .entry(dependency.clone())
                .or_insert_with(|| {
                    /* install_packages already parsed (and complained about)
                    every stored entry point, so no warn sink here: it would
                    just say the same thing twice */
                    package::entry_source(&dep.storage, &entry)
                        .and_then(|path| fs::read_to_string(path).ok())
                        .and_then(|source| package::exported_types(&source))
                        .unwrap_or_default()
                })
                .clone();
            let link_path = link_dir.join(format!("{alias}.luau"));
            if let Err(error) = fs::write(&link_path, package::link_contents_at(&require, &types)) {
                warn(format!(
                    "warning: could not write {} ({error})",
                    link_path.display()
                ));
            }
        }
    }
}

/// one ordinary path segment: no separators, no drive letter, not `.`/`..`.
fn is_plain_file_name(alias: &str) -> bool {
    !alias.is_empty()
        && alias != "."
        && alias != ".."
        && !alias.contains(['/', '\\', ':'])
        && !Path::new(alias).is_absolute()
}

/** exported types have to be restated in the link file to survive the
wrapper; an unparseable entry point still links, just without its types. */
fn link_types(
    package_dir: &Path,
    entry: &str,
    name: &str,
    warn: &mut impl FnMut(String),
) -> Vec<String> {
    let Some(source) =
        package::entry_source(package_dir, entry).and_then(|path| fs::read_to_string(path).ok())
    else {
        return Vec::new();
    };
    package::exported_types(&source).unwrap_or_else(|| {
        warn(format!(
            "warning: could not parse the entry point of {name}; its types are not re-exported"
        ));
        Vec::new()
    })
}

/// routes a warning line around the live progress bar.
fn bar_warn(bar: &ProgressBar) -> impl FnMut(String) + '_ {
    move |message: String| bar.suspend(|| eprintln!("{message}"))
}

fn warn_no_entry(name: &str, bar: &ProgressBar) {
    bar.suspend(|| {
        eprintln!("warning: could not find an entry point for {name}; no link file generated")
    });
}

fn install_tools(jobs: &[(String, Tool, bool)], bar: &ProgressBar) -> Result<(), Error> {
    let github = GithubAPI::new();
    for (alias, tool, global) in jobs {
        bar.set_message(tool.repository.clone());
        let downloaded = tools::install_tool(alias, tool, &github)?;
        let mut notes = Vec::new();
        if *global {
            notes.push("global");
        }
        if !downloaded {
            notes.push("cached");
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join(", "))
        };
        ui::bar_println(
            bar,
            &ui::success_line(&format!(
                "{}@{} → {alias}{notes}",
                tool.repository, tool.version
            )),
        );

        /* another toolchain manager's shim earlier in PATH (aftman, rokit)
        would run instead of ours and report its own errors; surface that
        or the tool looks broken for no visible reason */
        if let Some(shadow) = tools::shim::shadowing_executable(alias) {
            bar.suspend(|| {
                eprintln!(
                    "warning: `{alias}` resolves to {} on PATH before lpm's shims; that copy will run instead",
                    shadow.display()
                )
            });
        }
        bar.inc(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, file: &str, contents: &str) {
        let path = dir.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn stored(name: &str, storage: PathBuf, environment: Environment) -> StoredPackage {
        StoredPackage {
            // production lowercases here; mirror that so the tests exercise it
            name: name.to_lowercase(),
            storage,
            environment,
            in_place: false,
        }
    }

    #[test]
    fn writes_nested_links_for_stored_dependencies() {
        let base = std::env::temp_dir().join("lpm-test-nested-links");
        let _ = fs::remove_dir_all(&base);
        let shared = base.join("packages/shared");
        let luau = base.join("packages/luau");

        /* chief-shaped fixture: `lifecycles` depends on same-environment
        `core` (which exports a type) and cross-environment `util` (whose
        entry is its root init file) */
        let core = shared.join(".lpm/acme_core");
        write(&core, "lpm.toml", "[target]\nmain = \"out/lpm\"\n");
        write(
            &core,
            "out/lpm/init.luau",
            "export type Entry = { id: number }\nreturn {}\n",
        );
        let util = luau.join(".lpm/acme_util");
        write(&util, "init.luau", "return {}\n");
        let lifecycles = shared.join(".lpm/acme_lifecycles");
        write(
            &lifecycles,
            "lpm.toml",
            "[dependencies]\ncore = { name = \"acme/core\", version = \"^\" }\n\
             util = { name = \"acme/util\", version = \"^\" }\n",
        );
        write(&lifecycles, "out/lpm/init.luau", "return {}\n");

        let packages = [
            stored("Acme/Core", core.clone(), Environment::Shared),
            stored("acme/util", util, Environment::Luau),
            stored("acme/lifecycles", lifecycles.clone(), Environment::Shared),
        ];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));
        assert_eq!(warnings, Vec::<String>::new());

        /* the same-environment link: three hops up to the store, entry
        appended, exported types restated */
        assert_eq!(
            fs::read_to_string(lifecycles.join("packages/shared/core.luau")).unwrap(),
            "local module = require(\"../../../acme_core/out/lpm\")\n\
             export type Entry = module.Entry\n\
             return module\n"
        );
        /* the cross-environment link climbs out to the other output root;
        a root-init entry adds no suffix */
        assert_eq!(
            fs::read_to_string(lifecycles.join("packages/luau/util.luau")).unwrap(),
            "return require(\"../../../../../luau/.lpm/acme_util\")\n"
        );
        // packages without dependencies get no packages/ folder at all
        assert!(!core.join("packages").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_dependencies_warn_and_skip() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-missing");
        let _ = fs::remove_dir_all(&base);
        let storage = base.join("packages/shared/.lpm/acme_thing");
        write(
            &storage,
            "lpm.toml",
            "[dependencies]\ngone = { name = \"acme/gone\", version = \"^\" }\n",
        );

        let packages = [stored("acme/thing", storage.clone(), Environment::Shared)];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));

        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("acme/gone"));
        assert!(warnings[0].contains("not installed"));
        assert!(!storage.join("packages").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn aliases_cannot_escape_the_package() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-escape");
        let _ = fs::remove_dir_all(&base);
        let shared = base.join("packages/shared");

        let dep = shared.join(".lpm/acme_dep");
        write(&dep, "init.luau", "return {}\n");
        /* a downloaded manifest can quote anything as a key; neither of
        these may put a file outside the package */
        let hostile = shared.join(".lpm/acme_hostile");
        write(
            &hostile,
            "lpm.toml",
            "[dependencies]\n\"../../../../../../escaped\" = { name = \"acme/dep\", version = \"^\" }\n\
             \"C:/Windows/Temp/lpm-escaped\" = { name = \"acme/dep\", version = \"^\" }\n",
        );

        let packages = [
            stored("acme/dep", dep, Environment::Shared),
            stored("acme/hostile", hostile.clone(), Environment::Shared),
        ];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));

        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().all(|line| line.contains("unusable alias")));
        assert!(!hostile.join("packages").exists());
        assert!(!base.join("escaped.luau").exists());
        assert!(!Path::new("C:/Windows/Temp/lpm-escaped.luau").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn workspace_members_are_link_targets_but_never_written_into() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-member");
        let _ = fs::remove_dir_all(&base);

        /* a member consumed by the root shadows the registry copy of the
        same name, so packages depending on it must still link */
        let member = base.join("packages/core");
        write(&member, "lpm.toml", "[target]\nmain = \"src/init.luau\"\n");
        write(&member, "src/init.luau", "return {}\n");
        let consumer = base.join("packages/shared/.lpm/acme_extras");
        write(
            &consumer,
            "lpm.toml",
            "[dependencies]\ncore = { name = \"acme/core\", version = \"^\" }\n",
        );

        let packages = [
            StoredPackage {
                name: "acme/core".to_string(),
                storage: member.clone(),
                environment: Environment::Shared,
                in_place: true,
            },
            stored("acme/extras", consumer.clone(), Environment::Shared),
        ];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));

        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            fs::read_to_string(consumer.join("packages/shared/core.luau")).unwrap(),
            "return require(\"../../../../../core/src\")\n"
        );
        // the member's own source tree stays untouched
        assert!(!member.join("packages").exists());

        let _ = fs::remove_dir_all(&base);
    }
}
