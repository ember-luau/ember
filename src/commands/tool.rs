use std::{collections::HashMap, env::consts::EXE_SUFFIX, fs};

use crate::{
    error::Error,
    github::GithubAPI,
    manifest::{MANIFEST_FILE, Tool},
    tools, ui,
};
use clap::Subcommand;
use semver::Version;

#[derive(Subcommand, Debug)]
pub enum ToolCommand {
    /// Use a tool in the current project
    Add {
        /// Name of the tool (e.g. owner/repo)
        name: String,

        /// Specific version to add
        #[arg(short, long)]
        version: Option<String>,
    },

    /// Remove a tool from the current project
    Remove {
        /// Name of the tool
        name: String,
    },

    /// Update a tool
    Update,

    /// List tools installed on your system
    List,

    /// Delete a tool from your system
    Delete {
        /// Name of the tool (e.g. owner/repo)
        name: String,

        /// Specific version to delete
        #[arg(short, long)]
        version: Option<String>,
    },
}

fn shorthand_map() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("darklua", "seaofvoices/darklua"),
        ("rojo", "rojo-rbx/rojo"),
        ("luau-lsp", "johnnymorganz/luau-lsp"),
        ("stylua", "johnnymorganz/stylua"),
    ])
}

/// Splits an "owner/repo" GitHub repository name. Unlike index package names
/// (see `manifest::split_package_name`), GitHub owners and repos may contain
/// uppercase letters and dots (e.g. "JohnnyMorganz/StyLua"), so only the
/// shape is validated: exactly one '/', both halves non-empty.
fn split_repository(name: &str) -> Result<(&str, &str), Error> {
    match name.split_once('/') {
        Some((owner, repo)) if !owner.is_empty() && !repo.is_empty() && !repo.contains('/') => {
            Ok((owner, repo))
        }
        _ => Err(Error::InvalidToolSpec(name.to_string())),
    }
}

pub fn run(command: ToolCommand) -> Result<(), Error> {
    match command {
        ToolCommand::Add { name, version } => add(name, version),
        ToolCommand::Remove { name } => remove(name),
        ToolCommand::Update => update(),
        ToolCommand::Delete { name, version } => delete(name, version),
        ToolCommand::List => list(),
    }
}

fn add(name: String, version: Option<String>) -> Result<(), Error> {
    let github = GithubAPI::new();
    let shorthands = shorthand_map(); // Get the shorthands

    // If the shorthand doesn't exist assume its a longhand and use it as is
    let name = shorthands
        .get(name.as_str())
        .map(|longhand| longhand.to_string())
        .unwrap_or(name);

    // Seperate the author/package string
    let (author, package) = split_repository(&name)?;

    // Resolve the release first so a bad name/version never touches lpm.toml
    let release = match &version {
        Some(version) => github.get_release(&name, version.trim_start_matches('v'))?,
        None => github.get_latest_release(&name)?,
    };

    let mut document: toml_edit::DocumentMut = fs::read_to_string(MANIFEST_FILE)?.parse()?;

    // Get the tools table, if it doesn't exist create one
    let tools = document
        .entry("tools")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));

    // If tools isn't a table then throw
    let Some(table) = tools.as_table_mut() else {
        return Err(Error::ManifestInvalid("[tools] is not a table".to_string()));
    };

    // Get the version name not starting with v
    let version = release.tag_name.trim_start_matches('v');

    // Format the toml package string
    let value = format!("{author}/{package}@{}", &version);
    table[package] = toml_edit::value(value);

    fs::write(MANIFEST_FILE, document.to_string())?;

    ui::print_success(&format!("Added tool {}@{} to lpm.toml", &name, &version));
    println!("Run `lpm install` to install it");

    Ok(())
}

fn remove(name: String) -> Result<(), Error> {
    let mut document: toml_edit::DocumentMut = fs::read_to_string(MANIFEST_FILE)?.parse()?;

    // We can't remove tools if there is no tools table
    let Some(tools) = document.get_mut("tools") else {
        return Err(Error::ManifestInvalid("[tools] doesn't exist".to_string()));
    };

    // If tools isn't a table error
    let Some(table) = tools.as_table_mut() else {
        return Err(Error::ManifestInvalid("[tools] is not a table".to_string()));
    };

    // Try the exact alias key first; keys are short names, so a shorthand or
    // full "owner/repo" falls back to matching table values by repository
    let mut removed = table.remove(&name).is_some();
    if !removed {
        let shorthands = shorthand_map();
        let repository = shorthands.get(name.as_str()).copied().unwrap_or(&name);
        let prefix = format!("{repository}@");

        // GitHub repository names are case-insensitive, so match ignoring case
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

    // If the tool didn't exist then error
    if !removed {
        return Err(Error::ToolMissing(name));
    }

    // Delete tool table if its empty
    if table.is_empty() {
        document.remove("tools");
    }

    fs::write(MANIFEST_FILE, document.to_string())?;
    ui::print_success(&format!(
        "Successfully removed tool {} from lpm.toml",
        &name
    ));

    Ok(())
}

/// True when `latest` is newer than `current`. Both are compared as semver
/// when possible; otherwise any plain string difference counts as outdated.
fn is_outdated(current: &str, latest: &str) -> bool {
    match (Version::parse(current), Version::parse(latest)) {
        (Ok(current), Ok(latest)) => latest > current,
        _ => current != latest,
    }
}

fn update() -> Result<(), Error> {
    let github = GithubAPI::new();
    let mut document: toml_edit::DocumentMut = fs::read_to_string(MANIFEST_FILE)?.parse()?;

    // No tools table means there is simply nothing to update
    let Some(tools) = document.get_mut("tools") else {
        println!("No tools to update");
        return Ok(());
    };

    // If tools isn't a table error
    let Some(table) = tools.as_table_mut() else {
        return Err(Error::ManifestInvalid("[tools] is not a table".to_string()));
    };

    // Snapshot the entries up front so the table can be edited while looping
    let entries = table
        .iter()
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

        let release = github.get_latest_release(&tool.repository)?;
        let latest = release.tag_name.trim_start_matches('v');

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

    fs::write(MANIFEST_FILE, document.to_string())?;
    ui::print_success(&format!(
        "Updated {updated} tool{} in lpm.toml",
        if updated == 1 { "" } else { "s" }
    ));
    println!("Run `lpm install` to install the updated tools");

    Ok(())
}

fn list() -> Result<(), Error> {
    let tools_dir = tools::tools_dir()?;

    // Collect installed tools as (owner/repo, sorted versions)
    let mut installed: Vec<(String, Vec<String>)> = Vec::new();
    if tools_dir.is_dir() {
        for entry in fs::read_dir(&tools_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            // GitHub owners can never contain '_', so the first '_' in the
            // directory name splits owner from repo
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let Some((owner, repo)) = dir_name.split_once('_') else {
                continue;
            };

            let mut versions: Vec<String> = fs::read_dir(entry.path())?
                .filter_map(|version| version.ok())
                .filter(|version| version.path().is_dir())
                .map(|version| version.file_name().to_string_lossy().into_owned())
                .collect();

            // Order versions semver-aware where possible
            versions.sort_by(|a, b| match (Version::parse(a), Version::parse(b)) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                _ => a.cmp(b),
            });

            installed.push((format!("{owner}/{repo}"), versions));
        }
    }

    if installed.is_empty() {
        println!("No tools installed");
        return Ok(());
    }

    // One accent "✓ name@version" line per tool, matching install's output;
    // multiple installed versions share the line
    installed.sort();
    for (name, versions) in installed {
        if versions.is_empty() {
            ui::print_success(&name);
        } else {
            ui::print_success(&format!("{name}@{}", versions.join(", ")));
        }
    }

    Ok(())
}

fn delete(name: String, version: Option<String>) -> Result<(), Error> {
    let shorthands = shorthand_map(); // Get the shorthands

    // If the shorthand doesn't exist assume its a longhand and use it as is
    let name = shorthands
        .get(name.as_str())
        .map(|longhand| longhand.to_string())
        .unwrap_or(name);

    let (owner, repo) = split_repository(&name)?;
    let tool_dir = tools::tools_dir()?.join(format!("{owner}_{repo}"));

    // With --version only remove that version's subdir, otherwise the tool
    let target = match &version {
        Some(version) => tool_dir.join(version.trim_start_matches('v')),
        None => tool_dir.clone(),
    };

    if !target.exists() {
        return Err(Error::ToolMissing(name));
    }
    fs::remove_dir_all(&target)?;

    // When the whole tool (or its last version) is gone, drop the shim too.
    // Best-effort: the shim name may differ if the user aliased the tool
    let tool_gone = version.is_none()
        || fs::read_dir(&tool_dir)
            .map(|mut dir| dir.next().is_none())
            .unwrap_or(true);
    if tool_gone {
        let _ = fs::remove_dir(&tool_dir);
        if let Ok(bin_dir) = tools::bin_dir() {
            let _ = fs::remove_file(bin_dir.join(format!("{repo}{EXE_SUFFIX}")));
        }
    }

    let message = match &version {
        Some(version) => format!("Deleted tool {}@{}", &name, version.trim_start_matches('v')),
        None => format!("Deleted tool {}", &name),
    };
    ui::print_success(&message);

    Ok(())
}
