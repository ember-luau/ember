/*! embr's command line: every command, and what each one runs. the embr
binary parses into it, and a host tool runs it under its own name through
[`crate::api::run`]. */

use crate::commands::{self, self_cmd::SelfCommand};
use crate::error::Error;
use crate::ui;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "embr", bin_name = "embr", version, about, styles = ui::help_styles())]
pub struct Cli {
    /** None is bare `embr`, which prints the same help `-h` does rather than
    a usage error: it is what someone types when they want to see the tool. */
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Create an ember.toml manifest in the current directory
    Init,

    /// Runs a script from ember.toml, or lists them all when given no name
    Run(commands::run::RunArgs),

    /* the [scripts] names that double as subcommands, npm's
    test/start/stop/restart idea. real variants rather than a fallback for
    unrecognized commands, so they error like any other command and the set
    stays a decision instead of "whatever the manifest happens to define".
    the list lives in commands::run::SHORTCUTS.

    hidden, npm's way: `npm start` works everywhere and is nobody's idea of a
    command to look up. they belong to a project's [scripts], not to embr's own
    surface, and listing five of them would say otherwise while telling a
    project that defines none of them about commands it cannot run. `embr run`
    lists what this project actually has, and `script_hint` points at these
    from the one place they get typed by mistake. */
    /// Runs the `build` script from ember.toml
    #[command(hide = true)]
    Build(commands::run::ShortcutArgs),

    /// Runs the `test` script from ember.toml
    #[command(hide = true)]
    Test(commands::run::ShortcutArgs),

    /// Runs the `start` script from ember.toml
    #[command(hide = true)]
    Start(commands::run::ShortcutArgs),

    /// Runs the `serve` script from ember.toml
    #[command(hide = true)]
    Serve(commands::run::ShortcutArgs),

    /// Runs the `fmt` (or `format`) script from ember.toml
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

    /// Add a dependency to ember.toml
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

    /// Install dependencies and tools from ember.toml
    #[command(visible_alias = "i")]
    Install(commands::install::InstallArgs),

    /// Publish this package to the ember registry
    Publish(commands::publish::PublishArgs),

    /// List the dependencies with a newer release; installs nothing
    Outdated(commands::outdated::OutdatedArgs),

    /// Mark a published version so new installs skip it
    Yank(commands::yank::YankArgs),

    /// Mark a package, or one version, as deprecated, with a reason
    Deprecate(commands::deprecate::DeprecateArgs),

    /// Serve package search to a coding agent, as an MCP server on stdio
    Mcp,

    /// Manage embr's caches
    #[command(subcommand)]
    Cache(commands::cache::CacheCommand),

    /// Manage this embr installation
    #[command(subcommand, name = "self")]
    SelfManage(SelfCommand),
}

/// how a command ended, which decides what its caller prints after it.
pub enum Outcome {
    /// it finished, and the caller may say how long it took
    Done,
    /// it owned stdout, as the MCP server does, so nothing may follow it
    Quiet,
    /// it handed the terminal to another program, whose exit code passes through
    Exit(i32),
}

/// runs one parsed command.
pub fn dispatch(command: Commands) -> Result<Outcome, Error> {
    let done = |result: Result<(), Error>| result.map(|()| Outcome::Done);
    match command {
        Commands::Init => done(commands::init::run()),
        /* execute hands the terminal to another program, so its exit code
        passes through as-is and no "Done in" line prints */
        Commands::Execute(args) => commands::execute::run(args).map(Outcome::Exit),
        Commands::Add(args) => done(commands::add::run(args)),
        Commands::Index(command) => done(commands::index::run(command)),
        Commands::Install(args) => done(commands::install::run(args)),
        Commands::Patch(args) => done(commands::patch::run(args)),
        Commands::Publish(args) => done(commands::publish::run(args)),
        Commands::Outdated(args) => done(commands::outdated::run(args)),
        Commands::Yank(args) => done(commands::yank::run(args)),
        Commands::Deprecate(args) => done(commands::deprecate::run(args)),
        /* the protocol owns stdout, so no "Done in" line may follow it */
        Commands::Mcp => commands::mcp::run().map(|()| Outcome::Quiet),
        Commands::Cache(command) => done(commands::cache::run(command)),
        Commands::SelfManage(command) => done(commands::self_cmd::run(command)),
        Commands::Tool(command) => done(commands::tool::run(command)),
        Commands::Studio { command } => done(commands::studio::run(command)),
        Commands::Run(args) => done(commands::run::run(args)),
        Commands::Build(args) => done(commands::run::shortcut(&["build"], args)),
        Commands::Test(args) => done(commands::run::shortcut(&["test"], args)),
        Commands::Start(args) => done(commands::run::shortcut(&["start"], args)),
        Commands::Serve(args) => done(commands::run::shortcut(&["serve"], args)),
        Commands::Fmt(args) => done(commands::run::shortcut(&commands::run::FMT_NAMES, args)),
    }
}
