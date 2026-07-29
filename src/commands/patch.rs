/*!
`lpm patch`: edit a dependency's source and keep the edit. `lpm patch
scope/name` extracts a pristine working copy under .lpm-patch/ (its own
little git repo, so the published state is the baseline commit), the user
edits it, `lpm patch commit` diffs it into patches/ and records the file
under [patches] in lpm.toml, and every later install re-applies it. pnpm's
patch/patch-commit and pesde's [patches] are the models.

the working copy is the archive as published (download + flatten), NOT the
installed folder: rojo mirroring, require rewriting, and nested links all
run after patching at install time, so patches survive changes to lpm's own
transforms. that also means the copy won't be byte-identical to what sits
in packages/<env>/.lpm/.
*/

use crate::error::Error;
use crate::project::lockfile::Lockfile;
use crate::project::manifest::edit::{ManifestDoc, Scope};
use crate::project::manifest::{Manifest, split_package_name};
use crate::project::package;
use crate::registry::index::{self, CachePolicy, DownloadSource};
use crate::registry::resolver;
use crate::sys::git;
use crate::ui;
use clap::{Args, Subcommand};
use std::fs;
use std::path::{Path, PathBuf};

/// where working copies live, relative to the project root.
const WORK_DIR: &str = ".lpm-patch";

/// where committed patch files live, relative to the project root.
const PATCHES_DIR: &str = "patches";

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct PatchArgs {
    /// Package to patch, as scope/name (optionally @version)
    #[arg(required = true)]
    pub spec: Option<String>,

    /// Replace an existing working copy with fresh published bytes
    #[arg(long)]
    pub force: bool,

    #[command(subcommand)]
    pub command: Option<PatchCommand>,
}

#[derive(Subcommand, Debug)]
pub enum PatchCommand {
    /// Diff the working copy into patches/ and record it in lpm.toml
    Commit {
        /// Package the working copy belongs to, as scope/name (optionally @version)
        spec: String,
    },
    /// Drop the [patches] entry and delete the patch file
    Remove {
        /// Package to unpatch, as scope/name (optionally @version)
        spec: String,
    },
}

pub fn run(args: PatchArgs) -> Result<(), Error> {
    match args.command {
        Some(PatchCommand::Commit { spec }) => commit(&spec),
        Some(PatchCommand::Remove { spec }) => remove(&spec),
        None => {
            /* `required = true` + subcommand_negates_reqs means clap
            enforces this; the fallback stays graceful just in case */
            let Some(spec) = args.spec else {
                return Err(Error::PatchSpecInvalid {
                    spec: String::new(),
                    reason: "name a package to patch, e.g. `lpm patch scope/name`".to_string(),
                });
            };
            start(&spec, args.force)
        }
    }
}

/// "scope/name" or "scope/name@1.2.3" -> (name, exact version if given).
fn parse_spec(spec: &str) -> Result<(String, Option<semver::Version>), Error> {
    let invalid = |reason: String| Error::PatchSpecInvalid {
        spec: spec.to_string(),
        reason,
    };
    let (name, version) = match spec.split_once('@') {
        Some((name, version)) => {
            let version = semver::Version::parse(version).map_err(|error| {
                invalid(format!("'{version}' is not an exact version ({error})"))
            })?;
            (name, Some(version))
        }
        None => (spec, None),
    };
    if split_package_name(name).is_err() {
        return Err(invalid(format!(
            "'{name}' is not a 'scope/name' package name"
        )));
    }
    Ok((name.to_string(), version))
}

/** what one resolved copy of the package looks like: its exact version and
the archive it downloads from. */
struct Located {
    version: String,
    source: DownloadSource,
}

/** finds the package in this project: the lockfile when there is one (no
network), a fresh resolution otherwise. exactly one archive must come out
of it — several versions can't happen per install, but a multi-target
pesde package under two roots can, and that's refused (same rule the
install-side check enforces). */
fn locate(name: &str, version: Option<&semver::Version>) -> Result<Located, Error> {
    let mut candidates: Vec<Located> = if Path::new(crate::project::lockfile::LOCKFILE).exists() {
        Lockfile::load()?
            .packages
            .into_iter()
            .filter(|package| package.name == name)
            .map(|package| Located {
                version: package.version,
                source: package.source,
            })
            .collect()
    } else {
        let manifest = Manifest::load()?;
        let mut warnings = Vec::new();
        let resolved = ui::with_spinner("Resolving dependencies", || {
            resolver::resolve(
                &manifest,
                Path::new("."),
                index::Refresh::Ttl,
                &mut warnings,
            )
        })?;
        for warning in &warnings {
            eprintln!("{warning}");
        }
        resolved
            .into_iter()
            .filter(|package| package.name == name)
            .map(|package| Located {
                version: package.version.to_string(),
                source: package.source,
            })
            .collect()
    };

    if candidates.is_empty() {
        return Err(Error::PatchSpecInvalid {
            spec: name.to_string(),
            reason: format!(
                "{name} is not a dependency of this project (run `lpm install` first?)"
            ),
        });
    }
    if let Some(version) = version {
        let version = version.to_string();
        candidates.retain(|candidate| candidate.version == version);
        if candidates.is_empty() {
            return Err(Error::PatchSpecInvalid {
                spec: format!("{name}@{version}"),
                reason: format!(
                    "{name} resolved to a different version; drop the @version or update it"
                ),
            });
        }
    }

    /* contexts resolve independently, so one name can land at different
    versions under different roots; which one gets patched is the user's
    call, not a coin flip */
    let versions: std::collections::BTreeSet<&str> = candidates
        .iter()
        .map(|candidate| candidate.version.as_str())
        .collect();
    if versions.len() > 1 {
        return Err(Error::PatchAmbiguous {
            spec: name.to_string(),
            what: "resolved version",
            matches: versions
                .into_iter()
                .map(|version| format!("{name}@{version}"))
                .collect::<Vec<_>>()
                .join(", "),
        });
    }

    // and the one version must mean one archive
    let mut urls = std::collections::BTreeSet::new();
    for candidate in &candidates {
        match &candidate.source {
            DownloadSource::Zip { url } | DownloadSource::TarGz { url } => {
                urls.insert(url.clone());
            }
            DownloadSource::Workspace { .. } => {
                return Err(Error::PatchSpecInvalid {
                    spec: name.to_string(),
                    reason: format!(
                        "{name} is a workspace member; edit its source directly instead of patching"
                    ),
                });
            }
        }
    }
    if urls.len() > 1 {
        let mut urls = urls.into_iter();
        return Err(Error::PatchMultiTarget {
            name: name.to_string(),
            version: candidates[0].version.clone(),
            first: urls.next().unwrap_or_default(),
            second: urls.next().unwrap_or_default(),
        });
    }

    Ok(candidates.swap_remove(0))
}

/// `chief/traits` + `0.2.0` -> `chief_traits@0.2.0`, the folder and file stem both commands use.
fn slug(name: &str, version: &str) -> String {
    format!("{}@{version}", name.replace('/', "_"))
}

/** `lpm patch <spec>`: extract the published bytes into a working copy and
hand the path to the user. */
fn start(spec: &str, force: bool) -> Result<(), Error> {
    let (name, version) = parse_spec(spec)?;
    if !git::available() {
        return Err(Error::GitMissing);
    }
    let located = locate(&name, version.as_ref())?;

    let dir = Path::new(WORK_DIR).join(slug(&name, &located.version));
    if dir.exists() {
        if !force {
            return Err(Error::PatchCopyExists {
                key: format!("{name}@{}", located.version),
                path: dir.display().to_string(),
            });
        }
        fs::remove_dir_all(&dir)?;
    }

    /* published bytes + flatten: exactly the tree install_one patches, and
    the archive cache usually makes this free */
    index::download(&located.source, &dir, CachePolicy::Use)?;
    package::flatten_single_dir(&dir)?;

    /* its own git repo with the published state as the baseline commit:
    `commit` later diffs against it and gets standard a/ b/ headers, which
    `git apply` at install consumes with no path juggling */
    let in_copy = dir.to_string_lossy().into_owned();
    let git_run = |action: &'static str, args: &[&str]| {
        git::run(&git::verbatim(args)).map_err(|stderr| Error::PatchGitFailed { action, stderr })
    };
    git_run("init", &["-C", &in_copy, "init", "--quiet"])?;
    git_run("add", &["-C", &in_copy, "add", "-A"])?;
    git_run(
        "commit",
        &[
            "-C",
            &in_copy,
            /* a baseline commit needs an identity and must dodge user
            hooks/signing config; the working copy isn't a real repo */
            "-c",
            "user.name=lpm",
            "-c",
            "user.email=lpm@luaupm.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--no-verify",
            "-m",
            "published state",
        ],
    )?;

    ui::print_success(&format!(
        "Extracted {name}@{} to {}",
        located.version,
        dir.display()
    ));
    println!("Edit it, then run `lpm patch commit {name}` to save the changes as a patch");
    remind_gitignore();
    Ok(())
}

/** `lpm patch commit <spec>`: diff the working copy against the published
baseline, write the patch, record it, clean up. */
fn commit(spec: &str) -> Result<(), Error> {
    let (name, version) = parse_spec(spec)?;
    if !git::available() {
        return Err(Error::GitMissing);
    }

    /* the version comes from the working copy's own folder name, so a
    re-resolve can't quietly retarget the patch at different bytes */
    let (dir, version) = find_working_copy(&name, version.as_ref())?;
    let in_copy = dir.to_string_lossy().into_owned();

    /* intent-to-add makes files the user created show up in the diff;
    without it `git diff` only sees tracked paths */
    git::run(&git::verbatim(&["-C", &in_copy, "add", "-N", "."])).map_err(|stderr| {
        Error::PatchGitFailed {
            action: "add",
            stderr,
        }
    })?;
    // --binary so image/model assets survive the round trip
    let diff = match git::capture_diff(&git::verbatim(&["-C", &in_copy, "diff", "--binary"])) {
        Ok(diff) => diff,
        Err(git::GitError::Missing) => return Err(Error::GitMissing),
        Err(git::GitError::Failed(stderr)) => {
            return Err(Error::PatchGitFailed {
                action: "diff",
                stderr,
            });
        }
    };

    if diff.trim().is_empty() {
        println!(
            "No changes in {}; nothing to commit (the working copy is untouched)",
            dir.display()
        );
        return Ok(());
    }

    fs::create_dir_all(PATCHES_DIR)?;
    let file = Path::new(PATCHES_DIR).join(format!("{}.patch", slug(&name, &version)));
    fs::write(&file, &diff)?;

    // forward slashes: the path lands in lpm.toml and should read the same everywhere
    let recorded = format!("{PATCHES_DIR}/{}.patch", slug(&name, &version));
    let mut document = ManifestDoc::open(Scope::Project)?;
    document
        .table_or_create("patches")?
        .insert(&format!("{name}@{version}"), toml_edit::value(&recorded));
    document.save()?;

    fs::remove_dir_all(&dir)?;

    ui::print_success(&format!("Patched {name}@{version} -> {recorded}"));
    println!("Run `lpm install` to apply it");
    Ok(())
}

/** `lpm patch remove <spec>`: drop the entry and the file. */
fn remove(spec: &str) -> Result<(), Error> {
    let (name, version) = parse_spec(spec)?;

    let mut document = ManifestDoc::open(Scope::Project)?;
    let Some(table) = document.table("patches")? else {
        return Err(Error::PatchSpecInvalid {
            spec: spec.to_string(),
            reason: "there is no [patches] table in lpm.toml".to_string(),
        });
    };

    let prefix = format!("{name}@");
    let mut keys: Vec<String> = table
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| match &version {
            Some(version) => *key == format!("{name}@{version}"),
            None => key.starts_with(&prefix),
        })
        .collect();
    match keys.len() {
        0 => {
            return Err(Error::PatchSpecInvalid {
                spec: spec.to_string(),
                reason: format!("no [patches] entry matches '{spec}'"),
            });
        }
        1 => {}
        _ => {
            return Err(Error::PatchAmbiguous {
                spec: spec.to_string(),
                what: "[patches] entry",
                matches: keys.join(", "),
            });
        }
    }
    let key = keys.remove(0);
    let file = table
        .get(&key)
        .and_then(|value| value.as_str())
        .map(str::to_string);
    table.remove(&key);
    document.drop_if_empty("patches");
    document.save()?;

    match file {
        /* only delete what a valid entry could point at: a hand-edited
        absolute or escaping path must not become an rm outside the project */
        Some(file) if crate::project::manifest::patch_path(&key, &file).is_err() => {
            eprintln!(
                "warning: '{file}' is not a project-relative path; entry removed, file left alone"
            );
            ui::print_success(&format!("Removed patch {key}"));
        }
        Some(file) if Path::new(&file).exists() => {
            fs::remove_file(&file)?;
            ui::print_success(&format!("Removed patch {key} and deleted {file}"));
        }
        Some(file) => {
            ui::print_success(&format!("Removed patch {key} ({file} was already gone)"));
        }
        None => ui::print_success(&format!("Removed patch {key}")),
    }
    Ok(())
}

/** the working copy `spec` names: exact when a version was given, the only
`scope_name@*` folder otherwise (several -> ask for the version). */
fn find_working_copy(
    name: &str,
    version: Option<&semver::Version>,
) -> Result<(PathBuf, String), Error> {
    if let Some(version) = version {
        let dir = Path::new(WORK_DIR).join(slug(name, &version.to_string()));
        if !dir.is_dir() {
            return Err(Error::PatchCopyMissing(format!("{name}@{version}")));
        }
        return Ok((dir, version.to_string()));
    }

    let prefix = format!("{}@", name.replace('/', "_"));
    let mut matches: Vec<(PathBuf, String)> = Vec::new();
    if let Ok(entries) = fs::read_dir(WORK_DIR) {
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let folder = entry.file_name().to_string_lossy().into_owned();
            if let Some(version) = folder.strip_prefix(&prefix) {
                matches.push((entry.path(), version.to_string()));
            }
        }
    }
    match matches.len() {
        0 => Err(Error::PatchCopyMissing(name.to_string())),
        1 => Ok(matches.remove(0)),
        _ => Err(Error::PatchAmbiguous {
            spec: name.to_string(),
            what: "working copy",
            matches: matches
                .into_iter()
                .map(|(_, version)| format!("{name}@{version}"))
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

/** `lpm init` git-ignores .lpm-patch/ for new projects; existing ones get
this nudge once a working copy appears. */
fn remind_gitignore() {
    let Ok(contents) = fs::read_to_string(".gitignore") else {
        println!("Tip: add `{WORK_DIR}/` to .gitignore (working copies are scratch space)");
        return;
    };
    let ignored = contents.lines().map(str::trim).any(|line| {
        matches!(
            line,
            ".lpm-patch" | ".lpm-patch/" | "/.lpm-patch" | "/.lpm-patch/"
        )
    });
    if !ignored {
        println!("Tip: add `{WORK_DIR}/` to .gitignore (working copies are scratch space)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_with_and_without_a_version() {
        let (name, version) = parse_spec("chief/traits").unwrap();
        assert_eq!(name, "chief/traits");
        assert!(version.is_none());

        let (name, version) = parse_spec("chief/traits@0.2.0").unwrap();
        assert_eq!(name, "chief/traits");
        assert_eq!(version.unwrap().to_string(), "0.2.0");

        for bad in [
            "traits",
            "chief/traits@^1",
            "chief/traits@",
            "Chief/T@1.0.0",
        ] {
            assert!(
                matches!(parse_spec(bad), Err(Error::PatchSpecInvalid { .. })),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn slugs_join_name_and_version() {
        assert_eq!(slug("chief/traits", "0.2.0"), "chief_traits@0.2.0");
        assert_eq!(slug("acme/foo", "1.0.0+build.5"), "acme_foo@1.0.0+build.5");
    }

    /** the whole workstream-1 contract in one round trip: extract a tree,
    baseline it, edit, diff, apply the diff to a fresh copy of the same
    tree, end up with the edit. guarded on git like the index fixtures. */
    #[test]
    fn diffs_round_trip_through_git_apply() {
        if !git::available() {
            return;
        }
        let base = std::env::temp_dir().join("lpm-test-patch-roundtrip");
        let _ = fs::remove_dir_all(&base);

        let write = |dir: &Path, file: &str, contents: &str| {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        };
        // the "published" tree, twice: one to edit, one to apply onto
        for copy in ["work", "fresh"] {
            let dir = base.join(copy);
            write(&dir, "src/init.luau", "return { value = 1 }\n");
            write(&dir, "README.md", "hello\n");
        }

        let work = base.join("work");
        let in_work = work.to_string_lossy().into_owned();
        /* every step through `verbatim`, exactly as start/commit/apply run
        it: a machine with core.autocrlf=true (git for windows' default)
        would otherwise clean the recorded diff and smudge CRLF back on
        apply, and the round trip would stop being byte-exact */
        git::run(&git::verbatim(&["-C", &in_work, "init", "--quiet"])).unwrap();
        git::run(&git::verbatim(&["-C", &in_work, "add", "-A"])).unwrap();
        git::run(&git::verbatim(&[
            "-C",
            &in_work,
            "-c",
            "user.name=lpm",
            "-c",
            "user.email=lpm@luaupm.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--no-verify",
            "-m",
            "published state",
        ]))
        .unwrap();

        // the edit: change a file, add a new one
        write(&work, "src/init.luau", "return { value = 2 } -- patched\n");
        write(&work, "src/extra.luau", "return {}\n");
        git::run(&git::verbatim(&["-C", &in_work, "add", "-N", "."])).unwrap();
        let diff =
            git::capture_diff(&git::verbatim(&["-C", &in_work, "diff", "--binary"])).unwrap();
        assert!(diff.contains("-- patched"), "{diff}");
        assert!(diff.contains("extra.luau"), "{diff}");

        // standard a/ b/ headers apply at default -p1 in a plain directory
        let patch_file = base.join("the.patch");
        fs::write(&patch_file, &diff).unwrap();
        let fresh = base.join("fresh");
        git::run(&git::verbatim(&[
            "-C",
            &fresh.to_string_lossy(),
            "apply",
            &patch_file.to_string_lossy(),
        ]))
        .unwrap();
        assert_eq!(
            fs::read_to_string(fresh.join("src/init.luau")).unwrap(),
            "return { value = 2 } -- patched\n"
        );
        assert_eq!(
            fs::read_to_string(fresh.join("src/extra.luau")).unwrap(),
            "return {}\n"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
