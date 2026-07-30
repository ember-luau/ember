use crate::error::Error;
use crate::sys::paths;
use crate::ui;
use clap::Subcommand;
use std::fs;
use std::path::Path;

#[derive(Subcommand, Debug)]
pub enum CacheCommand {
    /// Delete the downloaded-archive and index caches under ~/.lpm
    Clean,
}

pub fn run(command: CacheCommand) -> Result<(), Error> {
    match command {
        CacheCommand::Clean => clean(),
    }
}

/** both caches go, archives and index clones. removing the index root
also sweeps orphaned index dirs left behind by older lpm versions that
keyed them differently. everything here re-downloads on demand. */
fn clean() -> Result<(), Error> {
    let mut freed = 0;
    for root in [paths::archive_cache_root()?, paths::index_cache_dir()?] {
        if root.exists() {
            freed += dir_size(&root);
            fs::remove_dir_all(&root)?;
        }
    }

    if freed == 0 {
        println!("Nothing cached");
    } else {
        ui::print_success(&format!(
            "Removed the archive and index caches ({:.1} MiB)",
            freed as f64 / (1024.0 * 1024.0)
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
        let base = std::env::temp_dir().join("lpm-test-cache-size");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("v1")).unwrap();
        fs::write(base.join("v1/a.bin"), [0u8; 100]).unwrap();
        fs::write(base.join("v1/a.meta"), [0u8; 20]).unwrap();

        assert_eq!(dir_size(&base), 120);
        assert_eq!(dir_size(&base.join("missing")), 0);

        let _ = fs::remove_dir_all(&base);
    }
}
