use crate::{error::Error, sys::process};
use std::process::Command;

pub fn run() -> Result<(), Error> {
    let mut command = studio_command();

    // Windows just pokes the roblox-studio: protocol handler and is done with
    // it; elsewhere we exec so the launcher keeps our terminal.
    if cfg!(windows) {
        command.spawn()?;
        Ok(())
    } else {
        process::exec(command).map(|_| ())
    }
}

#[cfg(windows)]
fn studio_command() -> Command {
    Command::new("roblox-studio:")
}

#[cfg(target_os = "macos")]
fn studio_command() -> Command {
    let mut command = Command::new("open");
    command.args(["-a", "RobloxStudio"]);
    command
}

/// Linux has no Studio build; Vinegar is the usual wrapper for it.
#[cfg(all(unix, not(target_os = "macos")))]
fn studio_command() -> Command {
    let mut command = Command::new("flatpak");
    command.args(["run", "org.vinegarhq.Vinegar"]);
    command
}
