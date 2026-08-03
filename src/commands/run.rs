use crate::{
    error::Error,
    project::{hooks::Lifecycle, manifest::Manifest},
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
A script hooks its own name: `lpm run build` runs `prebuild`, then `build`, \
then `postbuild`, using whichever of the three [scripts] defines. Hooks do \
not nest, so `prebuild` is run as-is and no `preprebuild` is looked for.\n\n\
Anything after the script name is appended to its command line, shell-quoted, \
with an optional `--` separator: `lpm run build -- --watch` and `lpm build \
--watch` reach the script identically.\n\n\
build, test, start, serve and fmt are also subcommands, so `run` is optional \
for those five. Every other script needs it.")]
pub struct RunArgs {
    /// Name of the script under [scripts] in lpm.toml
    pub name: String,

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
    script(&manifest, &args.name, &args.args)
}

/// runs `name` with its hooks around it, the one path every entry point takes.
fn script(manifest: &Manifest, name: &str, extra: &[String]) -> Result<(), Error> {
    // resolved before anything runs, so a typo'd name never fires `pre<name>`
    let command = append_args(manifest.script(name)?, extra);
    let lifecycle = Lifecycle::of(manifest, name);

    lifecycle.before()?;

    /* the banner names the package as well as the script. in a workspace
    the same script name means different things in different directories,
    and the output below it is otherwise unattributable */
    ui::print_script_notice(manifest.id().as_deref(), name, &command);

    /* wait rather than exec. a failing script should exit with its own
    code since CI reads it, and a successful one still gets lpm's "Done
    in" line -- and, now, its `post<name>` hook. */
    let code = process::wait(process::shell(&command))?;
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
    use super::append_args;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
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
