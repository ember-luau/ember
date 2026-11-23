use crate::{
    error::Error,
    project::{
        hooks::{self, Lifecycle},
        manifest::{Manifest, Script},
        workspace::Workspace,
    },
    sys::{paths, process},
    ui,
};
use clap::Args;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Condvar, Mutex, PoisonError};

/** The `[scripts]` names that are also subcommands, so `embr build` works
without `run`. npm's idea, and deliberately npm's scale: a handful of verbs
every project has, not every script. npm promotes `test`/`start`/`stop`/
`restart`; the Luau equivalents of "the things you always have" are these.

Widening this list later is harmless. Narrowing it breaks people, so it
stays short. Everything outside it is `embr run <name>`, which never stops
working for the names that are in it either.

Each entry needs a matching variant on `Commands` in main.rs, which
`shortcuts_are_all_real_subcommands` there checks. */
pub const SHORTCUTS: [&str; 6] = ["build", "test", "start", "serve", "dev", "fmt"];

/** The script names `embr fmt` will accept, in preference order. Both
spellings are common and neither is obviously right, so rather than make
people remember which one embr chose, `fmt` and `format` are accepted as the
subcommand (one is an alias of the other) and as the `[scripts]` key. */
pub const FMT_NAMES: [&str; 2] = ["fmt", "format"];

/** whether `name` runs without `run`: a shortcut itself, or a shortcut, a
colon, and more, as `build:prod` (#52). */
pub fn is_shortcut(name: &str) -> bool {
    let head = match name.split_once(':') {
        Some((_, "")) => return false,
        Some((head, _)) => head,
        None => name,
    };
    SHORTCUTS.contains(&head) || FMT_NAMES.contains(&head)
}

/// "build, test, start, serve and fmt", for prose that has to list them.
pub fn shortcut_list() -> String {
    match SHORTCUTS.split_last() {
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

#[derive(Args, Debug)]
#[command(after_long_help = "\
With no script name, `embr run` lists every [scripts] entry under three \
headings: Scripts, which need `embr run`; Lifecycle Scripts, whose names are \
subcommands too; and Hooks, the pre/post entries embr runs by itself.\n\n\
A script hooks its own name: `embr run build` runs `prebuild`, then `build`, \
then `postbuild`, using whichever of the three [scripts] defines. Hooks do \
not nest, so `prebuild` is run as-is and no `preprebuild` is looked for.\n\n\
A name that ember.toml does not define comes from the [scripts] of the global \
manifest, ~/.ember/tools.toml, so a global script runs in any directory, also \
outside a project. It runs in the current directory, and no hook runs around \
it. `embr script add --global` adds one.\n\n\
Anything after the script name is appended to its command line, shell-quoted, \
with an optional `--` separator: `embr run build -- --watch` and `embr build \
--watch` reach the script identically.\n\n\
build, test, start, serve, dev and fmt are also subcommands, so `run` is \
optional for those six. It is optional too for a name that starts with one of \
them and a colon, such as `embr dev:gameplay`. Every other script needs it.\n\n\
In a workspace, --workspace, --package, --filter and --changed select members, \
and the script runs in each selected member that defines it. Two or more of \
these flags select the members of each. A member runs after the members that \
it depends on with `workspace = `, and each line of its output starts with \
its name. --jobs runs members at once. After a member fails, no new member \
starts unless --continue is given, and the command exits with code 1. These \
flags go before the script name: each word after the name goes to the script.")]
pub struct RunArgs {
    /// Run the script in each workspace member that defines it, in dependency order
    #[arg(long)]
    pub workspace: bool,

    /// Run the script in this workspace member, by package name, folder name or path; repeat it for more
    #[arg(short, long = "package", value_name = "MEMBER")]
    pub packages: Vec<String>,

    /// Run the script in the workspace members whose name or path matches this glob; repeat it for more
    #[arg(long, value_name = "GLOB")]
    pub filter: Vec<String>,

    /// Run the script in the workspace members with a changed file, and in the members that depend on them
    #[arg(long)]
    pub changed: bool,

    /// Git ref that --changed compares the files with
    #[arg(long, value_name = "REF", default_value = "HEAD", requires = "changed")]
    pub since: String,

    /// Number of workspace members that run at once
    #[arg(short, long, value_name = "N", default_value_t = 1)]
    pub jobs: usize,

    /// Run the other workspace members after one fails
    #[arg(long = "continue")]
    pub keep_going: bool,

    /* the script name, then its arguments. one list, so each word after
    the name goes to the script, also a word that is a flag of `embr run`:
    `embr run test --filter x` gives `--filter x` to the test script. */
    /// Name of the script under [scripts] in ember.toml or the global manifest, then extra arguments for its command line; omit both to list the scripts
    #[arg(
        value_name = "NAME [ARGS]",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub words: Vec<String>,
}

/// the arguments a [`SHORTCUTS`] subcommand takes: the script name is its own.
#[derive(Args, Debug)]
pub struct ShortcutArgs {
    /// Extra arguments appended to the script's command line
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/** `embr build` and friends, exactly `embr run build` with the name baked in.

`names` is usually one name. Where it isn't, as with `fmt`/`format`, the
first one the manifest actually defines wins, and the first overall is what
the "no script named" error names when it defines none of them. */
pub fn shortcut(names: &[&str], args: ShortcutArgs) -> Result<(), Error> {
    let manifest = project()?;
    let global = global_scripts()?;
    // the project's spellings first, as `find` puts the project first
    let name = names
        .iter()
        .find(|name| {
            manifest
                .as_ref()
                .is_some_and(|m| m.scripts.contains_key(**name))
        })
        .or_else(|| names.iter().find(|name| global.contains_key(**name)))
        .unwrap_or(&names[0]);

    script(manifest.as_ref(), &global, name, &args.args)
}

pub fn run(args: RunArgs) -> Result<(), Error> {
    let name = args.words.first();
    /* the list takes a word with a hyphen, so a flag that embr does not
    know lands here as the name */
    if let Some(flag) = name.filter(|name| name.starts_with('-')) {
        return Err(fail(format!(
            "`embr run` has no option '{flag}'; its options go before the script name, see `embr run --help`"
        )));
    }
    let extra = args.words.get(1..).unwrap_or_default();
    if args.workspace || !args.packages.is_empty() || !args.filter.is_empty() || args.changed {
        let name = name
            .ok_or_else(|| fail("Name the script to run in the workspace members".to_string()))?;
        return members(&args, name, extra);
    }
    let manifest = project()?;
    let global = global_scripts()?;
    match name {
        Some(name) => script(manifest.as_ref(), &global, name, extra),
        // bare `embr run` is a question, not a mistake. npm answers it the same way
        None => list(manifest.as_ref(), &global),
    }
}

fn fail(message: String) -> Error {
    std::io::Error::other(message).into()
}

/** runs the script `name` in the workspace members that the flags select
and that define it, each after the members that it depends on.

each member is a child `embr run` in the folder of the member, so its
hooks, its banner and a script of several commands work as they do alone.
the output of a member is piped and gets the name of the member in front
of each line, since members run at once with `--jobs`. */
fn members(args: &RunArgs, name: &str, extra: &[String]) -> Result<(), Error> {
    let workspace = Workspace::here()?.ok_or_else(|| {
        fail(
            "This folder is in no workspace; the workspace flags need an ember.toml that lists members under [target] workspace"
                .to_string(),
        )
    })?;
    // before a member runs: a cycle has no order to run in
    let order = workspace.order()?;

    let mut selected = BTreeSet::new();
    if args.workspace {
        selected.extend(0..workspace.members.len());
    }
    for package in &args.packages {
        selected.insert(
            workspace
                .find(package)
                .ok_or_else(|| Error::NoWorkspaceMember(package.clone()))?,
        );
    }
    for glob in &args.filter {
        let matcher = globset::Glob::new(glob)
            .map_err(|error| Error::WorkspaceGlobInvalid {
                glob: glob.clone(),
                reason: error.to_string(),
            })?
            .compile_matcher();
        let matched: Vec<usize> = (0..workspace.members.len())
            .filter(|&member| {
                let manifest = &workspace.members[member].manifest;
                matcher.is_match(workspace.members[member].label())
                    || manifest.name().is_some_and(|name| matcher.is_match(name))
                    || matcher.is_match(workspace.path_of(member))
            })
            .collect();
        if matched.is_empty() {
            return Err(fail(format!("No workspace member matches '{glob}'")));
        }
        selected.extend(matched);
    }
    if args.changed {
        selected.extend(workspace.changed(&args.since)?);
    }
    if selected.is_empty() {
        // only --changed selects nothing without an error
        println!("No workspace member has a change since {}", args.since);
        return Ok(());
    }

    let queue: Vec<usize> = order
        .into_iter()
        .filter(|member| selected.contains(member))
        .filter(|&member| {
            workspace.members[member]
                .manifest
                .scripts
                .contains_key(name)
        })
        .collect();
    if queue.is_empty() {
        return Err(fail(format!(
            "No selected workspace member has a script named '{name}'"
        )));
    }

    /* a member waits for each member of this run that it depends on, also
    through a member that is not in the run */
    let edges = workspace.edges();
    let waits_for = |member: usize| -> Vec<usize> {
        let mut seen = BTreeSet::new();
        let mut stack = edges[member].clone();
        while let Some(on) = stack.pop() {
            if seen.insert(on) {
                stack.extend(&edges[on]);
            }
        }
        seen.into_iter().filter(|on| queue.contains(on)).collect()
    };
    let waits: BTreeMap<usize, Vec<usize>> = queue
        .iter()
        .map(|&member| (member, waits_for(member)))
        .collect();

    #[derive(Default)]
    struct State {
        queue: Vec<usize>,
        running: usize,
        finished: BTreeSet<usize>,
        failed: Vec<usize>,
        stopped: bool,
    }
    let state = Mutex::new(State {
        queue: queue.clone(),
        ..State::default()
    });
    let turn = Condvar::new();
    let embr = std::env::current_exe()?;

    std::thread::scope(|scope| {
        for _ in 0..args.jobs.clamp(1, queue.len()) {
            scope.spawn(|| {
                loop {
                    let mut now = state.lock().unwrap_or_else(PoisonError::into_inner);
                    let member = loop {
                        if now.stopped || now.queue.is_empty() {
                            return;
                        }
                        let ready = now.queue.iter().position(|member| {
                            waits[member].iter().all(|on| now.finished.contains(on))
                        });
                        if let Some(ready) = ready {
                            break now.queue.remove(ready);
                        }
                        now = turn.wait(now).unwrap_or_else(PoisonError::into_inner);
                    };
                    now.running += 1;
                    drop(now);

                    let label = workspace.members[member].label();
                    let code =
                        member_script(&embr, &workspace.members[member].dir, &label, name, extra);
                    let mut now = state.lock().unwrap_or_else(PoisonError::into_inner);
                    now.running -= 1;
                    now.finished.insert(member);
                    if code != 0 {
                        ui::print_member_line(&label, &format!("exited with code {code}"), true);
                        now.failed.push(member);
                        // without --continue no new member starts. the running ones finish
                        now.stopped = !args.keep_going;
                    }
                    turn.notify_all();
                }
            });
        }
    });

    let state = state.into_inner().unwrap_or_else(PoisonError::into_inner);
    if state.failed.is_empty() {
        return Ok(());
    }
    let labels = |members: &[usize]| -> String {
        let mut labels: Vec<String> = members
            .iter()
            .map(|&member| workspace.members[member].label())
            .collect();
        labels.sort();
        labels.join(", ")
    };
    let mut message = format!("'{name}' failed in {}", labels(&state.failed));
    if !state.queue.is_empty() {
        message.push_str(&format!(
            "; not started: {}; `--continue` runs them too",
            labels(&state.queue)
        ));
    }
    ui::print_error(&message);
    // a member keeps its own code in its line. the run of many has one code
    std::process::exit(1);
}

/** runs `embr run <name>` in `dir` and gives its exit code. each line of
its output goes to the same stream of this process, behind `label`. */
fn member_script(embr: &Path, dir: &Path, label: &str, name: &str, extra: &[String]) -> i32 {
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    let mut command = Command::new(embr);
    command
        .arg("run")
        .arg(name)
        .args(extra)
        .current_dir(dir)
        // members run at once, and one terminal input cannot serve them all
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // a pipe is no terminal, so the member would drop the colour that this run shows
    if ui::want_color() {
        command.env("FORCE_COLOR", "1").env("CLICOLOR_FORCE", "1");
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            ui::print_member_line(label, &format!("could not start: {error}"), true);
            return 1;
        }
    };
    let pump = |stream: Option<Box<dyn std::io::Read + Send>>, is_stderr: bool| {
        move || {
            let Some(stream) = stream else { return };
            for line in std::io::BufReader::new(stream).split(b'\n').flatten() {
                ui::print_member_line(label, &String::from_utf8_lossy(&line), is_stderr);
            }
        }
    };
    let stdout = child
        .stdout
        .take()
        .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>);
    let stderr = child
        .stderr
        .take()
        .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>);
    std::thread::scope(|scope| {
        scope.spawn(pump(stdout, false));
        scope.spawn(pump(stderr, true));
    });
    child
        .wait()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1)
}

/** the manifest in the current directory, None outside a project. a
manifest that is there and fails to load is still an error: it can hold a
project script that would shadow the global one. */
fn project() -> Result<Option<Manifest>, Error> {
    match Manifest::load() {
        Ok(manifest) => Ok(Some(manifest)),
        Err(Error::ManifestMissing) => Ok(None),
        Err(error) => Err(error),
    }
}

/// the [scripts] of the global manifest, ~/.ember/tools.toml. empty when the file does not exist.
pub fn global_scripts() -> Result<BTreeMap<String, Script>, Error> {
    global_scripts_in(&paths::global_tools_file()?)
}

fn global_scripts_in(path: &Path) -> Result<BTreeMap<String, Script>, Error> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    parse_global(&std::fs::read_to_string(path)?)
}

fn parse_global(text: &str) -> Result<BTreeMap<String, Script>, Error> {
    // the file also holds [tools], which the shims read on their own
    #[derive(Deserialize)]
    struct Global {
        #[serde(default)]
        scripts: BTreeMap<String, Script>,
    }
    Ok(toml::from_str::<Global>(text)?.scripts)
}

/// where `embr run <name>` found the script.
#[derive(Debug)]
enum Found<'a> {
    /// the project's [scripts], which always wins.
    Project(&'a Manifest),
    /// the global manifest's [scripts], when the project has no such name.
    Global(&'a Script),
}

/** finds `name`: the project first, then the global manifest. a name that
neither has keeps the error `embr run` gave before global scripts, which
outside a project is the missing ember.toml. */
fn find<'a>(
    manifest: Option<&'a Manifest>,
    global: &'a BTreeMap<String, Script>,
    name: &str,
) -> Result<Found<'a>, Error> {
    match (manifest, global.get(name)) {
        (Some(manifest), _) if manifest.scripts.contains_key(name) => Ok(Found::Project(manifest)),
        (_, Some(script)) if script.commands().is_empty() => {
            Err(Error::GlobalScriptEmpty(name.to_string()))
        }
        (_, Some(script)) => Ok(Found::Global(script)),
        (Some(_), None) => Err(Error::ScriptMissing(name.to_string())),
        (None, None) => Err(Error::ManifestMissing),
    }
}

/** How a `[scripts]` entry is reached, which is the only distinction worth
drawing between them when listing.

⚠ "Lifecycle" here means a script embr gave a subcommand to, npm's sense of
the word in `npm run`'s output. That is NOT `hooks::Lifecycle`, which is one
event's `pre`/`post` pair -- those land in [`Kind::Hook`]. */
#[derive(Debug, PartialEq)]
enum Kind {
    /// reachable only as `embr run <name>`.
    Script,
    /// its name is also a subcommand, so `run` is optional.
    Lifecycle,
    /// a `pre`/`post` hook: embr runs it, nobody types it.
    Hook,
}

/** Classifies one script by how you get at it.

The `pre`/`post` test is deliberately not "starts with pre" -- a script
named `prelude` or `postmortem` is a script, not a hook. What makes a name
a hook is that something else answers to the rest of it: a command event,
or another entry in this same table. That also means a `prebuild` with no
`build` reads as plain, which is exactly right, since `embr run prebuild` is
then the only way it ever runs. */
fn kind(manifest: &Manifest, name: &str) -> Kind {
    let base = name
        .strip_prefix("pre")
        .or_else(|| name.strip_prefix("post"));
    match base {
        Some(base)
            if !base.is_empty()
                && (hooks::EVENTS.contains(&base) || manifest.scripts.contains_key(base)) =>
        {
            Kind::Hook
        }
        _ if is_shortcut(name) => Kind::Lifecycle,
        _ => Kind::Script,
    }
}

/** prints the project's scripts, then the global ones. outside a project
with no global scripts, this is the missing ember.toml, as before. */
fn list(manifest: Option<&Manifest>, global: &BTreeMap<String, Script>) -> Result<(), Error> {
    match manifest {
        None if global.is_empty() => return Err(Error::ManifestMissing),
        Some(manifest) if manifest.scripts.is_empty() && global.is_empty() => {
            println!("No scripts in ember.toml. Add some under [scripts]:");
            ui::print_script_entry("build", &["rojo build -o game.rbxl".to_string()]);
            return Ok(());
        }
        Some(manifest) if !manifest.scripts.is_empty() => list_project(manifest),
        _ => {}
    }

    if !global.is_empty() {
        ui::print_heading("Global Scripts", "embr run <name>, in any directory");
        for (name, script, shadowed) in global_entries(manifest, global) {
            if shadowed {
                ui::print_script_entry(
                    &format!("{name} (shadowed by ember.toml)"),
                    script.commands(),
                );
            } else {
                ui::print_script_entry(name, script.commands());
            }
        }
    }
    Ok(())
}

/// each global script, and whether a project script of the same name shadows it.
fn global_entries<'a>(
    manifest: Option<&Manifest>,
    global: &'a BTreeMap<String, Script>,
) -> Vec<(&'a str, &'a Script, bool)> {
    global
        .iter()
        .map(|(name, script)| {
            let shadowed = manifest.is_some_and(|manifest| manifest.scripts.contains_key(name));
            (name.as_str(), script, shadowed)
        })
        .collect()
}

/// prints every project `[scripts]` entry, grouped by how it is reached.
fn list_project(manifest: &Manifest) {
    match manifest.id() {
        Some(id) => println!("Scripts in {id}"),
        // a consuming-only project has no [package] to name
        None => println!("Scripts in ember.toml"),
    }

    let mut scripts = Vec::new();
    let mut lifecycle = Vec::new();
    let mut hooks = Vec::new();
    for (name, command) in &manifest.scripts {
        match kind(manifest, name) {
            Kind::Script => scripts.push((name, command)),
            Kind::Lifecycle => lifecycle.push((name, command)),
            Kind::Hook => hooks.push((name, command)),
        }
    }

    /* an empty group prints nothing rather than an empty heading, so a
    project with only ordinary scripts gets one list and no taxonomy it
    never asked for. the dimmed hint carries what the heading alone can't,
    which is how you actually run the things underneath it */
    for (heading, hint, group) in [
        ("Scripts", "embr run <name>", scripts),
        ("Lifecycle Scripts", "embr <name>", lifecycle),
        ("Hooks", "run by embr", hooks),
    ] {
        if group.is_empty() {
            continue;
        }
        ui::print_heading(heading, hint);
        for (name, script) in group {
            ui::print_script_entry(name, script.commands());
        }
    }
}

/** runs `name`, the one path every entry point takes. a project script
gets its hooks around it. a global script runs alone, as hooks stay with
the project. */
fn script(
    manifest: Option<&Manifest>,
    global: &BTreeMap<String, Script>,
    name: &str,
    extra: &[String],
) -> Result<(), Error> {
    /* resolved before anything runs, so a typo'd name never fires `pre<name>`.
    the banner names the package as well as the script. in a workspace the
    same script name means different things in different directories, and
    the output below it is otherwise unattributable */
    let (banner, found, lifecycle) = match find(manifest, global, name)? {
        Found::Project(manifest) => (
            manifest.id(),
            manifest.script(name)?,
            Some(Lifecycle::of(manifest, name)),
        ),
        Found::Global(script) => (Some("global".to_string()), script, None),
    };
    /* extra arguments go on every command of a parallel script: dropping them
    silently would be worse, and there is no way to tell which one they meant */
    let commands: Vec<String> = found
        .commands()
        .iter()
        .map(|command| append_args(command, extra))
        .collect();

    if let Some(lifecycle) = &lifecycle {
        lifecycle.before()?;
    }

    ui::print_script_notice(banner.as_deref(), name, &commands);

    /* wait rather than exec. a failing script should exit with its own
    code since CI reads it, and a successful one still gets embr's "Done
    in" line -- and, now, its `post<name>` hook. */
    let code = process::script(&commands)?;
    if code != 0 {
        std::process::exit(code);
    }

    if let Some(lifecycle) = &lifecycle {
        lifecycle.after()?;
    }
    Ok(())
}

/** Appends extra command line arguments to a script.

npm's `--` separator is accepted and dropped, so `embr run build -- --watch`
and the bare `embr build --watch` produce the same command line. Each argument
is quoted for the platform shell: they arrive as one word each no matter what
whitespace or shell syntax they contain, which is the whole point of having
passed them as separate arguments. */
pub(crate) fn append_args(script: &str, args: &[String]) -> String {
    let args = match args.split_first() {
        Some((first, rest)) if first == "--" => rest,
        _ => args,
    };

    let mut command = script.to_string();
    for arg in args {
        command.push(' ');
        command.push_str(&quote(arg));
    }
    command
}

/** Whether `arg` has to be quoted at all.

Characters that mean nothing to either sh or cmd are left bare, so the
banner shows `rojo build --watch 'my game.rbxl'` rather than quoting every
word of it. Conservative on purpose: anything outside this set is quoted,
including `~`, `%`, `!` and `\`, which are inert in one shell and not the
other. An empty argument must be quoted or it would vanish. */
fn needs_quoting(arg: &str) -> bool {
    arg.is_empty()
        || !arg.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '=' | ':' | ',' | '+')
        })
}

/** Quotes one argument for `sh -c`. Single quotes make everything inside
literal; the one thing they cannot hold is a single quote, which closes,
escapes and reopens. */
#[cfg(not(windows))]
fn quote(arg: &str) -> String {
    if !needs_quoting(arg) {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/** Quotes one argument for `cmd /C`.

Everything is quoted, unconditionally: `&`, `|`, `<`, `>` and `^` are cmd
syntax outside double quotes and plain text inside them. Within the quotes
the only thing cmd itself still reads is `%VAR%`, which stays expandable --
the script string around it is a raw cmd command line too, so it would be
odd for the arguments to follow different rules.

Backslashes and inner quotes are escaped the way the C runtime unquotes
them, since that is what the program on the other end will apply: a quote
is `\"`, and a run of backslashes doubles only when a quote (including the
closing one) follows it. */
#[cfg(windows)]
fn quote(arg: &str) -> String {
    if !needs_quoting(arg) {
        return arg.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0;

    for character in arg.chars() {
        match character {
            '\\' => {
                backslashes += 1;
                quoted.push('\\');
            }
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                backslashes = 0;
                quoted.push(character);
            }
        }
    }

    // a trailing run would otherwise escape the closing quote
    quoted.extend(std::iter::repeat_n('\\', backslashes));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::{
        BTreeMap, Error, Found, Kind, Manifest, Script, append_args, find, global_entries,
        global_scripts_in, kind, parse_global,
    };

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    fn manifest(scripts: &str) -> Manifest {
        toml::from_str(&format!(
            "[package]\nname = \"scope/name\"\nversion = \"0.1.0\"\n\n[scripts]\n{scripts}"
        ))
        .unwrap()
    }

    #[test]
    fn hooks_are_named_after_something_that_answers() {
        let manifest = manifest(
            "build = \"a\"\nprebuild = \"b\"\npostbuild = \"c\"\n\
             preinstall = \"d\"\npostpublish = \"e\"\n",
        );

        // a hook over another script in the table
        assert_eq!(kind(&manifest, "prebuild"), Kind::Hook);
        assert_eq!(kind(&manifest, "postbuild"), Kind::Hook);
        // a hook over a command event, which needs no script of its own
        assert_eq!(kind(&manifest, "preinstall"), Kind::Hook);
        assert_eq!(kind(&manifest, "postpublish"), Kind::Hook);
    }

    #[test]
    fn a_word_starting_with_pre_is_not_a_hook() {
        /* the whole reason kind() looks past the prefix: nothing answers to
        "lude" or "mortem", so these are ordinary scripts */
        let manifest = manifest("prelude = \"a\"\npostmortem = \"b\"\npre = \"c\"\npost = \"d\"\n");
        for name in ["prelude", "postmortem", "pre", "post"] {
            assert_eq!(kind(&manifest, name), Kind::Script, "{name}");
        }
    }

    #[test]
    fn a_hook_with_nothing_to_hook_is_just_a_script() {
        /* `prebuild` without `build` never fires, so listing it as lifecycle
        would promise a run that cannot happen. `embr run prebuild` is the
        only way it goes, which is exactly what Plain says */
        let manifest = manifest("prebuild = \"a\"\n");
        assert_eq!(kind(&manifest, "prebuild"), Kind::Script);
    }

    #[test]
    fn shortcut_names_are_lifecycle_scripts_both_spellings_of_fmt_included() {
        let manifest = manifest(
            "build = \"a\"\ntest = \"b\"\nstart = \"c\"\nserve = \"d\"\n\
             fmt = \"e\"\nformat = \"f\"\nlint = \"g\"\n",
        );
        for name in ["build", "test", "start", "serve", "fmt", "format"] {
            assert_eq!(kind(&manifest, name), Kind::Lifecycle, "{name}");
        }
        // everything else needs `run`
        assert_eq!(kind(&manifest, "lint"), Kind::Script);
    }

    #[test]
    fn a_shortcut_and_a_colon_start_a_lifecycle_script() {
        let manifest = manifest(
            "\"build:prod\" = \"a\"\n\"format:check\" = \"b\"\n\"lint:fix\" = \"c\"\n\
             \"build:\" = \"d\"\nbuilder = \"e\"\n",
        );
        for name in ["build:prod", "format:check"] {
            assert_eq!(kind(&manifest, name), Kind::Lifecycle, "{name}");
        }
        // the colon has to follow a whole shortcut, and something has to follow it
        for name in ["lint:fix", "build:", "builder"] {
            assert_eq!(kind(&manifest, name), Kind::Script, "{name}");
        }
    }

    /// a global [scripts] table, parsed the way the global manifest is.
    fn globals(scripts: &str) -> BTreeMap<String, Script> {
        // [tools] sits beside [scripts] in the real file and must not get in the way
        parse_global(&format!(
            "[tools]\nrojo = \"rojo-rbx/rojo@7.4.4\"\n\n[scripts]\n{scripts}"
        ))
        .unwrap()
    }

    #[test]
    fn a_missing_global_manifest_has_no_scripts() {
        let path = std::env::temp_dir().join("embr-test-no-global/tools.toml");
        assert!(global_scripts_in(&path).unwrap().is_empty());
    }

    #[test]
    fn the_project_script_wins_and_the_global_one_fills_in() {
        let project = manifest("build = \"rojo build\"\n");
        let global = globals("build = \"global build\"\nlint = \"selene src\"\n");

        assert!(matches!(
            find(Some(&project), &global, "build"),
            Ok(Found::Project(_))
        ));
        assert!(matches!(
            find(Some(&project), &global, "lint"),
            Ok(Found::Global(script)) if script.commands() == ["selene src"]
        ));
        // outside a project, every global script runs
        assert!(matches!(
            find(None, &global, "build"),
            Ok(Found::Global(script)) if script.commands() == ["global build"]
        ));
    }

    #[test]
    fn a_name_that_nobody_defines_keeps_the_old_error() {
        let global = globals("lint = \"selene src\"\n");
        assert!(matches!(
            find(Some(&manifest("build = \"a\"\n")), &global, "test"),
            Err(Error::ScriptMissing(name)) if name == "test"
        ));
        assert!(matches!(
            find(None, &global, "test"),
            Err(Error::ManifestMissing)
        ));
    }

    #[test]
    fn an_empty_global_script_is_refused() {
        let global = globals("lint = []\n");
        assert!(matches!(
            find(None, &global, "lint"),
            Err(Error::GlobalScriptEmpty(name)) if name == "lint"
        ));
        // an empty project script still shadows, and `Manifest::script` refuses it
        let project = manifest("lint = []\n");
        assert!(matches!(
            find(Some(&project), &globals("lint = \"a\"\n"), "lint"),
            Ok(Found::Project(_))
        ));
    }

    #[test]
    fn listing_marks_the_global_scripts_a_project_shadows() {
        let global = globals("build = \"a\"\nlint = \"b\"\n");
        let project = manifest("build = \"c\"\n");

        let entries: Vec<(&str, bool)> = global_entries(Some(&project), &global)
            .into_iter()
            .map(|(name, _, shadowed)| (name, shadowed))
            .collect();
        assert_eq!(entries, [("build", true), ("lint", false)]);

        // outside a project nothing shadows
        assert!(
            global_entries(None, &global)
                .iter()
                .all(|(_, _, shadowed)| !shadowed)
        );
    }

    #[test]
    fn no_arguments_leaves_the_script_untouched() {
        assert_eq!(append_args("rojo build", &[]), "rojo build");
        // a lone separator is still no arguments
        assert_eq!(append_args("rojo build", &args(&["--"])), "rojo build");
    }

    #[test]
    fn the_separator_is_optional_and_only_the_first_one_is_dropped() {
        let with = append_args("rojo build", &args(&["--", "--watch"]));
        let without = append_args("rojo build", &args(&["--watch"]));
        assert_eq!(with, without);
        // a second one is an argument, it belongs to the script
        assert!(append_args("sh x", &args(&["--", "--", "-a"])).contains("--"));
    }

    #[test]
    fn ordinary_arguments_are_left_bare() {
        /* same on both shells, and what keeps the banner readable: nothing
        here means anything to sh or cmd, so nothing gets quotes */
        assert_eq!(
            append_args("stylua", &args(&["--check", "src/init.luau"])),
            "stylua --check src/init.luau"
        );
        assert_eq!(
            append_args("x", &args(&["a=1", "1.2.3", "a,b", "-o", "v1.0+build"])),
            "x a=1 1.2.3 a,b -o v1.0+build"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn sh_arguments_survive_whitespace_and_shell_syntax() {
        assert_eq!(
            append_args("rojo build -o", &args(&["my game.rbxl"])),
            "rojo build -o 'my game.rbxl'"
        );
        // syntax stays text: no second command, no glob, no substitution
        assert_eq!(
            append_args("echo", &args(&["a && rm -rf /", "*", "$HOME"])),
            "echo 'a && rm -rf /' '*' '$HOME'"
        );
        // the one character single quotes can't hold
        assert_eq!(append_args("echo", &args(&["it's"])), r"echo 'it'\''s'");
        // an empty argument still has to survive as an argument
        assert_eq!(append_args("echo", &args(&[""])), "echo ''");
    }

    #[cfg(windows)]
    #[test]
    fn cmd_arguments_survive_whitespace_and_shell_syntax() {
        assert_eq!(
            append_args("rojo build -o", &args(&["my game.rbxl"])),
            "rojo build -o \"my game.rbxl\""
        );
        assert_eq!(
            append_args("echo", &args(&["a && del x", "*"])),
            "echo \"a && del x\" \"*\""
        );
        // a quote is escaped for the callee's own unquoting
        assert_eq!(
            append_args("echo", &args(&["say \"hi\""])),
            r#"echo "say \"hi\"""#
        );
        // trailing backslashes double so they don't eat the closing quote
        assert_eq!(
            append_args("echo", &args(&[r"C:\dir\"])),
            r#"echo "C:\dir\\""#
        );
        // an empty argument still has to survive as an argument
        assert_eq!(append_args("echo", &args(&[""])), "echo \"\"");
    }
}
