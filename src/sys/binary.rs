/*! Writing executables that may be running. embr itself, the embx
launcher and every tool shim live in ~/.ember/bin, and any of them can be
in use by another process while an install or update rewrites it. */

use super::paths;
use std::fs;
use std::io;
use std::path::Path;

/** Puts `source` at `target`, replacing whatever is there, even while that
file is running.

A plain copy opens the destination for writing, and on unix writing a file
some process is currently executing fails with ETXTBSY -- "Text file busy".
On windows it fails with a sharing violation (os error 32). ~/.ember/bin is
exactly that whenever a tool is alive: a second terminal, a long `embr run`,
an embx, an editor's `rojo sourcemap --watch`. Installing should not
require hunting those down first.

So the new bytes land beside the target and a rename swaps the directory
entry. Anything already running keeps the old inode and finishes on it
undisturbed; the next run picks up the new one. fs::copy carries the
executable bit over on unix, and rename preserves it. */
pub fn replace(source: &Path, target: &Path) -> io::Result<()> {
    let staged = paths::with_suffix(target, ".new");
    // a leftover from an interrupted install would fail the copy below
    let _ = fs::remove_file(&staged);
    fs::copy(source, &staged)?;

    let Err(error) = fs::rename(&staged, target) else {
        return Ok(());
    };

    /* windows refuses to replace a file that is open for execution, but it
    does allow renaming that file away -- the running image follows the
    inode, not the name. unix never reaches here, the rename above works. */
    let retired = retired_path(target);
    let _ = fs::remove_file(&retired);
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
    // still locked until it exits; swept by the next write of this target
    let _ = fs::remove_file(&retired);
    Ok(())
}

/// where [`replace`] parks a running target it had to move out of the way.
pub fn retired_path(target: &Path) -> std::path::PathBuf {
    paths::with_suffix(target, ".old")
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

    /** a running exe on windows is open without FILE_SHARE_WRITE but with
    FILE_SHARE_DELETE, which is what lets it be renamed. holding a handle
    with that share mode reproduces the lock a live `rojo.exe` puts on its
    shim, the os error 32 an install used to fail with. */
    #[cfg(windows)]
    #[test]
    fn replaces_a_target_locked_like_a_running_exe() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_DELETE: u32 = 0x4;

        let dir = scratch("embr-test-replace-locked");
        let (source, target) = (dir.join("embr.exe"), dir.join("rojo.exe"));
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"old").unwrap();

        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .open(&target)
            .unwrap();

        assert!(
            fs::copy(&source, &target).is_err(),
            "a plain copy should hit the lock"
        );
        replace(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");

        drop(held);
        let _ = fs::remove_dir_all(&dir);
    }
}
