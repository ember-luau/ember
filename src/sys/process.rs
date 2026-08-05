/*! Handing off to other programs, tool shims, `lpm run` scripts, Studio.
the platform differences like exec vs wait and sh vs cmd live here
instead of being cfg-gated at every call site. */

use crate::error::Error;
use std::io::{BufRead, BufReader, IsTerminal, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/** A command that runs `script` through the platform's shell, so manifest
scripts get pipes, `&&` and the rest. windows gets cmd, whatever ComSpec
points at, everything else /bin/sh. */
#[cfg(windows)]
pub fn shell(script: &str) -> Command {
    use std::os::windows::process::CommandExt;

    let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let mut command = Command::new(comspec);
    command.arg("/C");
    /* the script goes on the command line verbatim. through arg() std would
    escape inner quotes MSVCRT-style as \", which cmd doesn't understand,
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
process outright, so it only ever returns on failure. elsewhere lpm
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

/** Runs a `[scripts]` entry and reports the exit code to answer with.

One command runs straight through on lpm's own stdio, which is what keeps a
TTY a TTY. Several run at once with their output tagged, which cannot. The
split is here rather than at the call sites so `lpm run`, the shortcuts and
the hooks all get the same behavior from the same place. */
pub fn script(commands: &[String]) -> Result<i32, Error> {
    match commands {
        [single] => wait(shell(single)),
        many => concurrent(many),
    }
}

/// how often the wait loop checks whether a child has exited.
const POLL: Duration = Duration::from_millis(40);

/** Runs every command at once, tagging each line of output with the
command's 1-based position in the list.

Tagging means lpm has to read the output rather than let it through, and
what the children are given to write to is the whole design question. See
`spawn_tagged`: a pseudo-terminal on unix, so they behave exactly as they
would alone and keep their colour, at the price of stdout and stderr
arriving merged; plain pipes on Windows, where the two stay separate and
colour depends on the tool honouring FORCE_COLOR. Either way a single
command still takes the inherit path above rather than being treated as a
list of one, and stdin is left inherited.

Ordering: lines are whole and never interleave mid-line, because every
writer goes through one lock. Between commands they arrive as they are
produced, which is the useful order for watching two servers come up.

Every command runs to its own end; one failing does not stop the others,
which is `concurrently`'s default and the only thing lpm can promise
honestly. Killing the survivors would have to kill process *groups* -- a
shell running `a; b` does not exec-replace itself, so signalling the child
lpm spawned leaves the actual program orphaned and still holding the pipe.
Ctrl+C is unaffected and does stop everything, since the whole tree is in
lpm's own process group. The code reported is the first non-zero one. */
fn concurrent(commands: &[String]) -> Result<i32, Error> {
    /* whether the children should be told to colour at all. the unix path
    hands them a terminal, which is signal enough on its own, but this still
    gates it: with lpm's own output redirected, `lpm serve > log` should get
    clean text rather than a file full of escapes. NO_COLOR wins outright. */
    let force_colour = std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());

    /* output is captured rather than inherited, so the lines can be read and
    tagged. stdin is left alone: it is one terminal shared by everyone, and
    handing it to several children is not a thing that can be divided
    sensibly. see `spawn_tagged` for what "captured" costs on each platform. */
    let mut children: Vec<Child> = Vec::with_capacity(commands.len());
    let mut pending: Vec<Vec<(Box<dyn Read + Send>, bool)>> = Vec::new();
    for command in commands {
        let tagged = spawn_tagged(command, force_colour)?;
        children.push(tagged.child);
        pending.push(tagged.streams);
    }

    /* the last tag written, under one lock. it does two jobs: a pump holds
    it for exactly one line, so a line from [1] can never land inside a line
    from [2], and comparing against it is what puts a blank line at each
    switch between commands. */
    let last = Arc::new(Mutex::new(None));
    // kept per child, so a command's output is all out before it is called done
    let mut pumps: Vec<Vec<JoinHandle<()>>> = Vec::new();
    for (index, streams) in pending.into_iter().enumerate() {
        let tag = index + 1;
        let mut mine = Vec::new();
        for (stream, is_stderr) in streams {
            let last = Arc::clone(&last);
            mine.push(thread::spawn(move || pump(stream, tag, is_stderr, &last)));
        }
        pumps.push(mine);
    }

    /* polled rather than a waiter thread per child: `Child::wait` wants the
    handle exclusively, so the one place that owns them does the waiting,
    and polling reports each exit near when it happens rather than in list
    order. */
    let mut done = vec![false; children.len()];
    let mut failure: Option<i32> = None;
    loop {
        let mut running = 0;
        for (index, child) in children.iter_mut().enumerate() {
            if done[index] {
                continue;
            }
            match child.try_wait()? {
                Some(status) => {
                    done[index] = true;
                    /* its pipes are closed now, so these return at once.
                    joining first is what keeps "exited" after the last line
                    that command printed rather than racing it. */
                    for pump in std::mem::take(&mut pumps[index]) {
                        let _ = pump.join();
                    }

                    let code = status.code().unwrap_or(1);
                    if code != 0 {
                        emit(&last, index + 1, |tag, separate| {
                            crate::ui::print_tagged_exit(tag, code, separate)
                        });
                        failure.get_or_insert(code);
                    }
                }
                None => running += 1,
            }
        }
        if running == 0 {
            break;
        }
        thread::sleep(POLL);
    }

    // nothing is left half-printed when the caller moves on
    for group in pumps {
        for pump in group {
            let _ = pump.join();
        }
    }

    Ok(failure.unwrap_or(0))
}

/** Writes one tagged thing, blank-separated from whatever came before it if
that was a different command. Holds the lock across the whole write, which is
what keeps lines whole. */
fn emit(last: &Mutex<Option<usize>>, tag: usize, write: impl FnOnce(usize, bool)) {
    // a poisoned lock means some pump panicked; the output still has to come out
    let mut last = last.lock().unwrap_or_else(|poison| poison.into_inner());
    write(tag, last.is_some_and(|previous| previous != tag));
    *last = Some(tag);
}

/// A spawned command and the streams whose lines carry its tag.
struct Tagged {
    child: Child,
    /// (stream, is_stderr). one entry on unix, where a terminal merges the two.
    streams: Vec<(Box<dyn Read + Send>, bool)>,
}

/** Spawns one command of a concurrent script onto a pseudo-terminal.

A pipe is what makes the tagging possible and also what makes half the
ecosystem turn colour off: `ls --color=auto`, and plenty of Rust tools,
decide by asking `isatty` and nothing else. No environment variable reaches
those -- FORCE_COLOR and CLICOLOR_FORCE are conventions each tool opts into,
and a tool that never looks at them stays grey. A pty is the only answer
that works without the tool's cooperation: the child gets a real terminal,
behaves exactly as it would run alone, and lpm reads the other end.

The cost is that a terminal has one stream. stdout and stderr arrive
merged, the same way they would on your screen, so a concurrent script
cannot redirect one without the other. That is why a single-command script
still takes the inherit path and never comes through here.

`terminal` is false when lpm's own output is not one, and then this falls
back to pipes -- deliberately. A pty would make every tool colour into a
redirect that nobody is watching, so `lpm serve > log` would fill the file
with escapes. Pipes give that case what it wants: clean text, and stdout
and stderr kept apart. */
#[cfg(unix)]
fn spawn_tagged(command: &str, terminal: bool) -> Result<Tagged, Error> {
    use std::os::fd::{FromRawFd, RawFd};

    if !terminal {
        return spawn_piped(command, false);
    }

    /* the child inherits this as its terminal size, so anything that wraps
    or draws a bar lines up with the real window rather than an 80x24 guess */
    let (columns, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let (mut controller, mut device): (RawFd, RawFd) = (-1, -1);
    // SAFETY: both fds are out params, and the two pointers below are read-only
    let opened = unsafe {
        libc::openpty(
            &mut controller,
            &mut device,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    };
    if opened != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }

    /* SAFETY: `device` is open and owned here. each dup is handed to Stdio,
    which closes it once spawn has copied it into the child. */
    let (out, err) = unsafe {
        (
            Stdio::from_raw_fd(libc::dup(device)),
            Stdio::from_raw_fd(libc::dup(device)),
        )
    };

    let mut spawner = shell(command);
    spawner.stdout(out).stderr(err);
    /* the terminal is already the strongest signal. these are for the tools
    that ask the variable instead of the fd, and cost nothing to add */
    spawner.env("FORCE_COLOR", "1").env("CLICOLOR_FORCE", "1");
    let child = spawner.spawn();

    /* our own copy goes either way. while any fd on this side stays open the
    reader below would never see EOF, because the terminal would still have a
    writer -- us. */
    // SAFETY: `device` is still open and not owned by anything else.
    unsafe { libc::close(device) };

    let child = match child {
        Ok(child) => child,
        Err(error) => {
            // SAFETY: opened above, never handed out on this path.
            unsafe { libc::close(controller) };
            return Err(error.into());
        }
    };

    // SAFETY: `controller` is open and ownership moves into the File.
    let stream = unsafe { std::fs::File::from_raw_fd(controller) };
    Ok(Tagged {
        child,
        // one merged stream, so everything is reported as stdout
        streams: vec![(Box::new(stream), false)],
    })
}

/** Spawns one command of a concurrent script onto pipes. The whole story on
Windows, where there is no ConPTY plumbing here, and the redirected case on
unix.

Colour then depends on the tool honouring FORCE_COLOR or CLICOLOR_FORCE,
which the ones built on the clap/anstream stack do and the ones asking
`isatty` do not. In exchange stdout and stderr stay separate, which the pty
path gives up. */
#[cfg(not(unix))]
fn spawn_tagged(command: &str, terminal: bool) -> Result<Tagged, Error> {
    spawn_piped(command, terminal)
}

fn spawn_piped(command: &str, force_colour: bool) -> Result<Tagged, Error> {
    let mut spawner = shell(command);
    spawner.stdout(Stdio::piped()).stderr(Stdio::piped());
    if force_colour {
        spawner.env("FORCE_COLOR", "1").env("CLICOLOR_FORCE", "1");
    }
    let mut child = spawner.spawn()?;

    let mut streams: Vec<(Box<dyn Read + Send>, bool)> = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        streams.push((Box::new(stdout), false));
    }
    if let Some(stderr) = child.stderr.take() {
        streams.push((Box::new(stderr), true));
    }
    Ok(Tagged { child, streams })
}

/** Reads one stream to EOF, writing every line back out under `[tag]`.

Bytes rather than `lines()`, and lossy rather than strict, because a script's
output is whatever the program it ran decided to emit -- a build tool that
writes one invalid byte should not cost the rest of the line. Colour comes
through untouched for the same reason: an escape sequence is just bytes in
the middle of the line. A final line with no newline is still printed. */
fn pump(stream: Box<dyn Read + Send>, tag: usize, is_stderr: bool, last: &Mutex<Option<usize>>) {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        // the newline is re-added by println, and \r\n would double up
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }

        let text = String::from_utf8_lossy(&line);
        emit(last, tag, |tag, separate| {
            crate::ui::print_tagged(tag, &text, is_stderr, separate)
        });
    }
}
