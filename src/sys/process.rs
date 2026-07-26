/*! Handing off to other programs: tool shims, `lpm run` scripts, Studio.
the platform differences (exec vs wait, sh vs cmd) live here instead of
being cfg-gated at every call site. */

use crate::error::Error;
use std::process::Command;

/** A command that runs `script` through the platform's shell, so manifest
scripts get pipes, `&&` and the rest. windows gets cmd (whatever ComSpec
points at), everything else /bin/sh. */
#[cfg(windows)]
pub fn shell(script: &str) -> Command {
    use std::os::windows::process::CommandExt;

    let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let mut command = Command::new(comspec);
    command.arg("/C");
    /* the script goes on the command line verbatim: through arg() std would
    escape inner quotes MSVCRT-style (\"), which cmd doesn't understand,
    so something like `rojo build -o "my game.rbxl"` arrives mangled. */
    command.raw_arg(script);
    command
}

#[cfg(not(windows))]
pub fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    command
}

/** Hands the terminal over to `command`. on unix this replaces the lpm
process outright, so it only ever returns on failure; elsewhere lpm
waits and passes the exit code back up. */
#[cfg(unix)]
pub fn exec(mut command: Command) -> Result<i32, Error> {
    use std::os::unix::process::CommandExt;
    Err(Error::Io(command.exec()))
}

#[cfg(not(unix))]
pub fn exec(mut command: Command) -> Result<i32, Error> {
    Ok(command.status()?.code().unwrap_or(1))
}

/** Runs `command` to completion and reports its exit code. unlike `exec`
this always comes back, so the caller keeps control afterwards. */
pub fn wait(mut command: Command) -> Result<i32, Error> {
    Ok(command.status()?.code().unwrap_or(1))
}
