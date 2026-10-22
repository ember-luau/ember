use clap::{CommandFactory, Parser};
use embr::cli::{Cli, Outcome};
use embr::{art, commands, project, tools, ui};

fn main() {
    /* tool shims are copies of embr named after their alias. started under
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

    let args: Vec<std::ffi::OsString> = if tools::shim::invoked_as_embx() {
        /* embx is `embr execute` under its own name, a copy `self install`
        drops beside embr, so every argument belongs to execute */
        let prefix: [std::ffi::OsString; 2] = ["embr".into(), "execute".into()];
        prefix
            .into_iter()
            .chain(std::env::args_os().skip(1))
            .collect()
    } else {
        std::env::args_os().collect()
    };

    /* the three ways to ask embr itself for help, caught before clap so they
    get the logo layout instead of the plain one. anything longer is a
    question about a subcommand, and clap answers those as it always has:
    `embr add -h`, `embr help tool`, `embr tool add --help`. */
    if matches!(
        args.get(1).and_then(|arg| arg.to_str()),
        Some("-h" | "--help" | "help")
    ) && args.len() == 2
    {
        print_root_help();
        return;
    }

    let cli = parse_cli(embr::cli::expand_shortcut(args));
    let Some(command) = cli.command else {
        print_root_help();
        return;
    };

    let started = std::time::Instant::now();
    let result = embr::cli::dispatch(command);

    match result {
        Ok(Outcome::Done) => ui::print_elapsed(started.elapsed()),
        Ok(Outcome::Quiet) => {}
        Ok(Outcome::Exit(code)) => std::process::exit(code),
        Err(err) => {
            ui::print_error(&err.to_string());
            std::process::exit(err.exit_code());
        }
    }
}

/// parses, reporting a failure in embr's own style rather than clap's.
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

    /* the art is for a person looking at a terminal. redirected, `embr --help`
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

    // a closed pipe (`embr | head`) is not an error
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

/** Points at `embr run <name>` when the unrecognized subcommand turns out to
be a script this project defines.

Only four script names double as subcommands, so `embr fmt` is a mistake
people will make -- and "unrecognized subcommand 'fmt'" is a bad answer when
ember.toml plainly has an `fmt` script. None for anything else, including a
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
            "'{name}' is a script in ember.toml; run it with `embr run {name}` (only {}, and a name that starts with one of them and a colon, can drop `run`)",
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
    use embr::cli::Commands;

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
                Cli::try_parse_from(["embr", name]).is_ok(),
                "`embr {name}` should be a subcommand"
            );
            // and it still takes trailing arguments, like `embr run` does
            assert!(Cli::try_parse_from(["embr", name, "--watch"]).is_ok());
        }
    }

    #[test]
    fn every_other_script_needs_run() {
        /* the point of a fixed list: a script named `lint` is reachable as
        `embr run lint` and nothing else, whatever ember.toml says. */
        assert!(Cli::try_parse_from(["embr", "lint"]).is_err());
        assert!(Cli::try_parse_from(["embr", "run", "lint"]).is_ok());
    }

    #[test]
    fn a_shortcut_and_a_colon_run_that_script() {
        let parse = |words: &[&str]| {
            let args = words.iter().map(std::ffi::OsString::from).collect();
            Cli::try_parse_from(embr::cli::expand_shortcut(args))
        };

        let Ok(Cli {
            command: Some(Commands::Run(run)),
        }) = parse(&["embr", "build:prod", "--watch"])
        else {
            panic!("`embr build:prod` should run the build:prod script");
        };
        assert_eq!(run.name.as_deref(), Some("build:prod"));
        assert_eq!(run.args, ["--watch"]);
        assert!(parse(&["embr", "format:check"]).is_ok());

        // a script that is no shortcut, an empty tail, or no colon still needs `run`
        for word in ["lint:fix", "build:", "builder"] {
            assert!(parse(&["embr", word]).is_err(), "{word}");
        }
        // and the words after the subcommand are never touched
        assert!(parse(&["embr", "run", "build:prod"]).is_ok());
    }

    #[test]
    fn fmt_answers_to_both_spellings() {
        // as the subcommand, either way round
        assert!(Cli::try_parse_from(["embr", "fmt"]).is_ok());
        assert!(Cli::try_parse_from(["embr", "format"]).is_ok());
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
