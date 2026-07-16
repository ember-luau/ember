mod commands;
mod error;
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
    /// Manage this lpm installation
    #[command(subcommand, name = "self")]
    SelfManage(SelfCommand),
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Init => commands::init::run(),
        Commands::SelfManage(command) => commands::self_cmd::run(command),
    };

    if let Err(err) = result {
        use crossterm::style::{Color, Stylize};

        let (r, g, b) = ui::ACCENT;
        eprintln!("{}", format!("✗ {err}").with(Color::Rgb { r, g, b }));
        std::process::exit(1);
    }
}
