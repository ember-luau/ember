mod commands;
mod error;
mod index;
mod lockfile;
mod manifest;
mod resolver;
mod ui;
mod http;
mod github;

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
    /// Add a dependency to lpm.toml
    Add(commands::add::AddArgs),

    /// Manage tooling used in the current project.
    #[command(subcommand)]
    Tool(commands::tool::ToolCommand),

    /// Install dependencies from lpm.toml into .lpm/packages
    #[command(visible_alias = "i")]
    Install(commands::install::InstallArgs),

    /// Publish this package to an index
    Publish,
    /// Manage this lpm installation
    #[command(subcommand, name = "self")]
    SelfManage(SelfCommand),
}

fn main() {
    let cli = parse_cli();

    let result = match cli.command {
        Commands::Init => commands::init::run(),
        Commands::Add(args) => commands::add::run(args),
        Commands::Install(args) => commands::install::run(args),
        Commands::Publish => commands::publish::run(),
        Commands::SelfManage(command) => commands::self_cmd::run(command),
        Commands::Tool(command) => commands::tool::run(command)
    };

    if let Err(err) = result {
        ui::print_error(&err.to_string());
        std::process::exit(1);
    }
}

/// Parses arguments, restyling clap's hardcoded "error: <message>" line as an
/// accent "✗ <message>" while keeping the styled usage/tip lines below it.
fn parse_cli() -> Cli {
    Cli::try_parse().unwrap_or_else(|err| {
        let plain = err.render().to_string();
        // Help and version output pass through clap untouched.
        let Some(rest) = plain.strip_prefix("error: ") else {
            err.exit()
        };

        // clap's messages start lowercase; capitalize to match our own errors.
        let message = rest.lines().next().unwrap_or(rest);
        let mut chars = message.chars();
        let capitalized = match chars.next() {
            Some(first) => first.to_uppercase().to_string() + chars.as_str(),
            None => String::new(),
        };
        ui::print_error(&capitalized);
        let styled = err.render().ansi().to_string();
        if let Some((_error_line, extra)) = styled.split_once('\n') {
            // clap renders suggestions as "tip: a similar subcommand exists";
            // drop the "tip:" label and re-capitalize what follows it.
            let extra = extra
                .replace("tip:", "")
                .replace(" a similar", "A similar")
                .replace(" some similar", "Some similar");
            eprint!("{extra}");
        }

        // clap uses exit code 2 for usage errors.
        std::process::exit(2)
    })
}
