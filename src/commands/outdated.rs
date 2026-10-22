use crate::error::Error;
use crate::project::lockfile::Lockfile;
use crate::project::manifest::{
    Dependency, Language, Manifest, dependency_target, parse_version_req,
};
use crate::registry::index::{Index, Refresh};
use crate::ui;
use clap::Args;
use crossterm::style::Stylize;
use std::collections::HashMap;

#[derive(Args, Debug)]
pub struct OutdatedArgs {
    /// Fetch the indices even if they were refreshed recently
    #[arg(long)]
    pub refresh: bool,
}

/// one dependency's versions: what is locked, what its requirement allows, what is out.
struct Row {
    alias: String,
    name: String,
    current: Option<semver::Version>,
    wanted: Option<semver::Version>,
    latest: Option<semver::Version>,
}

impl Row {
    /** behind when the newest release is past what is locked. a dependency
    with nothing locked yet counts too, so a first look says what installs. */
    fn behind(&self) -> bool {
        match (&self.current, &self.latest) {
            (Some(current), Some(latest)) => current < latest,
            (None, Some(_)) => true,
            _ => false,
        }
    }
}

/** lists the registry dependencies with a newer release. it only reads:
it never installs, and never edits ember.toml or ember.lock. */
pub fn run(args: OutdatedArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    let lock = Lockfile::load().ok();
    let refresh = if args.refresh {
        Refresh::Force
    } else {
        Refresh::Ttl
    };
    let prefer = manifest
        .target()
        .map(|target| (target.environment, target.language));

    let mut indices: HashMap<String, Index> = HashMap::new();
    let mut rows = Vec::new();
    let mut aliases: Vec<&String> = manifest.dependencies.keys().collect();
    aliases.sort();
    for alias in aliases {
        let dependency = &manifest.dependencies[alias];
        // a workspace member is the copy being developed, it has no release to be behind
        let Dependency::Registry {
            name,
            version,
            index,
            ..
        } = dependency
        else {
            continue;
        };
        let name = name.to_lowercase();
        let url = manifest.index_url(index.as_deref())?.to_string();
        if !indices.contains_key(&url) {
            indices.insert(url.clone(), Index::open(&url, refresh)?);
        }
        let index = &indices[&url];
        // a dependency's `target` names an environment, the project keeps its language
        let prefer = match dependency_target(alias, dependency)? {
            Some(environment) => Some((
                environment,
                prefer.map_or(Language::Luau, |(_, language)| language),
            )),
            None => prefer,
        };
        let newest = |req: &semver::VersionReq| {
            index
                .resolve(&name, req, prefer, &[])
                .ok()
                .map(|package| package.version)
        };

        rows.push(Row {
            alias: alias.clone(),
            current: lock.as_ref().and_then(|lock| locked(lock, &name)),
            wanted: newest(&parse_version_req(version)?),
            latest: newest(&semver::VersionReq::STAR),
            name,
        });
    }

    let behind: Vec<&Row> = rows.iter().filter(|row| row.behind()).collect();
    if behind.is_empty() {
        ui::print_success("Every dependency is up to date");
        return Ok(());
    }
    print_table(&behind);
    println!();
    println!(
        "{} of {} dependencies {} a newer release. `embr install` moves to Wanted; past it, raise the requirement with `embr add <name> --version <req>`.",
        behind.len(),
        rows.len(),
        if behind.len() == 1 { "has" } else { "have" },
    );
    Ok(())
}

/// the newest version ember.lock holds for `name`, across its contexts.
fn locked(lock: &Lockfile, name: &str) -> Option<semver::Version> {
    lock.packages
        .iter()
        .filter(|package| package.name.eq_ignore_ascii_case(name))
        .filter_map(|package| semver::Version::parse(&package.version).ok())
        .max()
}

fn print_table(rows: &[&Row]) {
    let text = |version: &Option<semver::Version>| {
        version
            .as_ref()
            .map_or_else(|| "-".to_string(), ToString::to_string)
    };
    let cells: Vec<[String; 4]> = rows
        .iter()
        .map(|row| {
            [
                format!("{} ({})", row.alias, row.name),
                text(&row.current),
                text(&row.wanted),
                text(&row.latest),
            ]
        })
        .collect();
    let headers = ["Dependency", "Current", "Wanted", "Latest"];
    let widths: Vec<usize> = (0..4)
        .map(|column| {
            cells
                .iter()
                .map(|cell| cell[column].len())
                .chain([headers[column].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let pad = |text: &str, column: usize| format!("{text:<width$}", width = widths[column]);

    println!(
        "{}",
        headers
            .iter()
            .enumerate()
            .map(|(column, header)| pad(header, column))
            .collect::<Vec<_>>()
            .join("  ")
            .bold()
    );
    for (row, cell) in rows.iter().zip(&cells) {
        /* colour only what moved: Wanted when an install would change it,
        Latest when it is past what the requirement allows */
        let wanted = if row.wanted.is_some() && row.wanted != row.current {
            pad(&cell[2], 2).with(ui::accent()).to_string()
        } else {
            pad(&cell[2], 2)
        };
        // the last column takes no padding, so no line ends in spaces
        let latest = if row.latest.is_some() && row.latest != row.wanted {
            cell[3].clone().with(ui::accent()).bold().to_string()
        } else {
            cell[3].clone()
        };
        println!(
            "{}  {}  {wanted}  {latest}",
            pad(&cell[0], 0),
            pad(&cell[1], 1).dark_grey()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(current: Option<&str>, wanted: Option<&str>, latest: Option<&str>) -> Row {
        let parse = |v: Option<&str>| v.map(|v| semver::Version::parse(v).unwrap());
        Row {
            alias: "Thing".to_string(),
            name: "acme/thing".to_string(),
            current: parse(current),
            wanted: parse(wanted),
            latest: parse(latest),
        }
    }

    #[test]
    fn behind_means_a_newer_release_exists() {
        assert!(!row(Some("1.2.0"), Some("1.2.0"), Some("1.2.0")).behind());
        // the requirement allows it, an install picks it up
        assert!(row(Some("1.2.0"), Some("1.3.0"), Some("1.3.0")).behind());
        // only past the requirement
        assert!(row(Some("1.3.0"), Some("1.3.0"), Some("2.0.0")).behind());
        // nothing locked yet
        assert!(row(None, Some("1.0.0"), Some("1.0.0")).behind());
        // an index that no longer has it says nothing
        assert!(!row(Some("1.0.0"), None, None).behind());
    }
}
