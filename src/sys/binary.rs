/*! Writing executables that may be running. embr itself, the embx
launcher and every tool shim live in ~/.ember/bin, and any of them can be
in use by another process while an install or update rewrites it. */

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

/** the folder beside a target where [`replace`] stages new copies and parks
running ones. its own folder so no file it names can be mistaken for, or
clobber, a tool alias in ~/.ember/bin. not on PATH, PATH lookups never
descend into subfolders. */
const SWAP_DIR: &str = ".swap";

/** Puts `source` at `target`, replacing whatever is there, even while that
file is running.

A plain copy opens the destination for writing, and on unix writing a file
some process is currently executing fails with ETXTBSY -- "Text file busy".
On windows it fails with a sharing violation (os error 32). ~/.ember/bin is
exactly that whenever a tool is alive: a second terminal, a long `embr run`,
an embx, an editor's `rojo sourcemap --watch`. Installing should not
require hunting those down first.

So the new bytes land in the swap folder and a rename swaps the directory
entry. Anything already running keeps the old inode and finishes on it
undisturbed; the next run picks up the new one. fs::copy carries the
executable bit over on unix, and rename preserves it.

Every name in the swap folder carries this process's id, and a parked file
also a timestamp, so two installs at once never touch each other's files,
and a parked copy that is still running never blocks the next swap. */
pub fn replace(source: &Path, target: &Path) -> io::Result<()> {
    let swap = swap_dir(target);
    fs::create_dir_all(&swap)?;
    sweep_retired(&swap, target);

    let staged = swap_path(&swap, target, &format!("{}.new", process::id()));
    // a leftover from an interrupted run of this pid would fail the copy
    let _ = fs::remove_file(&staged);
    if let Err(error) = fs::copy(source, &staged) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }

    let Err(error) = fs::rename(&staged, target) else {
        return Ok(());
    };

    /* windows refuses to replace a file that is open for execution, but it
    does allow renaming that file away -- the running image follows the
    inode, not the name. unix never reaches here, the rename above works. */
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let retired = swap_path(&swap, target, &format!("{}.{nanos}.old", process::id()));
    if fs::rename(target, &retired).is_err() {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    if let Err(error) = fs::rename(&staged, target) {
        // put back what was working rather than leave nothing installed
        let _ = fs::rename(&retired, target);
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    // still locked until it exits; swept by the next replace in this folder
    let _ = fs::remove_file(&retired);
    Ok(())
}

fn swap_dir(target: &Path) -> PathBuf {
    target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(SWAP_DIR)
}

/// `<swap>/<target file name>.<suffix>`
fn swap_path(swap: &Path, target: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(target.file_name().unwrap_or_default());
    name.push(".");
    name.push(suffix);
    swap.join(name)
}

/** best effort removal of parked copies whose process has exited. a
running one stays locked and is retried on the next replace. `.new` files
are left alone, they may be another install's copy in progress. */
fn sweep_retired(swap: &Path, target: &Path) {
    if let Ok(entries) = fs::read_dir(swap) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".old") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    /* embr before the swap folder parked a running target beside itself.
    windows only, the only place that path was taken, and there every shim
    ends in .exe, so `<name>.exe.old` can't be another tool's shim */
    if cfg!(windows) {
        let mut legacy = target.as_os_str().to_os_string();
        legacy.push(".old");
        let _ = fs::remove_file(PathBuf::from(legacy));
    }
}

/** true when both files exist and hold the same bytes. reading works on a
running executable on every platform, so this never trips the lock that
writing does. */
pub fn same_contents(a: &Path, b: &Path) -> bool {
    let (Ok(meta_a), Ok(meta_b)) = (fs::metadata(a), fs::metadata(b)) else {
        return false;
    };
    if meta_a.len() != meta_b.len() {
        return false;
    }
    matches!((fs::read(a), fs::read(b)), (Ok(left), Ok(right)) if left == right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn same_contents_compares_bytes() {
        let dir = scratch("embr-test-same-contents");
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        fs::write(&a, b"shim").unwrap();
        fs::write(&b, b"shim").unwrap();
        fs::write(&c, b"shin").unwrap();
        assert!(same_contents(&a, &b));
        assert!(!same_contents(&a, &c));
        assert!(!same_contents(&a, &dir.join("missing")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_names_nothing_beside_the_target() {
        let dir = scratch("embr-test-replace-beside");
        let shim = |alias: &str| {
            dir.join("bin")
                .join(format!("{alias}{}", std::env::consts::EXE_SUFFIX))
        };
        let (source, target) = (dir.join("source"), shim("foo"));
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"old").unwrap();
        // tools aliased like the old suffixes must survive a swap of `foo`
        let neighbours = [shim("foo.old"), shim("foo.new")];
        for neighbour in &neighbours {
            fs::write(neighbour, b"other tool").unwrap();
        }

        replace(&source, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new");
        for neighbour in &neighbours {
            assert_eq!(fs::read(neighbour).unwrap(), b"other tool");
        }
        let staged: Vec<_> = fs::read_dir(dir.join("bin").join(SWAP_DIR))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert!(staged.is_empty(), "left in the swap folder: {staged:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_sweeps_parked_copies_but_not_other_staging() {
        let dir = scratch("embr-test-replace-sweep");
        let (source, target) = (dir.join("source"), dir.join("bin").join("foo"));
        let swap = dir.join("bin").join(SWAP_DIR);
        fs::create_dir_all(&swap).unwrap();
        fs::write(&source, b"new").unwrap();
        let parked = swap.join("bar.1.2.old");
        let in_progress = swap.join("bar.1.new");
        fs::write(&parked, b"exited").unwrap();
        fs::write(&in_progress, b"another install").unwrap();

        replace(&source, &target).unwrap();

        assert!(!parked.exists());
        assert!(in_progress.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /** a copy of cmd.exe left waiting on stdin, killed on drop. only a real
    running image reproduces the lock: a handle opened with any share mode
    still lets rename replace the file, so it would never reach the
    fallback in `replace`. */
    #[cfg(windows)]
    struct Running(std::process::Child);

    #[cfg(windows)]
    impl Running {
        fn spawn(exe: &Path) -> Self {
            use std::process::{Command, Stdio};
            let child = Command::new(exe)
                .args(["/q", "/k"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            Self(child)
        }
    }

    #[cfg(windows)]
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(windows)]
    fn parked_count(swap: &Path) -> usize {
        fs::read_dir(swap)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".old"))
            .count()
    }

    /** the os error 32 `embr install` used to fail with, and the next swap
    while the parked copy still runs. bytes appended to an exe don't stop
    it running, so each generation is cmd.exe plus a marker. */
    #[cfg(windows)]
    #[test]
    fn replaces_a_running_exe_twice_while_both_keep_running() {
        let cmd = std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
        let cmd_bytes = fs::read(&cmd).unwrap();
        let dir = scratch("embr-test-replace-running");
        let swap = dir.join(SWAP_DIR);
        let target = dir.join("rojo.exe");
        let generation = |marker: &[u8]| {
            let path = dir.join(format!("gen-{}.exe", String::from_utf8_lossy(marker)));
            fs::write(&path, [cmd_bytes.as_slice(), marker].concat()).unwrap();
            path
        };
        let (first, second, third) = (generation(b"1"), generation(b"2"), generation(b"3"));
        fs::copy(&first, &target).unwrap();

        let first_running = Running::spawn(&target);
        assert!(
            fs::copy(&second, &target).is_err(),
            "a plain copy over a running exe should fail"
        );
        replace(&second, &target).unwrap();
        assert!(same_contents(&second, &target));
        assert_eq!(parked_count(&swap), 1, "the running copy is parked");

        let second_running = Running::spawn(&target);
        replace(&third, &target).unwrap();
        assert!(same_contents(&third, &target));
        assert_eq!(parked_count(&swap), 2, "both running copies are parked");

        drop((first_running, second_running));
        replace(&third, &target).unwrap();
        assert_eq!(parked_count(&swap), 0, "exited copies are swept");
        let _ = fs::remove_dir_all(&dir);
    }
}
