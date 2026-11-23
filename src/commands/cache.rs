use crate::error::Error;
use crate::registry::index;
use crate::sys::paths;
use crate::ui;
use clap::{Args, Subcommand};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Subcommand, Debug)]
pub enum CacheCommand {
    /// Show the size and the entry count of each cache under ~/.ember
    Info {
        /// Print the sizes as one JSON document on stdout
        #[arg(long)]
        json: bool,
    },
    /// Check each cached archive against its sidecar; deletes nothing, and exits 1 when an archive is bad
    Verify {
        /// Print the result as one JSON document on stdout
        #[arg(long)]
        json: bool,
    },
    /// Delete the downloaded-archive, index, git and tool shorthand caches under ~/.ember, or only the ones that the flags select
    Clean(CleanArgs),
}

#[derive(Args, Debug)]
pub struct CleanArgs {
    /// Delete the downloaded archives
    #[arg(long)]
    pub archives: bool,
    /// Delete the index clones
    #[arg(long)]
    pub indexes: bool,
    /// Delete the clones of the git dependencies
    #[arg(long)]
    pub git: bool,
    /// Delete only the archives that `embr cache verify` reports
    #[arg(long)]
    pub bad: bool,
    /// Delete only the entries that no command wrote in this many days
    #[arg(long, value_name = "DAYS")]
    pub older_than: Option<u64>,
    /// Print what would go and its size, and delete nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Print the result as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

impl CacheCommand {
    /// whether the run prints one JSON document, so no other line may follow it.
    pub fn is_json(&self) -> bool {
        match self {
            CacheCommand::Info { json } | CacheCommand::Verify { json } => *json,
            CacheCommand::Clean(args) => args.json,
        }
    }
}

/// runs the command. false is a `verify` that found a bad archive, which exits 1.
pub fn run(command: CacheCommand) -> Result<bool, Error> {
    match command {
        CacheCommand::Info { json } => info(json).map(|()| true),
        CacheCommand::Verify { json } => verify(json),
        CacheCommand::Clean(args) => clean(&args).map(|()| true),
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// the top-level entries of a cache folder.
fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    entries
}

fn size(path: &Path) -> u64 {
    match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => dir_size(path),
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}

fn info(json: bool) -> Result<(), Error> {
    let shorthands = paths::shorthands_file()?;
    let rows = [
        (
            "archives",
            paths::archive_cache_root()?,
            index::cached_archives().len(),
        ),
        (
            "indexes",
            paths::index_cache_dir()?,
            entries(&paths::index_cache_dir()?).len(),
        ),
        (
            "git",
            paths::git_cache_dir()?,
            entries(&paths::git_cache_dir()?).len(),
        ),
        (
            "shorthands",
            shorthands.clone(),
            usize::from(shorthands.is_file()),
        ),
    ];
    let total: u64 = rows.iter().map(|(_, path, _)| size(path)).sum();
    if json {
        let caches: Vec<_> = rows
            .iter()
            .map(|(name, path, count)| {
                json!({ "name": name, "path": path, "entries": count, "bytes": size(path) })
            })
            .collect();
        return ui::print_json(
            &json!({ "version": ui::JSON_VERSION, "caches": caches, "bytes": total }),
        );
    }
    for (name, path, count) in &rows {
        println!(
            "{name:<10}  {:>10}  {count:>5} {}  {}",
            mib(size(path)),
            if *count == 1 { "entry  " } else { "entries" },
            path.display()
        );
    }
    println!("{:<10}  {:>10}", "total", mib(total));
    Ok(())
}

/** checks each archive against its sidecars, see [`index::check_archive`].
it reports a bad entry and leaves it: `embr cache clean --bad` deletes. */
fn verify(json: bool) -> Result<bool, Error> {
    let archives = index::cached_archives();
    let mut bad = Vec::new();
    let mut without_sha256 = 0;
    for archive in &archives {
        let check = index::check_archive(&archive.path);
        match check.bad {
            Some(reason) => bad.push((&archive.path, check.url, reason)),
            None if !check.sha256 => without_sha256 += 1,
            None => {}
        }
    }
    if json {
        let rows: Vec<_> = bad
            .iter()
            .map(|(path, url, reason)| json!({ "path": path, "url": url, "reason": reason }))
            .collect();
        ui::print_json(&json!({
            "version": ui::JSON_VERSION,
            "checked": archives.len(),
            "without_sha256": without_sha256,
            "bad": rows,
        }))?;
        return Ok(bad.is_empty());
    }
    for (path, url, reason) in &bad {
        let of = url
            .as_ref()
            .map(|url| format!(" ({})", ui::printable(url)))
            .unwrap_or_default();
        ui::print_error(&format!("{}{of}: {reason}", path.display()));
    }
    let count = archives.len();
    if without_sha256 > 0 {
        println!(
            "No SHA-256 record: {without_sha256} of {count}, because an older embr stored them"
        );
    }
    if bad.is_empty() {
        ui::print_success(&format!("Checked {count} archive{}", ui::plural(count)));
    } else {
        println!(
            "{} of {count} archive{} did not pass. `embr cache clean --bad` deletes them.",
            bad.len(),
            ui::plural(count)
        );
    }
    Ok(bad.is_empty())
}

/// one thing that `clean` deletes.
struct Item {
    cache: &'static str,
    path: PathBuf,
    bytes: u64,
    /// an entry of the archive cache: the archive and its sidecars.
    archive: bool,
}

/// the newest write to `path`, or to a file or folder directly in it.
fn modified(path: &Path) -> Option<SystemTime> {
    let of = |path: &Path| fs::metadata(path).and_then(|meta| meta.modified()).ok();
    entries(path)
        .iter()
        .filter_map(|path| of(path))
        .chain(of(path))
        .max()
}

/** the caches go: archives, index clones, git dependency clones and the registry's tool
shorthands. removing the index root also sweeps orphaned index dirs left
behind by older embr versions that keyed them differently. everything here
re-downloads on demand.

a flag narrows that. `--archives`, `--indexes` and `--git` name the caches.
`--bad` and `--older-than` are filters: an entry goes when it passes each
filter that is given. `--bad` reads only archives, so it names that cache
when no other flag names one. */
fn clean(args: &CleanArgs) -> Result<(), Error> {
    let named = args.archives || args.indexes || args.git || args.bad;
    let old = |modified: Option<SystemTime>| match args.older_than {
        None => true,
        Some(days) => modified
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= Duration::from_secs(days.saturating_mul(24 * 60 * 60))),
    };
    let whole = |cache, path: PathBuf| Item {
        cache,
        bytes: size(&path),
        path,
        archive: false,
    };

    let mut items = Vec::new();
    if args.bad || args.older_than.is_some() {
        if args.archives || args.bad || !named {
            for archive in index::cached_archives() {
                if old(archive.modified)
                    && (!args.bad || index::check_archive(&archive.path).bad.is_some())
                {
                    items.push(Item {
                        cache: "archives",
                        path: archive.path,
                        bytes: archive.bytes,
                        archive: true,
                    });
                }
            }
        }
    } else if args.archives || !named {
        items.push(whole("archives", paths::archive_cache_root()?));
    }
    for (cache, selected, root) in [
        ("indexes", args.indexes, paths::index_cache_dir()?),
        ("git", args.git, paths::git_cache_dir()?),
    ] {
        if !(selected || !named) {
            continue;
        }
        match args.older_than {
            None => items.push(whole(cache, root)),
            Some(_) => items.extend(
                entries(&root)
                    .into_iter()
                    .filter(|path| old(modified(path)))
                    .map(|path| whole(cache, path)),
            ),
        }
    }
    let shorthands = paths::shorthands_file()?;
    if !named && old(modified(&shorthands)) {
        items.push(whole("shorthands", shorthands));
    }
    /* a cache that is not there is nothing to delete. a cache folder that
    is a link, or that sits in one, leads to files that are not cache
    entries, so it stays */
    items.retain(|item| {
        item.archive
            || (item.path.exists()
                && !paths::is_symlink(&item.path)
                && !item.path.parent().is_some_and(paths::is_symlink))
    });

    let freed: u64 = items.iter().map(|item| item.bytes).sum();
    if !args.dry_run {
        for item in &items {
            if item.archive {
                index::evict_entry(&item.path);
            } else if item.path.is_dir() {
                fs::remove_dir_all(&item.path)?;
            } else {
                fs::remove_file(&item.path)?;
            }
        }
    }

    if args.json {
        let rows: Vec<_> = items
            .iter()
            .map(|item| json!({ "cache": item.cache, "path": item.path, "bytes": item.bytes }))
            .collect();
        return ui::print_json(&json!({
            "version": ui::JSON_VERSION,
            "dry_run": args.dry_run,
            "removed": rows,
            "bytes": freed,
        }));
    }
    if items.is_empty() || (freed == 0 && !named) {
        println!("Nothing cached");
    } else if args.dry_run {
        for item in &items {
            println!("{}  {}", item.path.display(), mib(item.bytes));
        }
        println!(
            "Would remove {} entr{} ({})",
            items.len(),
            if items.len() == 1 { "y" } else { "ies" },
            mib(freed)
        );
    } else if named || args.older_than.is_some() {
        ui::print_success(&format!(
            "Removed {} cache entr{} ({})",
            items.len(),
            if items.len() == 1 { "y" } else { "ies" },
            mib(freed)
        ));
    } else {
        ui::print_success(&format!(
            "Removed the archive, index, git and shorthand caches ({})",
            mib(freed)
        ));
    }
    Ok(())
}

/// best-effort recursive size, an unreadable entry just doesn't count.
pub(crate) fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |meta| meta.len()),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_directories_recursively() {
        let base = std::env::temp_dir().join("embr-test-cache-size");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("v1")).unwrap();
        fs::write(base.join("v1/a.bin"), [0u8; 100]).unwrap();
        fs::write(base.join("v1/a.meta"), [0u8; 20]).unwrap();

        assert_eq!(dir_size(&base), 120);
        assert_eq!(dir_size(&base.join("missing")), 0);

        let _ = fs::remove_dir_all(&base);
    }
}
