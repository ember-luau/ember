use crate::error::Error;
use clap::Subcommand;

// TODO: Implement github utility

#[derive(Subcommand, Debug)]
pub enum ToolCommand {
    /// Use a tool in the current project
    Add,
    /// Remove a tool from the current project
    Remove,
    /// Update a tool
    Update,
    /// Install a tool on your system
    Install,
}

pub fn run(command: ToolCommand) -> Result<(), Error> {
    match command {
        ToolCommand::Add => add(),
        ToolCommand::Remove => remove(),
        ToolCommand::Update => update(),
        ToolCommand::Install => install(),
    }
}

fn add() -> Result<(), Error> {
    Ok(())
}

fn remove() -> Result<(), Error> {
    Ok(())
}

fn update() -> Result<(), Error> {
    Ok(())
}

fn install() -> Result<(), Error> {
    Ok(())
}
