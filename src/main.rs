mod art;
mod commands;
mod error;
mod net;
mod project;
mod registry;
mod sys;
mod tools;
mod ui;

use clap::{CommandFactory, Parser, Subcommand};
use commands::self_cmd::SelfCommand;

#[derive(Parser, Debug)]
#[command(name = "lpm", bin_name = "lpm", version, about, styles = ui::help_styles())]
struct Cli {
    /** None is bare `lpm`, which prints the same help `-h` does rather than
    a usage error: it is what someone types when they want to see the tool. */
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create an lpm.toml manifest in the current directory
    Init,

    /// Runs a script from lpm.toml, or lists them all when given no name
    Run(commands::run::RunArgs),

    /* the [scripts] names that double as subcommands, npm's
    test/start/stop/restart idea. real variants rather than a fallback for
    unrecognized commands, so they error like any other command and the set
    stays a decision instead of "whatever the manifest happens to define".
    the list lives in commands::run::SHORTCUTS.

    hidden, npm's way: `npm start` works everywhere and is nobody's idea of a
    command to look up. they belong to a project's [scripts], not to lpm's own
    surface, and listing five of them would say otherwise while telling a
    project that defines none of them about commands it cannot run. `lpm run`
    lists what this project actually has, and `script_hint` points at these
    from the one place they get typed by mistake. */
    /// Runs the `build` script from lpm.toml
    #[command(hide = true)]
    Build(commands::run::ShortcutArgs),

    /// Runs the `test` script from lpm.toml
    #[command(hide = true)]
    Test(commands::run::ShortcutArgs),

    /// Runs the `start` script from lpm.toml
    #[command(hide = true)]
    Start(commands::run::ShortcutArgs),

    /// Runs the `serve` script from lpm.toml
    #[command(hide = true)]
    Serve(commands::run::ShortcutArgs),

    /// Runs the `fmt` (or `format`) script from lpm.toml
    // plain alias, not visible_alias: nothing of this command is on display
    #[command(hide = true, alias = "format")]
    Fmt(commands::run::ShortcutArgs),

    /// Download (if needed) and run a GitHub-released executable
    #[command(visible_alias = "x")]
    Execute(commands::execute::ExecuteArgs),

    /// Set up and open this project's place in Roblox Studio
    #[command(arg_required_else_help = true)]
    Studio {
        #[command(subcommand)]
        command: commands::studio::StudioCommand,
    },

    /// Add a dependency to lpm.toml
    Add(commands::add::AddArgs),

    /// Manage tooling used in the current project
    #[command(subcommand)]
    Tool(commands::tool::ToolCommand),

    /// Manage the package indices this project pulls from
    #[command(subcommand)]
    Index(commands::index::IndexCommand),

    /// Edit a dependency's source and keep the edit across installs
    #[command(arg_required_else_help = true)]
    Patch(commands::patch::PatchArgs),

    /// Install dependencies and tools from lpm.toml
    #[command(visible_alias = "i")]
    Install(commands::install::InstallArgs),

    /// Publish this package to the lpm registry
    Publish(commands::publish::PublishArgs),

    /// Manage lpm's caches
    #[command(subcommand)]
    Cache(commands::cache::CacheCommand),

    /// Manage this lpm installation
    #[command(subcommand, name = "self")]
    SelfManage(SelfCommand),
}

fn main() {
    /* tool shims are copies of lpm named after their alias. started under
    one of those names, run the tool the surrounding manifest pins
    instead of the CLI. */
    if let Some(alias) = tools::shim::shim_alias() {
        match tools::shim::run(&alias) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                ui::print_error(&err.to_string());
                std::process::exit(1);
            }
        }
    }

    let args: Vec<std::ffi::OsString> = if tools::shim::invoked_as_lpx() {
        /* lpx is `lpm execute` under its own name, a copy `self install`
        drops beside lpm, so every argument belongs to execute */
        let prefix: [std::ffi::OsString; 2] = ["lpm".into(), "execute".into()];
        prefix
            .into_iter()
            .chain(std::env::args_os().skip(1))
            .collect()
    } else {
        std::env::args_os().collect()
    };

    /* the three ways to ask lpm itself for help, caught before clap so they
    get the logo layout instead of the plain one. anything longer is a
    question about a subcommand, and clap answers those as it always has:
    `lpm add -h`, `lpm help tool`, `lpm tool add --help`. */
    if matches!(
        args.get(1).and_then(|arg| arg.to_str()),
        Some("-h" | "--help" | "help")
    ) && args.len() == 2
    {
        print_root_help();
        return;
    }

    let cli = parse_cli(args);
    let Some(command) = cli.command else {
        print_root_help();
        return;
    };

    let started = std::time::Instant::now();
    let result = match command {
        Commands::Init => commands::init::run(),
        /* execute hands the terminal to another program, so its exit code
        passes through as-is and no "Done in" line prints */
        Commands::Execute(args) => match commands::execute::run(args) {
            Ok(code) => std::process::exit(code),
            Err(err) => Err(err),
        },
        Commands::Add(args) => commands::add::run(args),
        Commands::Index(command) => commands::index::run(command),
        Commands::Install(args) => commands::install::run(args),
        Commands::Patch(args) => commands::patch::run(args),
        Commands::Publish(args) => commands::publish::run(args),
        Commands::Cache(command) => commands::cache::run(command),
        Commands::SelfManage(command) => commands::self_cmd::run(command),
        Commands::Tool(command) => commands::tool::run(command),
        Commands::Studio { command } => commands::studio::run(command),
        Commands::Run(args) => commands::run::run(args),
        Commands::Build(args) => commands::run::shortcut(&["build"], args),
        Commands::Test(args) => commands::run::shortcut(&["test"], args),
        Commands::Start(args) => commands::run::shortcut(&["start"], args),
        Commands::Serve(args) => commands::run::shortcut(&["serve"], args),
        Commands::Fmt(args) => commands::run::shortcut(&commands::run::FMT_NAMES, args),
    };

    match result {
        Ok(()) => ui::print_elapsed(started.elapsed()),
        Err(err) => {
            ui::print_error(&err.to_string());
            std::process::exit(err.exit_code());
        }
    }
}

/// parses, reporting a failure in lpm's own style rather than clap's.
fn parse_cli(args: impl IntoIterator<Item = std::ffi::OsString>) -> Cli {
    Cli::try_parse_from(args).unwrap_or_else(|err| report_parse_error(err))
}

/** fastfetch's layout: the logo on the left, the help beside it, both painted
with one gradient so they read as a single object. stacks when the terminal is
too narrow to hold them side by side. */
fn print_root_help() {
    use std::io::Write;

    /// columns between the logo and the help text.
    const GAP: usize = 3;
    /// narrower than this beside the logo and the help wraps into soup, so stack instead.
    const HELP_MIN: usize = 50;

    /* the art is for a person looking at a terminal. redirected, `lpm --help`
    is being read by something -- a pager, a grep, scripts/golden-cli.ps1 --
    and a logo down the left of every line is in its way. colour is gated
    separately, so NO_COLOR in a terminal still gets the layout, unpainted. */
    let color = ui::want_color();
    let logo = art::logo(color);
    let logo_lines: Vec<&str> = logo.lines().collect();
    let logo_width = logo_lines
        .iter()
        .map(|line| ui::visible_width(line))
        .max()
        .unwrap_or(0);

    let width = ui::term_width();
    let side_by_side = ui::is_terminal() && width >= logo_width + GAP + HELP_MIN;
    let help_width = if side_by_side {
        (width - logo_width - GAP).min(80)
    } else {
        width.min(100)
    };

    /* bold only: the gradient below paints every line, and clap's own accent
    colours would fight it */
    let rendered = Cli::command()
        .styles(ui::bold_styles())
        .term_width(help_width)
        .render_help();
    let help = if color {
        rendered.ansi().to_string()
    } else {
        rendered.to_string()
    };
    let help_lines: Vec<&str> = help.lines().collect();

    let mut out = String::new();
    if side_by_side {
        let rows = logo_lines.len().max(help_lines.len());
        // centre the shorter column against the taller one
        let logo_top = (rows - logo_lines.len()) / 2;
        let help_top = (rows - help_lines.len()) / 2;

        for row in 0..rows {
            let logo_line = row
                .checked_sub(logo_top)
                .and_then(|index| logo_lines.get(index).copied())
                .unwrap_or("");
            let help_line = row
                .checked_sub(help_top)
                .and_then(|index| help_lines.get(index).copied())
                .unwrap_or("");

            out.push_str(logo_line);
            for _ in 0..logo_width - ui::visible_width(logo_line) + GAP {
                out.push(' ');
            }
            push_help_line(&mut out, help_line, row, rows, color);
            // no trailing whitespace on rows the help doesn't reach
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
        }
    } else {
        // a narrow terminal still gets the logo, stacked above its help
        if ui::is_terminal() {
            out.push_str(&logo);
            out.push_str("\n\n");
        }
        let rows = help_lines.len();
        for (row, line) in help_lines.iter().enumerate() {
            push_help_line(&mut out, line, row, rows, color);
            out.push('\n');
        }
    }

    // a closed pipe (`lpm | head`) is not an error
    if let Err(err) = std::io::stdout().write_all(out.as_bytes())
        && err.kind() != std::io::ErrorKind::BrokenPipe
    {
        ui::print_error(&err.to_string());
        std::process::exit(1);
    }
}

/** appends one help line in its row's gradient colour. clap's bold markers
reset the colour as they close, so the colour is reapplied after each one or
the rest of the line would fall back to the terminal's own. */
fn push_help_line(out: &mut String, line: &str, row: usize, rows: usize, color: bool) {
    if !color || line.is_empty() {
        out.push_str(line);
        return;
    }
    let paint = ui::fg(art::row_color(row, rows));
    let repainted = line.replace(ui::RESET, &format!("{}{paint}", ui::RESET));
    out.push_str(&paint);
    out.push_str(&repainted);
    out.push_str(ui::RESET);
}

/** Points at `lpm run <name>` when the unrecognized subcommand turns out to
be a script this project defines.

Only four script names double as subcommands, so `lpm fmt` is a mistake
people will make -- and "unrecognized subcommand 'fmt'" is a bad answer when
lpm.toml plainly has an `fmt` script. None for anything else, including a
name that is simply nobody's script, which keeps clap's own did-you-mean
suggestion the only thing said about it. */
fn script_hint(err: &clap::Error) -> Option<String> {
    use clap::error::{ContextKind, ContextValue};

    if err.kind() != clap::error::ErrorKind::InvalidSubcommand {
        return None;
    }
    let Some(ContextValue::String(name)) = err.get(ContextKind::InvalidSubcommand) else {
        return None;
    };

    let manifest = project::manifest::Manifest::load().ok()?;
    manifest.scripts.contains_key(name).then(|| {
        format!(
            "'{name}' is a script in lpm.toml; run it with `lpm run {name}` (only {} can drop `run`)",
            commands::run::shortcut_list()
        )
    })
}

/** Reports a clap parse failure, restyling its hardcoded "error: <message>"
line as an accent "✗ <message>" while keeping the styled usage/tip lines
below. Exits rather than returning. */
fn report_parse_error(err: clap::Error) -> ! {
    let plain = err.render().to_string();
    // help and version output pass through clap untouched.
    let Some(rest) = plain.strip_prefix("error: ") else {
        err.exit()
    };

    // clap starts messages lowercase, capitalize to match our own errors.
    let message = rest.lines().next().unwrap_or(rest);
    let mut chars = message.chars();

    let capitalized = match chars.next() {
        Some(first) => first.to_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    };

    ui::print_error(&capitalized);
    if let Some(hint) = script_hint(&err) {
        eprintln!("{hint}");
    }

    let styled = err.render().ansi().to_string();
    if let Some((_error_line, extra)) = styled.split_once('\n') {
        /* clap renders suggestions as "tip: a similar subcommand
        exists". drop the "tip:" label and recapitalize. */
        let extra = extra
            .replace("tip:", "")
            .replace(" a similar", "A similar")
            .replace(" some similar", "Some similar");
        eprint!("{extra}");
    }

    // usage errors exit 2, matching clap.
    std::process::exit(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn shortcuts_are_all_real_subcommands() {
        /* the list documents itself and the enum dispatches. this is what
        keeps the two from drifting: adding a name to SHORTCUTS without a
        `Commands` variant fails here rather than at someone's terminal. */
        for name in commands::run::SHORTCUTS {
            assert!(
                Cli::try_parse_from(["lpm", name]).is_ok(),
                "`lpm {name}` should be a subcommand"
            );
            // and it still takes trailing arguments, like `lpm run` does
            assert!(Cli::try_parse_from(["lpm", name, "--watch"]).is_ok());
        }
    }

    #[test]
    fn every_other_script_needs_run() {
        /* the point of a fixed list: a script named `lint` is reachable as
        `lpm run lint` and nothing else, whatever lpm.toml says. */
        assert!(Cli::try_parse_from(["lpm", "lint"]).is_err());
        assert!(Cli::try_parse_from(["lpm", "run", "lint"]).is_ok());
    }

    #[test]
    fn fmt_answers_to_both_spellings() {
        // as the subcommand, either way round
        assert!(Cli::try_parse_from(["lpm", "fmt"]).is_ok());
        assert!(Cli::try_parse_from(["lpm", "format"]).is_ok());
        // and as the script name the subcommand looks for
        assert_eq!(commands::run::FMT_NAMES, ["fmt", "format"]);
    }

    #[test]
    fn shortcuts_never_shadow_a_real_command() {
        // a shortcut named after a command would silently replace it
        let commands: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|sub| sub.get_name().to_string())
            .filter(|name| !commands::run::SHORTCUTS.contains(&name.as_str()))
            .collect();
        for name in commands::run::SHORTCUTS {
            assert!(
                !commands.contains(&name.to_string()),
                "{name} collides with an existing command"
            );
        }
    }
}
