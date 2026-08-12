mod commands;
mod error;
mod net;
mod project;
mod registry;
mod sys;
mod tools;
mod ui;

use clap::{Parser, Subcommand};
use commands::self_cmd::SelfCommand;

#[derive(Parser, Debug)]
#[command(name = "lpm", bin_name = "lpm", version, about, styles = ui::help_styles())]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create an lpm.toml manifest in the current directory
    Init,

    /// Runs a script from lpm.toml, or lists them all when given no name
    Run(commands::run::RunArgs),

    /* the four [scripts] names that double as subcommands, npm's
    test/start/stop/restart idea. real variants rather than a fallback for
    unrecognized commands, so they appear in --help, error like any other
    command, and the set stays a decision instead of "whatever the manifest
    happens to define". the list lives in commands::run::SHORTCUTS. */
    /// Runs the `build` script from lpm.toml
    Build(commands::run::ShortcutArgs),

    /// Runs the `test` script from lpm.toml
    Test(commands::run::ShortcutArgs),

    /// Runs the `start` script from lpm.toml
    Start(commands::run::ShortcutArgs),

    /// Runs the `serve` script from lpm.toml
    Serve(commands::run::ShortcutArgs),

    /// Runs the `fmt` (or `format`) script from lpm.toml
    #[command(visible_alias = "format")]
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

    /* lpx is `lpm execute` under its own name, a copy `self install`
    drops beside lpm, so every argument belongs to execute. */
    let cli = if tools::shim::invoked_as_lpx() {
        let prefix: [std::ffi::OsString; 2] = ["lpm".into(), "execute".into()];
        parse_cli(prefix.into_iter().chain(std::env::args_os().skip(1)))
    } else {
        parse_cli(std::env::args_os())
    };

    let started = std::time::Instant::now();
    let result = match cli.command {
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

fn parse_cli(args: impl IntoIterator<Item = std::ffi::OsString>) -> Cli {
    Cli::try_parse_from(args).unwrap_or_else(|err| report_parse_error(err))
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
