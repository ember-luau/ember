use std::{collections::BTreeMap, env::consts::EXE_SUFFIX, fs, path::PathBuf};

use crate::{
    error::Error,
    net::{github::GithubAPI, http::responses::Release, registry},
    project::manifest::{
        EMBER_PREFIX, NPM_PREFIX, Tool,
        edit::{ManifestDoc, Scope},
        split_npm_spec, split_package_name,
    },
    sys::paths,
    tools, ui,
};
use clap::{Subcommand, ValueEnum};
use semver::Version;

/// the index that `tool add` takes an owner/repo from.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum ToolIndex {
    Ember,
    Github,
}

#[derive(Subcommand, Debug)]
pub enum ToolCommand {
    /// Use a tool in the current project
    Add {
        /// Name of the tool: owner/repo, ember:scope/name, or npm:package for a tool that npm publishes
        name: String,

        /// Specific version to add
        #[arg(short, long)]
        version: Option<String>,

        /// Add to the global tools file, usable anywhere
        #[arg(short, long)]
        global: bool,

        /// Take an owner/repo from this index only. Without it, Ember first, then GitHub
        #[arg(long, value_enum)]
        index: Option<ToolIndex>,
    },

    /// Publish a version of a tool to Ember, from tool.toml
    Publish {
        /// Files to upload as the version's assets
        files: Vec<PathBuf>,

        /// Path to tool.toml
        #[arg(long, value_name = "PATH", default_value = "tool.toml")]
        manifest: PathBuf,

        /// Version to publish, in place of the one in tool.toml
        #[arg(long)]
        version: Option<String>,

        /// Markdown file with the release notes
        #[arg(long, value_name = "FILE")]
        notes: Option<PathBuf>,

        /// Mark the version as a prerelease
        #[arg(long)]
        prerelease: bool,

        /// Check everything and upload nothing
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove a tool from the current project
    Remove {
        /// Name of the tool
        name: String,

        /// Remove from the global tools file instead of this project
        #[arg(short, long)]
        global: bool,
    },

    /// Update a tool
    Update,

    /// List installed and pinned tools
    List {
        /// Print the list as one JSON document on stdout
        #[arg(long)]
        json: bool,
    },

    /// Delete a tool's binaries from your system, its manifest pins remain
    Delete {
        /// Name of the tool, e.g. owner/repo
        name: String,

        /// Specific version to delete
        #[arg(short, long)]
        version: Option<String>,
    },

    /// Manage the stored tool binaries under ~/.ember/tools
    #[command(subcommand)]
    Cache(ToolCacheCommand),
}

#[derive(Subcommand, Debug)]
pub enum ToolCacheCommand {
    /// Delete every stored tool binary, manifest pins and shims remain
    Clean,
}

pub fn run(command: ToolCommand) -> Result<(), Error> {
    match command {
        ToolCommand::Add {
            name,
            version,
            global,
            index,
        } => add(name, version, global, index),
        ToolCommand::Publish {
            files,
            manifest,
            version,
            notes,
            prerelease,
            dry_run,
        } => {
            let plan =
                tools::ember::plan(&manifest, &files, version, notes.as_deref(), prerelease)?;
            publish(&plan, dry_run)
        }
        ToolCommand::Remove { name, global } => remove(name, global),
        ToolCommand::Update => update(),
        ToolCommand::Delete { name, version } => delete(name, version),
        ToolCommand::List { json } => list(json),
        ToolCommand::Cache(ToolCacheCommand::Clean) => cache_clean(),
    }
}

/** wipes the tool store, every version of every tool, plus the `.latest`
stamps `embr execute` keeps beside them. shims, manifest pins, and the
global tools file survive. a shim without storage errors with "run
`embr install`", and that or the next embx run is the repair path. */
fn cache_clean() -> Result<(), Error> {
    let (freed, skipped) = sweep_tool_store(&paths::tools_dir()?);

    /* a tool that is running right now can't be deleted on windows, say
    which ones were left rather than aborting the sweep mid-way */
    for (name, error) in &skipped {
        eprintln!("warning: skipped {name} (still running?): {error}");
    }

    if freed == 0 && skipped.is_empty() {
        println!("Nothing stored");
        return Ok(());
    }

    ui::print_success(&format!(
        "Removed stored tool binaries ({:.1} MiB)",
        freed as f64 / (1024.0 * 1024.0)
    ));
    println!(
        "Pinned tools reinstall with `embr install` in each project; embx runs re-download on demand"
    );
    Ok(())
}

/** removes every entry of the tool store one by one, so a single locked
binary only survives the sweep instead of aborting it. returns the bytes
freed and the entries that would not go. */
fn sweep_tool_store(tools_dir: &std::path::Path) -> (u64, Vec<(String, std::io::Error)>) {
    let Ok(entries) = fs::read_dir(tools_dir) else {
        return (0, Vec::new());
    };

    let mut freed = 0;
    let mut skipped = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let (size, removed) = if path.is_dir() {
            (
                crate::commands::cache::dir_size(&path),
                fs::remove_dir_all(&path),
            )
        } else {
            (
                entry.metadata().map_or(0, |meta| meta.len()),
                fs::remove_file(&path),
            )
        };
        match removed {
            Ok(()) => freed += size,
            Err(error) => skipped.push((entry.file_name().to_string_lossy().into_owned(), error)),
        }
    }

    // drop the now-empty root too, harmless to leave if something survived
    let _ = fs::remove_dir(tools_dir);
    (freed, skipped)
}

fn add(
    name: String,
    version: Option<String>,
    global: bool,
    index: Option<ToolIndex>,
) -> Result<(), Error> {
    let github = GithubAPI::new();
    let shorthand = tools::shorthand_repository(&name);
    let from_shorthand = shorthand.is_some();
    let name = shorthand.unwrap_or(name);
    if let Some(spec) = name.strip_prefix(NPM_PREFIX) {
        return add_npm(spec, version, global);
    }
    if tools::shorthands::is_name(&name) {
        return Err(Error::UnknownToolShorthand(name));
    }

    // resolve the release first so a bad name/version never touches the file
    let version = version
        .as_deref()
        .map(|version| version.trim_start_matches('v'));
    let (repository, release) = locate(
        &name,
        version,
        index,
        from_shorthand,
        tools::ember::release,
        |repository, version| match version {
            Some(version) => github.get_release(repository, version),
            None => github.get_latest_release(repository),
        },
    )?;

    /* the repo short name becomes the alias key and its bin shim, a repo
    named embr or embx would shadow embr's own binaries there */
    let repo = repository
        .rsplit_once('/')
        .map_or(repository.as_str(), |(_, repo)| repo);
    if tools::is_reserved_alias(repo) {
        return Err(Error::ReservedToolAlias(repo.to_string()));
    }

    /* global tools live in ~/.ember/tools.toml, created on first use, and
    resolve in any directory. project tools only inside their project */
    let mut manifest = ManifestDoc::open_or_create(Scope::from_global(global))?;

    /* alias key defaults to the repo short name. the bin shim takes the
    same name, which is what `delete` relies on when removing it */
    let version = release.tag_name.trim_start_matches('v');
    manifest.table_or_create("tools")?[repo] = toml_edit::value(format!("{repository}@{version}"));
    manifest.save()?;

    if global {
        ui::print_success(&format!("Added global tool {repository}@{version}"));
    } else {
        ui::print_success(&format!("Added tool {repository}@{version} to ember.toml"));
    }
    println!("Run `embr install` to install it");

    Ok(())
}

/** the repository that `name` pins under, and its release. `ember:` asks
Ember. a bare owner/repo asks Ember first and GitHub when Ember has no such
tool, unless `index` names one index. a shorthand's bare target stays on
GitHub, as it was before Ember listed tools. */
fn locate(
    name: &str,
    version: Option<&str>,
    index: Option<ToolIndex>,
    from_shorthand: bool,
    ember: impl Fn(&str, Option<&str>) -> Result<Release, Error>,
    github: impl Fn(&str, Option<&str>) -> Result<Release, Error>,
) -> Result<(String, Release), Error> {
    if let Some(spec) = name.strip_prefix(EMBER_PREFIX) {
        let (spec, pinned) = match spec.split_once('@') {
            Some((spec, pinned)) => (spec, Some(pinned)),
            None => (spec, None),
        };
        let release = ember(spec, version.or(pinned))?;
        return Ok((format!("{EMBER_PREFIX}{spec}"), release));
    }
    Tool::split_repository(name)?;

    // JohnnyMorganz/StyLua finds the pin johnnymorganz/stylua
    let lower = name.to_ascii_lowercase();
    let on_ember = format!("{EMBER_PREFIX}{lower}");
    let ember_first = match index {
        Some(ToolIndex::Ember) => return Ok((on_ember, ember(&lower, version)?)),
        Some(ToolIndex::Github) => false,
        None => !from_shorthand && tools::ember_candidate(name).is_some(),
    };
    if ember_first {
        match ember(&lower, version) {
            Err(Error::EmberToolMissing(_)) => {}
            found => return Ok((on_ember, found?)),
        }
    }
    Ok((name.to_string(), github(name, version)?))
}

/** uploads a checked version to Ember with the `embr publish` login, or
with `dry_run` stops after the checks. */
fn publish(plan: &tools::ember::Plan, dry_run: bool) -> Result<(), Error> {
    println!(
        "Publishing {}@{} with {} file{}",
        plan.name,
        plan.version,
        plan.uploads.len(),
        ui::plural(plan.uploads.len())
    );
    if dry_run {
        for upload in &plan.uploads {
            println!(
                "  {} ({:.1} MiB)",
                upload.name,
                upload.size as f64 / (1024.0 * 1024.0)
            );
        }
        ui::print_success("Dry run: every check passed, nothing uploaded");
        return Ok(());
    }
    crate::commands::yank::as_owner(|token| {
        tools::ember::publish(&registry::api_url(), token, plan)
    })?;
    ui::print_success(&format!("Published tool {}@{}", plan.name, plan.version));
    println!("https://emberpm.dev/tool/{}", plan.name);
    Ok(())
}

/** pins a tool that npm publishes, `npm:package` with an optional
`@version`. the alias is the package's command, so `npm:azul-sync` pins as
`azul`. npm answers which version and command, nothing is installed. */
fn add_npm(spec: &str, version: Option<String>, global: bool) -> Result<(), Error> {
    let invalid = || Error::InvalidToolSpec(format!("{NPM_PREFIX}{spec}"));
    let (package, pinned) = split_npm_spec(spec).ok_or_else(invalid)?;
    let wanted = version
        .as_deref()
        .or(pinned)
        .filter(|wanted| !wanted.is_empty());
    let (version, alias) = tools::npm::resolve(package, wanted)?;
    if tools::is_reserved_alias(&alias) {
        return Err(Error::ReservedToolAlias(alias));
    }

    let tool = format!("{NPM_PREFIX}{package}@{version}");
    let mut manifest = ManifestDoc::open_or_create(Scope::from_global(global))?;
    manifest.table_or_create("tools")?[alias.as_str()] = toml_edit::value(&tool);
    manifest.save()?;

    if global {
        ui::print_success(&format!("Added global tool {tool} as `{alias}`"));
    } else {
        ui::print_success(&format!("Added tool {tool} as `{alias}` to ember.toml"));
    }
    println!("Run `embr install` to install it");
    Ok(())
}

fn remove(name: String, global: bool) -> Result<(), Error> {
    let scope = Scope::from_global(global);

    // no global tools file at all means nothing was ever pinned globally
    let mut manifest = match ManifestDoc::open(scope) {
        Err(Error::ManifestMissing) if global => return Err(Error::ToolMissing(name)),
        opened => opened?,
    };
    let table = manifest.require_table("tools")?;

    /* try the exact alias key first. keys are short names, so a shorthand
    or full "owner/repo" falls back to matching table values by repository */
    let mut removed = table.remove(&name).is_some();
    if !removed {
        let repository = tools::shorthand_repository(&name).unwrap_or_else(|| name.clone());
        let prefix = format!("{repository}@");

        // github repo names are case-insensitive, so match ignoring case
        let key = table.iter().find_map(|(key, item)| {
            item.as_str()
                .and_then(|spec| spec.trim().get(..prefix.len()))
                .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
                .then(|| key.to_string())
        });

        if let Some(key) = key {
            removed = table.remove(&key).is_some();
        }
    }

    if !removed {
        return Err(Error::ToolMissing(name));
    }

    manifest.drop_if_empty("tools");
    manifest.save()?;
    ui::print_success(&format!(
        "Successfully removed tool {name} from {}",
        scope.label()
    ));

    Ok(())
}

/** true when `latest` is newer than `current`. compared as semver when
possible, otherwise any plain string difference counts as outdated. */
fn is_outdated(current: &str, latest: &str) -> bool {
    match (Version::parse(current), Version::parse(latest)) {
        (Ok(current), Ok(latest)) => latest > current,
        _ => current != latest,
    }
}

fn update() -> Result<(), Error> {
    let github = GithubAPI::new();
    let mut manifest = ManifestDoc::open(Scope::Project)?;

    let Some(table) = manifest.table("tools")? else {
        println!("No tools to update");
        return Ok(());
    };

    // snapshot the entries up front so the table can be edited while looping
    let entries = table
        .iter()
        .filter(|(alias, item)| {
            // the version of an inherited tool is in the workspace root
            let inherited = item.get("workspace").and_then(|value| value.as_bool()) == Some(true);
            if inherited {
                println!("Skipping {alias}: it comes from the workspace root");
            }
            !inherited
        })
        .map(|(alias, item)| {
            item.as_str()
                .map(|spec| (alias.to_string(), spec.to_string()))
                .ok_or_else(|| {
                    Error::ManifestInvalid(format!("[tools] entry '{alias}' is not a string"))
                })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    if entries.is_empty() {
        println!("No tools to update");
        return Ok(());
    }

    let mut updated = 0;
    for (alias, spec) in entries {
        let tool = Tool::parse(&spec)?;

        let latest = match tool.npm_package() {
            Some(package) => tools::npm::latest(package)?,
            None => tools::get_latest_release(&tool.repository, &github)?
                .tag_name
                .trim_start_matches('v')
                .to_string(),
        };
        let latest = latest.as_str();

        if !is_outdated(&tool.version, latest) {
            continue;
        }

        table[alias.as_str()] = toml_edit::value(format!("{}@{latest}", tool.repository));
        println!("{}: {} -> {latest}", tool.repository, tool.version);
        updated += 1;
    }

    if updated == 0 {
        println!("All tools are up to date");
        return Ok(());
    }

    manifest.save()?;
    ui::print_success(&format!(
        "Updated {updated} tool{} in ember.toml",
        if updated == 1 { "" } else { "s" }
    ));
    println!("Run `embr install` to install the updated tools");

    Ok(())
}

fn list(json: bool) -> Result<(), Error> {
    /* everything known about one version of a tool, whether its binaries
    are stored and which manifests pin it. pins without storage still show
    up as "not installed" so a fresh `tool add` never looks like a ghost */
    #[derive(Default)]
    struct VersionState {
        installed: bool,
        project: bool,
        global: bool,
    }

    /* keyed by lowercased "owner/repo", manifests may spell the repository
    with different casing than the storage folder was created with */
    type Seen = BTreeMap<String, (String, BTreeMap<String, VersionState>)>;
    fn state_of<'a>(seen: &'a mut Seen, name: &str, version: String) -> &'a mut VersionState {
        let entry = seen
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| (name.to_string(), BTreeMap::new()));
        entry.1.entry(version).or_default()
    }

    let mut seen: Seen = BTreeMap::new();

    // what's stored on disk
    let tools_dir = paths::tools_dir()?;
    let mut stored = Vec::new();
    if tools_dir.is_dir() {
        for entry in fs::read_dir(&tools_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            /* github owners can never contain '_', so the first '_' in the
            directory name splits owner from repo */
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            // npm and Ember tools sit one level down, a '+' in place of the '/'
            let prefix = match dir_name.as_str() {
                "npm" => Some(NPM_PREFIX),
                "ember" => Some(EMBER_PREFIX),
                _ => None,
            };
            if let Some(prefix) = prefix {
                for package in fs::read_dir(entry.path())? {
                    let package = package?;
                    let name = package.file_name().to_string_lossy().replace('+', "/");
                    stored.push((format!("{prefix}{name}"), package.path()));
                }
                continue;
            }
            let Some((owner, repo)) = dir_name.split_once('_') else {
                continue;
            };
            stored.push((format!("{owner}/{repo}"), entry.path()));
        }
        for (name, path) in stored {
            if !path.is_dir() {
                continue;
            }
            for version in fs::read_dir(path)? {
                let version = version?;
                if version.path().is_dir() {
                    let version = version.file_name().to_string_lossy().into_owned();
                    state_of(&mut seen, &name, version).installed = true;
                }
            }
        }
    }

    // what the surrounding project and the global tools file pin
    let project = tools::shim::project_tools(&std::env::current_dir()?)?;
    let global = tools::shim::global_tools()?;
    for (tools, is_global) in [(&project, false), (&global, true)] {
        for tool in tools.values() {
            let state = state_of(&mut seen, &tool.repository, tool.version.clone());
            if is_global {
                state.global = true;
            } else {
                state.project = true;
            }
        }
    }

    let tools = seen.into_values().map(|(name, versions)| {
        let mut versions: Vec<_> = versions.into_iter().collect();
        // order versions semver-aware where possible
        versions.sort_by(
            |(a, _), (b, _)| match (Version::parse(a), Version::parse(b)) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                _ => a.cmp(b),
            },
        );
        (name, versions)
    });

    if json {
        let tools: Vec<serde_json::Value> = tools
            .map(|(name, versions)| {
                let versions: Vec<serde_json::Value> = versions
                    .into_iter()
                    .map(|(version, state)| {
                        serde_json::json!({
                            "version": version,
                            "installed": state.installed,
                            "project": state.project,
                            "global": state.global,
                        })
                    })
                    .collect();
                serde_json::json!({ "name": name, "versions": versions })
            })
            .collect();
        return ui::print_json(&serde_json::json!({
            "version": ui::JSON_VERSION,
            "tools": tools,
        }));
    }

    let mut tools = tools.peekable();
    if tools.peek().is_none() {
        println!("No tools installed or pinned");
        return Ok(());
    }

    /* one accent check line, "name@version", per tool, matching install's
    output. multiple versions share the line, each annotated with its scopes */
    for (name, versions) in tools {
        let versions = versions
            .into_iter()
            .map(|(version, state)| {
                let mut notes = Vec::new();
                if state.project {
                    notes.push("project");
                }
                if state.global {
                    notes.push("global");
                }
                if !state.installed {
                    notes.push("not installed");
                }
                if notes.is_empty() {
                    version
                } else {
                    format!("{version} ({})", notes.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        ui::print_success(&format!("{name}@{versions}"));
    }

    Ok(())
}

fn delete(name: String, version: Option<String>) -> Result<(), Error> {
    let name = tools::expand_shorthand(&name);
    /* the shim of a GitHub tool is named after its repo. an npm tool's shim
    is named after its command, which only its package.json knows, so that
    shim stays and says "run `embr install`" until one runs */
    let (tool_dir, shim) = match name.strip_prefix(NPM_PREFIX) {
        Some(spec) => match split_npm_spec(spec) {
            Some((package, None)) => (tools::npm::package_root(package)?, None),
            _ => return Err(Error::InvalidToolSpec(name.clone())),
        },
        None => {
            let repo = match name.strip_prefix(EMBER_PREFIX) {
                Some(ember) => {
                    split_package_name(ember)
                        .map_err(|_| Error::InvalidToolSpec(name.clone()))?
                        .1
                }
                None => Tool::split_repository(&name)?.1,
            };
            (tools::tool_root(&name)?, Some(repo.to_string()))
        }
    };
    let version = version
        .as_deref()
        .map(|version| version.trim_start_matches('v'));

    // with --version only remove that version's subdir, otherwise the tool
    let target = match version {
        Some(version) => tool_dir.join(version),
        None => tool_dir.clone(),
    };
    if !target.exists() {
        return Err(Error::ToolMissing(name.to_string()));
    }
    fs::remove_dir_all(&target)?;

    /* when the whole tool or its last version is gone, drop the shim too.
    manifest pins stay, `tool list` shows it as "not installed" and the
    next `embr install` puts it back. only version directories count.
    the `.latest` stamp `embr execute` leaves beside them must not keep a
    deleted tool half-alive, and gets swept along with the dir. */
    let tool_gone = version.is_none()
        || fs::read_dir(&tool_dir)
            .map(|mut dir| !dir.any(|entry| entry.is_ok_and(|entry| entry.path().is_dir())))
            .unwrap_or(true);
    if tool_gone {
        let _ = fs::remove_dir_all(&tool_dir);
        if let (Some(shim), Ok(bin_dir)) = (shim, paths::bin_dir()) {
            let _ = fs::remove_file(bin_dir.join(format!("{shim}{EXE_SUFFIX}")));
        }
    }

    let message = match version {
        Some(version) => format!("Deleted tool {name}@{version}"),
        None => format!("Deleted tool {name}"),
    };
    ui::print_success(&message);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn release(version: &str) -> Release {
        Release {
            tag_name: version.to_string(),
            assets: Vec::new(),
            draft: false,
        }
    }

    /** `locate` with an Ember that holds `on_ember` and a GitHub that holds
    everything. gives what it found, and every ask as "index name version". */
    fn locate_in(
        on_ember: &[&str],
        name: &str,
        version: Option<&str>,
        index: Option<ToolIndex>,
        from_shorthand: bool,
    ) -> (Result<String, String>, Vec<String>) {
        let asked = RefCell::new(Vec::new());
        let ask = |index: &str, name: &str, version: Option<&str>| {
            asked
                .borrow_mut()
                .push(format!("{index} {name} {}", version.unwrap_or("latest")));
        };
        let found = locate(
            name,
            version,
            index,
            from_shorthand,
            |name, version| {
                ask("ember", name, version);
                if on_ember.contains(&name) {
                    Ok(release(version.unwrap_or("2.0.0")))
                } else {
                    Err(Error::EmberToolMissing(name.to_string()))
                }
            },
            |name, version| {
                ask("github", name, version);
                Ok(release(version.unwrap_or("1.0.0")))
            },
        );
        let found = found
            .map(|(repository, release)| format!("{repository}@{}", release.tag_name))
            .map_err(|error| error.to_string());
        (found, asked.into_inner())
    }

    #[test]
    fn a_bare_name_asks_ember_first_then_github() {
        let pins = ["johnnymorganz/stylua"];
        // Ember has it: the manifest gets the ember: form, lowercase
        assert_eq!(
            locate_in(&pins, "JohnnyMorganz/StyLua", None, None, false),
            (
                Ok("ember:johnnymorganz/stylua@2.0.0".to_string()),
                vec!["ember johnnymorganz/stylua latest".to_string()]
            )
        );
        // Ember answers 404: GitHub, as the user wrote it
        assert_eq!(
            locate_in(&pins, "Acme/Tool", Some("1.2.0"), None, false),
            (
                Ok("Acme/Tool@1.2.0".to_string()),
                vec![
                    "ember acme/tool 1.2.0".to_string(),
                    "github Acme/Tool 1.2.0".to_string()
                ]
            )
        );
        // a name that Ember cannot hold goes to GitHub at once
        let (found, asked) = locate_in(&pins, "acme/tool.nvim", None, None, false);
        assert_eq!(found.unwrap(), "acme/tool.nvim@1.0.0");
        assert_eq!(asked, ["github acme/tool.nvim latest"]);
    }

    #[test]
    fn index_forces_one_index_with_no_fallback() {
        let pins = ["johnnymorganz/stylua"];
        let (found, asked) = locate_in(
            &pins,
            "johnnymorganz/stylua",
            None,
            Some(ToolIndex::Github),
            false,
        );
        assert_eq!(found.unwrap(), "johnnymorganz/stylua@1.0.0");
        assert_eq!(asked, ["github johnnymorganz/stylua latest"]);

        let (found, asked) = locate_in(&pins, "acme/tool", None, Some(ToolIndex::Ember), false);
        assert!(matches!(found, Err(ref error) if error.contains("acme/tool")));
        assert_eq!(asked, ["ember acme/tool latest"]);
    }

    #[test]
    fn ember_names_and_shorthand_targets_pick_their_index() {
        // an ember: name, or a shorthand whose target is one, asks only Ember
        let (found, asked) = locate_in(&["acme/lint"], "ember:acme/lint@1.1.0", None, None, true);
        assert_eq!(found.unwrap(), "ember:acme/lint@1.1.0");
        assert_eq!(asked, ["ember acme/lint 1.1.0"]);
        // --version wins over the @version of the name
        let (found, _) = locate_in(
            &["acme/lint"],
            "ember:acme/lint@1.1.0",
            Some("1.2.0"),
            None,
            false,
        );
        assert_eq!(found.unwrap(), "ember:acme/lint@1.2.0");
        // and Ember's 404 is the answer, no GitHub fallback
        let (found, asked) = locate_in(&[], "ember:acme/none", None, None, false);
        assert!(found.is_err());
        assert_eq!(asked, ["ember acme/none latest"]);

        // a bare shorthand target stays GitHub
        let (found, asked) = locate_in(&["rojo-rbx/rojo"], "rojo-rbx/rojo", None, None, true);
        assert_eq!(found.unwrap(), "rojo-rbx/rojo@1.0.0");
        assert_eq!(asked, ["github rojo-rbx/rojo latest"]);
    }

    /** any other Ember answer is not a 404: a tool without the cli tag is
    refused, and GitHub is not asked. */
    #[test]
    fn a_refusal_from_ember_is_no_fallback() {
        let found = locate(
            "acme/plugin",
            None,
            None,
            false,
            |name, _| Err(Error::NotCliTool(name.to_string())),
            |_, _| panic!("GitHub must not be asked"),
        );
        assert!(matches!(found, Err(Error::NotCliTool(_))));
    }

    #[test]
    fn sweeping_the_store_removes_tools_and_stamps() {
        let store = std::env::temp_dir().join("embr-test-tool-store-sweep");
        let _ = fs::remove_dir_all(&store);
        fs::create_dir_all(store.join("acme_tool").join("1.0.0")).unwrap();
        fs::write(
            store.join("acme_tool").join("1.0.0").join("tool"),
            [0u8; 64],
        )
        .unwrap();
        fs::write(store.join("acme_tool").join(".latest"), "1.0.0\n").unwrap();
        fs::write(store.join("stray-file"), [0u8; 8]).unwrap();

        let (freed, skipped) = sweep_tool_store(&store);
        assert_eq!(freed, 64 + 6 + 8);
        assert!(skipped.is_empty());
        assert!(!store.exists());

        // a missing store is a quiet no-op, not an error
        let (freed, skipped) = sweep_tool_store(&store);
        assert_eq!(freed, 0);
        assert!(skipped.is_empty());
    }
}
