/*! `embr update`: moves packages to the newest versions that ember.toml
takes. `--dry-run` resolves the same update and shows the change to
ember.lock, with the packages that no line of ember.toml names, and writes
nothing. the list that `install --json` prints comes from here too. */

use crate::error::Error;
use crate::project::lockfile::{LockedPackage, Lockfile, lockfile_in};
use crate::project::manifest::{Environment, Manifest};
use crate::project::workspace::{self, Workspace};
use crate::registry::index::{DownloadSource, Refresh};
use crate::registry::resolver::{self, ResolvedInstall};
use crate::ui;
use clap::Args;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Packages to move, each a scope/name or its alias in ember.toml; every package when none is named
    #[arg(value_name = "PACKAGE")]
    pub packages: Vec<String>,

    /// Show each change to ember.lock, and write nothing
    #[arg(long)]
    pub dry_run: bool,

    /// Print the changes to ember.lock as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

/// one entry of an ember.lock, or of the lock that an update would write.
struct Row {
    name: String,
    /// empty for a path folder, as in ember.lock.
    version: String,
    /// the commit of a git package, which can move under one version.
    rev: Option<String>,
    /// the alias in ember.toml. a package that ember.toml does not name has none.
    link: Option<String>,
    /// the environment root that it installs under.
    context: Environment,
    /// the channel that picked the version, see [`LockedPackage::channel`].
    channel: Option<String>,
}

impl Row {
    fn new(
        name: String,
        version: String,
        source: &DownloadSource,
        link: Option<String>,
        context: Environment,
        channel: Option<String>,
    ) -> Self {
        Row {
            name,
            // the lock keeps no version for a path folder, see `DownloadSource::Path`
            version: match source {
                DownloadSource::Path { .. } => String::new(),
                _ => version,
            },
            rev: match source {
                DownloadSource::Git { rev, .. } => Some(rev.clone()),
                _ => None,
            },
            link,
            context,
            channel,
        }
    }

    fn locked(package: LockedPackage) -> Self {
        let context = package.context();
        Row::new(
            package.name,
            package.version,
            &package.source,
            package.link,
            context,
            package.channel,
        )
    }

    fn resolved(package: ResolvedInstall) -> Self {
        Row::new(
            package.name,
            package.version.to_string(),
            &package.source,
            package.link,
            package.context,
            package.channel,
        )
    }
}

/// the lock entries of the project, or of one member of its workspace.
pub(crate) struct Project {
    /// how the output names a member. None for the project itself.
    member: Option<String>,
    dir: PathBuf,
    rows: Vec<Row>,
}

/** what ember.lock holds now, for the project in the current folder and
for each member of its workspace: an install writes one lock for each. a
lock that is missing or does not parse holds nothing, as for an install. */
pub(crate) fn locked(manifest: &Manifest) -> Result<Vec<Project>, Error> {
    let mut dirs = vec![(None, PathBuf::from("."))];
    if !manifest.workspace_members().is_empty() {
        let workspace = Workspace::open(Path::new("."))?;
        for member in &workspace.members {
            if member.dir != workspace.root {
                dirs.push((Some(member.label()), member.dir.clone()));
            }
        }
    }
    Ok(dirs
        .into_iter()
        .map(|(member, dir)| {
            let rows = std::fs::read_to_string(lockfile_in(&dir))
                .ok()
                .and_then(|text| Lockfile::parse(&text).ok())
                .map_or_else(Vec::new, |lock| {
                    lock.packages.into_iter().map(Row::locked).collect()
                });
            Project { member, dir, rows }
        })
        .collect())
}

/** what an update of `update`, or of every package when it is empty, would
lock for each project of `before`. it resolves as that install does, and
writes no file of the project. an index copy that is past its age is pulled,
because the answer needs the newest entries. */
fn planned(before: &[Project], update: &[String]) -> Result<Vec<Project>, Error> {
    let mode = if update.is_empty() {
        Refresh::Force
    } else {
        Refresh::Ttl
    };
    before
        .iter()
        .map(|project| {
            workspace::in_dir(&project.dir, || {
                let manifest = Manifest::load()?;
                let mut warnings = Vec::new();
                let resolved = resolver::resolve_updating(
                    &manifest,
                    Path::new("."),
                    mode,
                    &super::install::pinned_versions(),
                    resolver::Allow::for_project(&manifest).as_ref(),
                    update,
                    &mut warnings,
                );
                for warning in &warnings {
                    eprintln!("{}", ui::printable(warning));
                }
                Ok(Project {
                    member: project.member.clone(),
                    dir: project.dir.clone(),
                    rows: resolved?.into_iter().map(Row::resolved).collect(),
                })
            })
        })
        .collect()
}

/// one lock entry that came, went, or moved.
struct Change<'a> {
    member: Option<&'a str>,
    name: &'a str,
    alias: Option<&'a str>,
    from: Option<String>,
    to: Option<String>,
    kind: &'static str,
    /// ember.toml names the package, so it has a link file.
    direct: bool,
    environment: Environment,
    /// the channel that the new version comes from.
    channel: Option<&'a str>,
}

/** how far a version moved: the first part of `from` and `to` that
differs. `other` is a move that the three numbers do not show: a
prerelease, a git commit, or a version that is no semver. */
fn kind(from: &str, to: &str) -> &'static str {
    match (semver::Version::parse(from), semver::Version::parse(to)) {
        (Ok(from), Ok(to)) if from.major != to.major => "major",
        (Ok(from), Ok(to)) if from.minor != to.minor => "minor",
        (Ok(from), Ok(to)) if from.patch != to.patch => "patch",
        _ => "other",
    }
}

/** the entries that differ between two locks of each project, and the
count of the entries that stay. an entry is a package name in an
environment root, so the packages that ember.toml does not name are in the
list. the order is the project, the root, then the name. */
fn changes<'a>(before: &'a [Project], after: &'a [Project]) -> (Vec<Change<'a>>, usize) {
    let by_key = |project: &'a Project| -> BTreeMap<(Environment, String), &'a Row> {
        project
            .rows
            .iter()
            .map(|row| ((row.context, row.name.to_lowercase()), row))
            .collect()
    };
    let mut list = Vec::new();
    let mut unchanged = 0;
    for (old, new) in before.iter().zip(after) {
        let (old, new_rows) = (by_key(old), by_key(new));
        let keys: std::collections::BTreeSet<_> = old.keys().chain(new_rows.keys()).collect();
        for key in keys {
            let (from, to, kind) = match (old.get(key), new_rows.get(key)) {
                (Some(from), Some(to)) if from.version != to.version => (
                    Some(from.version.clone()),
                    Some(to.version.clone()),
                    kind(&from.version, &to.version),
                ),
                // a git package at another commit of one version
                (Some(from), Some(to)) if from.rev != to.rev => {
                    let short =
                        |row: &Row| row.rev.as_deref().map(|rev| rev.chars().take(7).collect());
                    (short(from), short(to), "other")
                }
                (Some(_), Some(_)) => {
                    unchanged += 1;
                    continue;
                }
                (Some(from), None) => (Some(from.version.clone()), None, "removed"),
                (None, Some(to)) => (None, Some(to.version.clone()), "added"),
                (None, None) => continue,
            };
            let row = new_rows
                .get(key)
                .or(old.get(key))
                .expect("one side has the key");
            list.push(Change {
                member: new.member.as_deref(),
                name: &row.name,
                alias: row.link.as_deref(),
                from,
                to,
                kind,
                direct: row.link.is_some(),
                environment: key.0,
                channel: new_rows.get(key).and_then(|row| row.channel.as_deref()),
            });
        }
    }
    (list, unchanged)
}

/** the `--json` document of `update` and `install`: each lock entry that
differs between `before` and `after`. a change in a workspace member has
the key `member`. */
pub(crate) fn document(before: &[Project], after: &[Project], dry_run: bool) -> serde_json::Value {
    let (list, unchanged) = changes(before, after);
    let list: Vec<serde_json::Value> = list
        .iter()
        .map(|change| {
            let mut entry = serde_json::json!({
                "name": change.name,
                "alias": change.alias,
                "from": change.from,
                "to": change.to,
                "change": change.kind,
                "direct": change.direct,
                "environment": change.environment.as_str(),
            });
            if let Some(member) = change.member {
                entry["member"] = member.into();
            }
            if let Some(channel) = change.channel {
                entry["channel"] = channel.into();
            }
            entry
        })
        .collect();
    serde_json::json!({
        "version": ui::JSON_VERSION,
        "dry_run": dry_run,
        "changes": list,
        "unchanged": unchanged,
    })
}

/** prints what a dry run found, or what an update did, one root per
heading, after the summary of an install:

```text
roblox
  ↑ evaera/promise 4.0.0 → 4.1.0  minor, direct
  + acme/fuel 0.3.0  added, transitive
  - sleitnick/signal 2.0.0  removed, direct
Dry run: 3 lockfile entries would change, and 12 stay. embr wrote nothing
``` */
fn print(before: &[Project], after: &[Project], dry_run: bool) {
    use crate::ui::Stylize;

    let (list, unchanged) = changes(before, after);
    let mut heading = None;
    for change in &list {
        let here = (change.member, change.environment);
        if heading != Some(here) {
            let root = change.environment.as_str();
            match change.member {
                Some(member) => println!("{}", format!("{member}: {root}").bold()),
                None => println!("{}", root.bold()),
            }
            heading = Some(here);
        }
        let name = ui::printable(change.name);
        let moved = match (&change.from, &change.to) {
            (Some(from), Some(to)) => {
                let down = semver::Version::parse(from)
                    .ok()
                    .zip(semver::Version::parse(to).ok())
                    .is_some_and(|(from, to)| to < from);
                format!("{} {name} {from} → {to}", if down { "↓" } else { "↑" })
            }
            (None, Some(to)) => format!("{} {name} {to}", "+".with(ui::accent())),
            (Some(from), None) => format!("{} {name} {from}", "-".with(ui::rgb(ui::CRIMSON))),
            (None, None) => continue,
        };
        let mut note = format!(
            "{}, {}",
            change.kind,
            if change.direct {
                "direct"
            } else {
                "transitive"
            }
        );
        if let Some(channel) = change.channel {
            note.push_str(&format!(", channel {}", ui::printable(channel)));
        }
        println!("  {moved}  {}", note.as_str().dark_grey());
    }
    let entries = |count: usize| if count == 1 { "entry" } else { "entries" };
    let stay = if unchanged == 1 { "stays" } else { "stay" };
    match (list.len(), dry_run) {
        (0, true) => println!("Dry run: no lockfile entry would change. embr wrote nothing"),
        (0, false) => println!("No lockfile entry changed: each package is at its newest version"),
        (count, true) => println!(
            "Dry run: {count} lockfile {} would change, and {unchanged} {stay}. embr wrote nothing",
            entries(count)
        ),
        (count, false) => println!(
            "{count} lockfile {} changed, and {unchanged} {stay}",
            entries(count)
        ),
    }
}

/** the package name of each of `wanted`, which names a package of a lock
by its name or by its alias. a name that no lock has is an error: the
resolver would leave it out with no word, and the update would do nothing. */
fn names(wanted: &[String], before: &[Project]) -> Result<Vec<String>, Error> {
    wanted
        .iter()
        .map(|wanted| {
            before
                .iter()
                .flat_map(|project| &project.rows)
                .find(|row| {
                    row.name.eq_ignore_ascii_case(wanted) || row.link.as_deref() == Some(wanted)
                })
                .map(|row| row.name.clone())
                .ok_or_else(|| Error::UpdateUnknown(wanted.clone()))
        })
        .collect()
}

pub fn run(args: UpdateArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    let before = locked(&manifest)?;
    let update = names(&args.packages, &before)?;

    if args.dry_run {
        let after = planned(&before, &update)?;
        if args.json {
            return ui::print_json(&document(&before, &after, true));
        }
        print(&before, &after, true);
        return Ok(());
    }

    super::install::run(super::install::InstallArgs {
        update_all: update.is_empty(),
        update,
        json: args.json,
        ..Default::default()
    })?;
    /* the summary of an install lists the packages that ember.toml names.
    an update also moves the ones below them, so it lists each entry */
    if !args.json {
        println!();
        print(&before, &locked(&manifest)?, false);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, version: &str, link: Option<&str>) -> Row {
        Row {
            name: name.to_string(),
            version: version.to_string(),
            rev: None,
            link: link.map(str::to_string),
            context: Environment::Luau,
            channel: None,
        }
    }

    fn project(rows: Vec<Row>) -> Vec<Project> {
        vec![Project {
            member: None,
            dir: PathBuf::from("."),
            rows,
        }]
    }

    #[test]
    fn the_first_number_that_differs_names_the_move() {
        assert_eq!(kind("1.2.3", "2.0.0"), "major");
        assert_eq!(kind("1.2.3", "1.3.0"), "minor");
        assert_eq!(kind("1.2.3", "1.2.4"), "patch");
        assert_eq!(kind("1.2.3-beta.1", "1.2.3"), "other");
        assert_eq!(kind("", "1.0.0"), "other");
    }

    #[test]
    fn a_change_lists_the_packages_that_the_manifest_does_not_name() {
        let before = project(vec![
            row("acme/rocket", "1.0.0", Some("rocket")),
            row("acme/fuel", "0.2.0", None),
            row("acme/gone", "1.0.0", None),
            row("acme/same", "1.0.0", Some("same")),
        ]);
        let mut moved_commit = row("acme/git", "1.0.0", None);
        moved_commit.rev = Some("0123456789abcdef".to_string());
        let after = project(vec![
            row("acme/rocket", "1.1.0", Some("rocket")),
            row("acme/fuel", "0.3.0", None),
            row("acme/new", "2.0.0", None),
            row("acme/same", "1.0.0", Some("same")),
        ]);
        let (list, unchanged) = changes(&before, &after);
        let shown: Vec<_> = list
            .iter()
            .map(|change| (change.name, change.kind, change.direct))
            .collect();
        assert_eq!(
            shown,
            [
                ("acme/fuel", "minor", false),
                ("acme/gone", "removed", false),
                ("acme/new", "added", false),
                ("acme/rocket", "minor", true),
            ]
        );
        assert_eq!(unchanged, 1);

        // one version, another commit
        let mut old_commit = row("acme/git", "1.0.0", None);
        old_commit.rev = Some("fedcba9876543210".to_string());
        let (before, after) = (project(vec![old_commit]), project(vec![moved_commit]));
        let (list, _) = changes(&before, &after);
        assert_eq!(list[0].from.as_deref(), Some("fedcba9"));
        assert_eq!(list[0].to.as_deref(), Some("0123456"));
        assert_eq!(list[0].kind, "other");
    }
}
