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

    /// Runs a script from lpm.toml
    Run(commands::run::RunArgs),

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

    /* lpx is `lpm execute` under its own name — a copy `self install`
    drops beside lpm — so every argument belongs to execute. */
    let cli = if tools::shim::invoked_as_lpx() {
        let prefix: [std::ffi::OsString; 2] = ["lpm".into(), "execute".into()];
        parse_cli(prefix.into_iter().chain(std::env::args_os().skip(1)))
    } else {
        parse_cli(std::env::args_os())
    };

    let started = std::time::Instant::now();
    let result = match cli.command {
        Commands::Init => commands::init::run(),
        /* execute hands the terminal to another program: its exit code
        passes through as-is, with no "Done in" line in its output */
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
    };

    match result {
        Ok(()) => ui::print_elapsed(started.elapsed()),
        Err(err) => {
            ui::print_error(&err.to_string());
            std::process::exit(1);
        }
    }
}

/** Parses arguments, restyling clap's hardcoded "error: <message>" line as
an accent "✗ <message>" while keeping the styled usage/tip lines below. */
fn parse_cli(args: impl IntoIterator<Item = std::ffi::OsString>) -> Cli {
    Cli::try_parse_from(args).unwrap_or_else(|err| {
        let plain = err.render().to_string();
        // help and version output pass through clap untouched.
        let Some(rest) = plain.strip_prefix("error: ") else {
            err.exit()
        };

        // clap starts messages lowercase; capitalize to match our own errors.
        let message = rest.lines().next().unwrap_or(rest);
        let mut chars = message.chars();
        let capitalized = match chars.next() {
            Some(first) => first.to_uppercase().to_string() + chars.as_str(),
            None => String::new(),
        };
        ui::print_error(&capitalized);
        let styled = err.render().ansi().to_string();
        if let Some((_error_line, extra)) = styled.split_once('\n') {
            /* clap renders suggestions as "tip: a similar subcommand
            exists"; drop the "tip:" label and recapitalize. */
            let extra = extra
                .replace("tip:", "")
                .replace(" a similar", "A similar")
                .replace(" some similar", "Some similar");
            eprint!("{extra}");
        }

        // usage errors exit 2, matching clap.
        std::process::exit(2)
    })
}
