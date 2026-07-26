use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::project::lockfile::{LockedPackage, Lockfile};
use crate::project::manifest::{Environment, Manifest, Tool};
use crate::project::package;
use crate::registry::index;
use crate::registry::resolver;
use crate::tools;
use crate::ui;
use clap::Args;
use indicatif::ProgressBar;
use std::fs;
use std::path::Path;

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
            resolver::resolve(&manifest, true)
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

    // Installs are reproduced from scratch each run: every environment's
    // configured output folder is rebuilt even when there is nothing to
    // install, so removing the last dependency leaves no stale packages.
    for environment in Environment::ALL {
        let out = manifest.packages_out(environment);
        if out.exists() {
            fs::remove_dir_all(&out)?;
        }
    }

    // Extraction happens before the environment (and therefore the output
    // folder) is always known, so stage in a project-local temp dir; a rename
    // then moves it into place (same filesystem as the outputs).
    let staging = Path::new(".lpm-staging").to_path_buf();
    let locked = ui::with_progress(jobs.len() as u64, |bar| {
        install_packages(&manifest, jobs, &staging, bar)
    })?;

    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }

    let package_count = locked.len();
    if !args.locked {
        // Written even when empty, so lpm.lock always mirrors the manifest.
        Lockfile::new(locked).save()?;
    }

    // Tool versions are pinned exactly, so tools have no lockfile entries and
    // install the same way on normal and --locked runs. Global tools
    // (~/.lpm/tools.toml) install here too: `tool add` never downloads, so
    // this is the one place every tool gets installed.
    let mut tool_jobs: Vec<(String, Tool, bool)> = manifest
        .tools
        .iter()
        .map(|(alias, tool)| (alias.clone(), tool.clone(), false))
        .collect();
    for (alias, tool) in tools::shim::global_tools()? {
        // A pin listed identically in both scopes only needs one install
        let duplicate = manifest.tools.get(&alias).is_some_and(|project| {
            project.repository.eq_ignore_ascii_case(&tool.repository)
                && project.version == tool.version
        });
        if !duplicate {
            tool_jobs.push((alias, tool, true));
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

/// Downloads, stages, and links every package, reporting progress on `bar`.
/// The caller owns the bar's lifecycle so it gets cleared on errors too.
fn install_packages(
    manifest: &Manifest,
    jobs: Vec<Job>,
    staging: &Path,
    bar: &ProgressBar,
) -> Result<Vec<LockedPackage>, Error> {
    let mut locked = Vec::new();
    for job in jobs {
        bar.set_message(job.name.clone());
        if staging.exists() {
            fs::remove_dir_all(staging)?;
        }
        index::download(&job.source, staging)?;
        package::flatten_single_dir(staging)?;

        // Indices usually know the environment; otherwise ask the files
        // (lpm.toml -> pesde.toml -> wally.toml).
        let environment = match job.environment {
            Some(environment) => environment,
            None => package::environment(staging)
                .ok_or_else(|| Error::UnknownPackageEnvironment(job.name.clone()))?,
        };

        // Real package contents live under <out>/.lpm/<scope>_<name>/; a
        // <out>/<link>.luau file re-exports the package's entry point so
        // consumers can `require(".../<link>")`.
        let folder = job.name.replace('/', "_");
        let out = manifest.packages_out(environment);
        let storage = out.join(".lpm").join(&folder);
        fs::create_dir_all(storage.parent().expect("storage dir has a parent"))?;
        if storage.exists() {
            fs::remove_dir_all(&storage)?;
        }
        fs::rename(staging, &storage)?;

        match package::entry_point(&storage) {
            Some(entry) => {
                let types = link_types(&storage, &entry, &job.name, bar);
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

/// Exported types must be restated in the link file to survive the wrapper;
/// a package whose entry point can't be parsed still links, just without
/// its types.
fn link_types(package_dir: &Path, entry: &str, name: &str, bar: &ProgressBar) -> Vec<String> {
    let Some(source) =
        package::entry_source(package_dir, entry).and_then(|path| fs::read_to_string(path).ok())
    else {
        return Vec::new();
    };
    package::exported_types(&source).unwrap_or_else(|| {
        bar.suspend(|| {
            eprintln!(
                "warning: could not parse the entry point of {name}; its types are not re-exported"
            )
        });
        Vec::new()
    })
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

        // Another toolchain manager's shim earlier in PATH (aftman,
        // rokit) would run instead of ours and report its own errors;
        // surface that or the tool looks broken for no visible reason.
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
