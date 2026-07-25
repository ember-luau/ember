//! Handing off to other programs: tool shims, `lpm run` scripts, Studio. The
//! platform differences (exec vs wait, sh vs cmd) live here instead of being
//! cfg-gated at every call site.

use crate::error::Error;
use std::process::Command;

/// A command that runs `script` through the platform's shell, so manifest
/// scripts can use pipes, `&&` and the rest of the usual shell syntax.
/// Windows gets cmd (what ComSpec points at), everything else /bin/sh.
#[cfg(windows)]
pub fn shell(script: &str) -> Command {
    use std::os::windows::process::CommandExt;

    let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let mut command = Command::new(comspec);
    command.arg("/C");
    // The script goes on the command line verbatim: std would escape inner
    // quotes MSVCRT-style (\"), which cmd does not understand, so a script
    // like `rojo build -o "my game.rbxl"` would arrive mangled.
    command.raw_arg(script);
    command
}

#[cfg(not(windows))]
pub fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    command
}

/// Hands the terminal over to `command` and reports its exit code. On unix
/// this replaces the lpm process outright, so it only ever returns on
/// failure; elsewhere lpm waits around and passes the code back up.
#[cfg(unix)]
pub fn exec(mut command: Command) -> Result<i32, Error> {
    use std::os::unix::process::CommandExt;
    Err(Error::Io(command.exec()))
}

#[cfg(not(unix))]
pub fn exec(mut command: Command) -> Result<i32, Error> {
    Ok(command.status()?.code().unwrap_or(1))
}

/// Runs `command` to completion and reports its exit code. Unlike `exec` this
/// always comes back, so callers keep control after the program is done.
pub fn wait(mut command: Command) -> Result<i32, Error> {
    Ok(command.status()?.code().unwrap_or(1))
}
