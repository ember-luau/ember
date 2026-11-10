use crate::commands::init::{parse_environment, parse_language};
use crate::commands::mcp;
use crate::error::Error;
use crate::net::registry;
use crate::project::manifest::{Environment, Language};
use clap::Args;
use crossterm::style::Stylize;

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Text to find in package names and descriptions
    pub query: String,
    /// Only packages for this environment, such as roblox, roblox_server, or lune
    #[arg(long, value_parser = parse_environment, conflicts_with = "all")]
    pub environment: Option<Environment>,
    /// Only packages in this language: luau, alloy, loom, or luwu
    #[arg(long, value_parser = parse_language, conflicts_with = "all")]
    pub language: Option<Language>,
    /// Most packages to list, from 1 to 50, and from each registry with --all
    #[arg(long)]
    pub limit: Option<usize>,
    /// Also search Wally, pesde, Forest, and LuaRocks
    #[arg(long)]
    pub all: bool,
    /// Print the results as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

/** the same answer that the `embr mcp` search tools give, painted for a
terminal. one answer for both keeps them from drifting apart. */
pub fn run(args: SearchArgs) -> Result<(), Error> {
    if args.json {
        return crate::ui::print_json(&document(args)?);
    }
    let text = if args.all {
        let args = mcp::SearchAllArgs {
            query: args.query,
            registries: None,
            limit: args.limit,
        };
        mcp::search_registries(&args, mcp::fetch, mcp::REGISTRY_TIMEOUT)
    } else {
        let args = mcp::SearchArgs {
            query: args.query,
            environment: args.environment,
            language: args.language,
            limit: args.limit,
        };
        let page = mcp::ember_search(
            &registry::api_url(),
            &args.query,
            args.environment,
            args.language,
        )?;
        mcp::search(&page, &args)
    };
    for (row, line) in text.lines().enumerate() {
        println!("{}", paint(row, line));
    }
    Ok(())
}

/** the `--json` answer: the same registries and packages as the text,
from the same requests. without `--all` it holds the ember registry only. */
fn document(args: SearchArgs) -> Result<serde_json::Value, Error> {
    if args.all {
        let args = mcp::SearchAllArgs {
            query: args.query,
            registries: None,
            limit: args.limit,
        };
        let answers = mcp::ask_registries(&args, mcp::fetch, mcp::REGISTRY_TIMEOUT);
        return Ok(mcp::search_document(answers, mcp::all_limit(&args)));
    }
    let page = mcp::ember_search(
        &registry::api_url(),
        &args.query,
        args.environment,
        args.language,
    )?;
    let limit = args
        .limit
        .unwrap_or(mcp::DEFAULT_LIMIT)
        .clamp(1, mcp::MAX_LIMIT);
    let answers = vec![(mcp::Registry::Ember, Ok((page.hits(), page.total)))];
    Ok(mcp::search_document(answers, limit))
}

/** one line of a search answer, styled the way `embr outdated` styles its
table. the first line counts the matches. a package line is
`- <name and version> | <facts> | install: <command>`, and its name takes
bold and its command the accent. */
fn paint(row: usize, line: &str) -> String {
    if row == 0 {
        return line.bold().to_string();
    }
    let Some((head, rest)) = line
        .strip_prefix("- ")
        .and_then(|line| line.split_once(" | "))
    else {
        return line.to_string();
    };
    let (facts, command) = match rest.rsplit_once("install: ") {
        Some((facts, command)) => (facts, command),
        None => (rest, ""),
    };
    format!(
        "{} {}{}{}",
        head.bold(),
        facts.replace(" | ", &" | ".dark_grey().to_string()),
        "install: ".dark_grey(),
        command.with(crate::ui::accent())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_line_keeps_its_words_and_loses_its_pipes_to_grey() {
        let line =
            "- chief/core 0.3.0 | roblox | 12 downloads | Core | install: embr add chief/core";
        let painted = paint(1, line);
        assert!(painted.contains("chief/core 0.3.0"), "{painted}");
        assert!(painted.contains("embr add chief/core"), "{painted}");
        assert!(painted.contains("12 downloads"), "{painted}");
        assert!(!painted.starts_with("- "), "{painted}");

        // the count line, a hint, and a failed registry keep their text
        assert!(paint(0, "3 of 3 matching packages").contains("3 of 3"));
        assert_eq!(paint(4, "Failed registries:"), "Failed registries:");
        assert_eq!(paint(5, "- forest: timed out"), "- forest: timed out");
    }
}
