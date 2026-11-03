/*! `embr notices`: what the owners of the installed packages say about
them, from ember.lock and the indices, and the author's side, which posts
and removes notices. See `registry::notices` for where notices live. */

use crate::commands::tree;
use crate::commands::yank::as_owner;
use crate::error::Error;
use crate::net::registry;
use crate::project::lockfile::LockedPackage;
use crate::project::manifest::split_package_name;
use crate::registry::notices::{self, Item, Kind};
use crate::ui;
use clap::{Args, Subcommand, ValueEnum};
use crossterm::style::Stylize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct NoticesArgs {
    #[command(subcommand)]
    pub command: Option<NoticesCommand>,
    /// Exit with an error when an item of this kind applies, such as `--deny warnings`
    #[arg(long, value_enum, value_delimiter = ',', value_name = "KIND")]
    pub deny: Vec<Deny>,
}

#[derive(Subcommand, Debug)]
pub enum NoticesCommand {
    /// Post a notice on your package, or on a range of its versions
    Add(AddArgs),
    /// Remove a notice from your package
    Remove(RemoveArgs),
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// The package, as scope/name
    pub package: String,
    /// The text: at most 500 characters on at most 5 lines
    pub message: String,
    /// Post a warning, which shows on each install, not a notice
    #[arg(long)]
    pub warning: bool,
    /// The versions that it covers, as a semver range such as ">=1.0.0, <1.3.0"; all versions when left out
    #[arg(long, alias = "range", value_name = "RANGE")]
    pub versions: Option<String>,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// The package, as scope/name
    pub package: String,
    /// The notice's id, which `embr info` shows
    pub id: String,
}

/// a kind of item that `--deny` refuses.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deny {
    Warnings,
    Deprecations,
    Yanked,
    Notices,
}

impl Deny {
    fn kind(self) -> Kind {
        match self {
            Deny::Warnings => Kind::Warning,
            Deny::Deprecations => Kind::Deprecated,
            Deny::Yanked => Kind::Yanked,
            Deny::Notices => Kind::Notice,
        }
    }
}

pub fn run(args: NoticesArgs) -> Result<(), Error> {
    match args.command {
        Some(NoticesCommand::Add(add)) => post(add),
        Some(NoticesCommand::Remove(remove)) => take(remove),
        None => list(&args.deny),
    }
}

/** every notice, warning, deprecation and yank of the installed packages,
direct and transitive, by package. it exits 0 unless `--deny` names a
kind that applies, so CI can fail on it. */
fn list(deny: &[Deny]) -> Result<(), Error> {
    let (manifest, lock) = tree::load()?;
    let mut problems = Vec::new();
    let items = ui::with_spinner("Reading the indices", || {
        Ok(notices::gather(&lock.packages, true, &mut problems))
    })?;
    for problem in &problems {
        eprintln!("warning: embr cannot read {}", ui::printable(problem));
    }
    let by = tree::brought_in_by(&manifest, &lock, Path::new(""));
    print!("{}", describe(&lock.packages, &items, by.as_deref()));

    let denied: BTreeSet<&Item> = items
        .iter()
        .flatten()
        .filter(|item| deny.iter().any(|deny| deny.kind() == item.kind))
        .collect();
    match denied.len() {
        0 => Ok(()),
        count => Err(Error::NoticesDenied(format!(
            "{count} item{} above {} of a kind that --deny names",
            ui::plural(count),
            if count == 1 { "is" } else { "are" }
        ))),
    }
}

/// one package and version of the list.
#[derive(Default)]
struct Group<'a> {
    /// a manifest entry names it
    direct: bool,
    /// the manifest entries whose tree holds it
    by: BTreeSet<&'a String>,
    items: BTreeSet<&'a Item>,
}

/** the list text. one heading for each package and version, which names
the manifest entries that bring it in, then its items. a package in two
environments shows once. */
fn describe(packages: &[LockedPackage], items: &[Vec<Item>], by: Option<&[Vec<String>]>) -> String {
    let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
    for (index, (package, items)) in packages.iter().zip(items).enumerate() {
        if items.is_empty() {
            continue;
        }
        let key = (package.name.to_lowercase(), package.version.clone());
        let group = groups.entry(key).or_default();
        group.direct |= package.link.is_some();
        group
            .by
            .extend(by.and_then(|by| by.get(index)).into_iter().flatten());
        group.items.extend(items);
    }
    if groups.is_empty() {
        return "The installed packages have no notices, warnings, deprecations or yanks\n"
            .to_string();
    }

    let mut out = String::new();
    for ((name, version), Group { direct, by, items }) in groups {
        let from = if direct {
            " (direct)".to_string()
        } else if by.is_empty() {
            String::new()
        } else {
            let by: Vec<&str> = by.into_iter().map(String::as_str).collect();
            format!(" (through {})", by.join(", "))
        };
        out.push_str(&format!(
            "{}{}\n",
            format!("{name}@{version}").bold(),
            from.dark_grey()
        ));
        for item in items {
            out.push_str(&format!("  {}\n", item.line(false).replace('\n', "\n  ")));
        }
    }
    out
}

/// `embr notices add`: posts a notice as an owner of the package.
fn post(args: AddArgs) -> Result<(), Error> {
    let name = args.package.trim().to_lowercase();
    split_package_name(&name)?;
    let level = if args.warning { "warning" } else { "notice" };
    let versions = args.versions.as_deref().map(str::trim);
    let id = as_owner(|token| {
        ui::with_spinner("Contacting the registry", || {
            registry::post_notice(token, &name, level, &args.message, versions)
        })
    })?;
    let on = match versions.filter(|range| !range.is_empty()) {
        Some(range) => format!("{name} {range}"),
        None => name.clone(),
    };
    ui::print_success(&format!("Posted a {level} on {on}"));
    if args.warning {
        println!("Each install shows it.");
    } else {
        println!("An install that adds or updates the package shows it.");
    }
    println!("To remove it: embr notices remove {name} {id}");
    Ok(())
}

/// `embr notices remove`: removes a notice by its id.
fn take(args: RemoveArgs) -> Result<(), Error> {
    let name = args.package.trim().to_lowercase();
    split_package_name(&name)?;
    let id = args.id.trim();
    as_owner(|token| {
        ui::with_spinner("Contacting the registry", || {
            registry::remove_notice(token, &name, id)
        })
    })?;
    ui::print_success(&format!("Removed the notice {id} from {name}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::lockfile::Lockfile;

    /// drops the colour escapes, so a test reads the text alone.
    fn plain(text: &str) -> String {
        let mut out = String::new();
        let mut characters = text.chars();
        while let Some(character) = characters.next() {
            if character == '\x1b' {
                characters.by_ref().find(|&escape| escape == 'm');
            } else {
                out.push(character);
            }
        }
        out
    }

    const LOCK: &str = "version = 2\n\
        [[package]]\nname = \"acme/rocket\"\nversion = \"1.0.0\"\nenvironment = \"roblox\"\n\
        link = \"rocket\"\nindex = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n\
        [[package]]\nname = \"acme/fuel\"\nversion = \"0.2.0\"\nenvironment = \"roblox\"\n\
        index = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n\
        [[package]]\nname = \"acme/fuel\"\nversion = \"0.2.0\"\nenvironment = \"roblox\"\n\
        context = \"server\"\nindex = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n\
        [[package]]\nname = \"acme/quiet\"\nversion = \"3.0.0\"\nenvironment = \"roblox\"\n\
        link = \"quiet\"\nindex = \"i\"\ntype = \"tar-gz\"\nurl = \"u\"\n";

    fn item(kind: Kind, package: &str, version: Option<&str>, text: &str) -> Item {
        Item {
            kind,
            package: package.to_string(),
            version: version.map(str::to_string),
            text: text.to_string(),
            replacement: None,
        }
    }

    #[test]
    fn the_list_groups_by_package_and_names_what_brings_it_in() {
        let lock: Lockfile = toml::from_str(LOCK).unwrap();
        let fuel = vec![
            item(Kind::Notice, "acme/fuel", None, "docs moved"),
            item(Kind::Yanked, "acme/fuel", Some("0.2.0"), ""),
            item(Kind::Warning, "acme/fuel", Some("0.2.0"), "leaks\nupdate"),
        ];
        let rocket = vec![Item {
            replacement: Some("acme/jet".to_string()),
            ..item(Kind::Deprecated, "acme/rocket", None, "no longer kept up")
        }];
        // fuel sits in two environments, and quiet has nothing to say
        let items = vec![rocket, fuel.clone(), fuel, Vec::new()];
        let by = vec![
            vec!["rocket".to_string()],
            vec!["rocket".to_string()],
            vec!["server-tools".to_string()],
            vec!["quiet".to_string()],
        ];
        assert_eq!(
            plain(&describe(&lock.packages, &items, Some(&by))),
            "acme/fuel@0.2.0 (through rocket, server-tools)\n  \
               warning: leaks\n    update\n  \
               yanked: the owners withdrew this version, so new installs skip it\n  \
               notice: docs moved\n\
             acme/rocket@1.0.0 (direct)\n  \
               deprecated: no longer kept up, use acme/jet instead\n"
        );
        // with no installed graph, a transitive package names nothing
        let text = plain(&describe(&lock.packages, &items, None));
        assert!(text.starts_with("acme/fuel@0.2.0\n"), "{text}");
    }

    #[test]
    fn a_quiet_project_says_so() {
        let lock: Lockfile = toml::from_str(LOCK).unwrap();
        let items = vec![Vec::new(); lock.packages.len()];
        assert_eq!(
            describe(&lock.packages, &items, None),
            "The installed packages have no notices, warnings, deprecations or yanks\n"
        );
    }

    #[test]
    fn deny_takes_a_list_of_kinds() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            args: NoticesArgs,
        }
        let cli = Cli::try_parse_from(["embr", "--deny", "warnings,yanked"]).unwrap();
        assert_eq!(cli.args.deny, [Deny::Warnings, Deny::Yanked]);
        assert!(Cli::try_parse_from(["embr", "--deny", "errors"]).is_err());
        let cli = Cli::try_parse_from([
            "embr",
            "add",
            "acme/rocket",
            "--range",
            "^1",
            "--warning",
            "leaks",
        ])
        .unwrap();
        let Some(NoticesCommand::Add(add)) = cli.args.command else {
            panic!("add parses");
        };
        assert_eq!(
            (add.message.as_str(), add.warning, add.versions.as_deref()),
            ("leaks", true, Some("^1"))
        );
    }
}
