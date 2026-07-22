use std::{collections::HashMap, fs};

use crate::{
    error::Error,
    github::GithubAPI,
    manifest::{MANIFEST_FILE, split_package_name},
    ui,
};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum ToolCommand {
    /// Use a tool in the current project
    Add {
        /// Name of the tool (e.g. owner/repo)
        name: String,

        /// Specific version to install
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

pub fn run(command: ToolCommand) -> Result<(), Error> {
    match command {
        ToolCommand::Add { name, version } => add(name, version),
        ToolCommand::Remove { name } => remove(name),
        ToolCommand::Update => update(),
        ToolCommand::Delete { name, version } => todo!(),
        ToolCommand::List => todo!(),
    }
}

fn add(name: String, _version: Option<String>) -> Result<(), Error> {
    let github = GithubAPI::new();
    let shorthands = shorthand_map(); // Get the shorthands

    // If the shorthand doesn't exist assume its a longhand and use it as is
    let name = shorthands
        .get(name.as_str())
        .map(|longhand| longhand.to_string())
        .unwrap_or(name);

    // Seperate the author/package string
    let (author, package) = split_package_name(&name)?;
    let release = github.get_latest_release(&name)?; // Request for the latest release info

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
    table[package] = toml_edit::value(value.clone());

    fs::write(MANIFEST_FILE, document.to_string())?;
    ui::print_success(&format!(
        "Successfully added tool {}@{} to lpm.toml",
        &name, &version
    ));

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

    // If the tool didn't exist then error
    if table.remove(&name).is_none() {
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

fn update() -> Result<(), Error> {
    Ok(())
}
