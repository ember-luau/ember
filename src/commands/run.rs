use crate::{
    error::Error,
    project::{
        hooks::{self, Lifecycle},
        manifest::Manifest,
    },
    sys::process,
    ui,
};
use clap::Args;

/** The `[scripts]` names that are also subcommands, so `lpm build` works
without `run`. npm's idea, and deliberately npm's scale: a handful of verbs
every project has, not every script. npm promotes `test`/`start`/`stop`/
`restart`; the Luau equivalents of "the things you always have" are these.

Widening this list later is harmless. Narrowing it breaks people, so it
stays short. Everything outside it is `lpm run <name>`, which never stops
working for the names that are in it either.

Each entry needs a matching variant on `Commands` in main.rs, which
`shortcuts_are_all_real_subcommands` there checks. */
pub const SHORTCUTS: [&str; 5] = ["build", "test", "start", "serve", "fmt"];

/** The script names `lpm fmt` will accept, in preference order. Both
spellings are common and neither is obviously right, so rather than make
people remember which one lpm chose, `fmt` and `format` are accepted as the
subcommand (one is an alias of the other) and as the `[scripts]` key. */
pub const FMT_NAMES: [&str; 2] = ["fmt", "format"];

/// "build, test, start, serve and fmt", for prose that has to list them.
pub fn shortcut_list() -> String {
    match SHORTCUTS.split_last() {
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

#[derive(Args, Debug)]
#[command(after_long_help = "\
With no script name, `lpm run` lists every [scripts] entry under three \
headings: Scripts, which need `lpm run`; Lifecycle Scripts, whose names are \
subcommands too; and Hooks, the pre/post entries lpm runs by itself.\n\n\
A script hooks its own name: `lpm run build` runs `prebuild`, then `build`, \
then `postbuild`, using whichever of the three [scripts] defines. Hooks do \
not nest, so `prebuild` is run as-is and no `preprebuild` is looked for.\n\n\
Anything after the script name is appended to its command line, shell-quoted, \
with an optional `--` separator: `lpm run build -- --watch` and `lpm build \
--watch` reach the script identically.\n\n\
build, test, start, serve and fmt are also subcommands, so `run` is optional \
for those five. Every other script needs it.")]
pub struct RunArgs {
    /// Name of the script under [scripts] in lpm.toml; omit to list them all
    pub name: Option<String>,

    /// Extra arguments appended to the script's command line
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/// the arguments a [`SHORTCUTS`] subcommand takes: the script name is its own.
#[derive(Args, Debug)]
pub struct ShortcutArgs {
    /// Extra arguments appended to the script's command line
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/** `lpm build` and friends, exactly `lpm run build` with the name baked in.

`names` is usually one name. Where it isn't, as with `fmt`/`format`, the
first one the manifest actually defines wins, and the first overall is what
the "no script named" error names when it defines none of them. */
pub fn shortcut(names: &[&str], args: ShortcutArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    let name = names
        .iter()
        .find(|name| manifest.scripts.contains_key(**name))
        .unwrap_or(&names[0]);

    script(&manifest, name, &args.args)
}

pub fn run(args: RunArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    match &args.name {
        Some(name) => script(&manifest, name, &args.args),
        // bare `lpm run` is a question, not a mistake. npm answers it the same way
        None => {
            list(&manifest);
            Ok(())
        }
    }
}

/** How a `[scripts]` entry is reached, which is the only distinction worth
drawing between them when listing.

⚠ "Lifecycle" here means a script lpm gave a subcommand to, npm's sense of
the word in `npm run`'s output. That is NOT `hooks::Lifecycle`, which is one
event's `pre`/`post` pair -- those land in [`Kind::Hook`]. */
#[derive(Debug, PartialEq)]
enum Kind {
    /// reachable only as `lpm run <name>`.
    Script,
    /// its name is also a subcommand, so `run` is optional.
    Lifecycle,
    /// a `pre`/`post` hook: lpm runs it, nobody types it.
    Hook,
}

/** Classifies one script by how you get at it.

The `pre`/`post` test is deliberately not "starts with pre" -- a script
named `prelude` or `postmortem` is a script, not a hook. What makes a name
a hook is that something else answers to the rest of it: a command event,
or another entry in this same table. That also means a `prebuild` with no
`build` reads as plain, which is exactly right, since `lpm run prebuild` is
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
        _ if SHORTCUTS.contains(&name) || FMT_NAMES.contains(&name) => Kind::Lifecycle,
        _ => Kind::Script,
    }
}

/// prints every `[scripts]` entry, grouped by how it is reached.
fn list(manifest: &Manifest) {
    if manifest.scripts.is_empty() {
        println!("No scripts in lpm.toml. Add some under [scripts]:");
        ui::print_script_entry("build", &["rojo build -o game.rbxl".to_string()]);
        return;
    }

    match manifest.id() {
        Some(id) => println!("Scripts in {id}"),
        // a consuming-only project has no [package] to name
        None => println!("Scripts in lpm.toml"),
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
        ("Scripts", "lpm run <name>", scripts),
        ("Lifecycle Scripts", "lpm <name>", lifecycle),
        ("Hooks", "run by lpm", hooks),
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

/// runs `name` with its hooks around it, the one path every entry point takes.
fn script(manifest: &Manifest, name: &str, extra: &[String]) -> Result<(), Error> {
    /* resolved before anything runs, so a typo'd name never fires `pre<name>`.
    extra arguments go on every command of a parallel script: dropping them
    silently would be worse, and there is no way to tell which one they meant */
    let commands: Vec<String> = manifest
        .script(name)?
        .commands()
        .iter()
        .map(|command| append_args(command, extra))
        .collect();
    let lifecycle = Lifecycle::of(manifest, name);

    lifecycle.before()?;

    /* the banner names the package as well as the script. in a workspace
    the same script name means different things in different directories,
    and the output below it is otherwise unattributable */
    ui::print_script_notice(manifest.id().as_deref(), name, &commands);

    /* wait rather than exec. a failing script should exit with its own
    code since CI reads it, and a successful one still gets lpm's "Done
    in" line -- and, now, its `post<name>` hook. */
    let code = process::script(&commands)?;
    if code != 0 {
        std::process::exit(code);
    }

    lifecycle.after()?;
    Ok(())
}

/** Appends extra command line arguments to a script.

npm's `--` separator is accepted and dropped, so `lpm run build -- --watch`
and the bare `lpm build --watch` produce the same command line. Each argument
is quoted for the platform shell: they arrive as one word each no matter what
whitespace or shell syntax they contain, which is the whole point of having
passed them as separate arguments. */
fn append_args(script: &str, args: &[String]) -> String {
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
    use super::{Kind, Manifest, append_args, kind};

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
        would promise a run that cannot happen. `lpm run prebuild` is the
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
