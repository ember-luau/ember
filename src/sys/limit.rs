/*! Caps on how much an archive may unpack to. a few kilobytes of gzip, xz
or zip can hold gigabytes of zeroes, and embr unpacks archives that anyone
may have published, into memory or onto the disk of whoever installs them.
past the cap the read or the write fails with [`TOO_LARGE`], and the
install fails with it. */

use std::io::{Error, Read, Result, Write};

/// what a package may unpack to, 1 GiB. the registry takes far smaller archives.
pub const PACKAGE_BYTES: u64 = 1 << 30;

/// what a tool's release asset may unpack to, 4 GiB. a toolchain can be large.
pub const TOOL_BYTES: u64 = 4 << 30;

pub const TOO_LARGE: &str = "the archive unpacks to more than embr allows";

fn too_large(cap: u64) -> Error {
    Error::other(format!("{TOO_LARGE}, {} MiB", cap >> 20))
}

/** the error when the sizes that the entries of `archive` declare add up
to more than `cap`. this reads the sizes in the zip's own directory, so it
stops the honest large archive and the common zip bomb. it is not a count
of the bytes that come out, as [`Capped`] is for gzip. */
pub fn check_zip<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    cap: u64,
) -> Result<()> {
    let mut declared = 0u128;
    for index in 0..archive.len() {
        if let Ok(entry) = archive.by_index_raw(index) {
            declared += u128::from(entry.size());
        }
    }
    check_declared(declared, cap)
}

/** the error when `tar` holds a GNU sparse entry, or when the sizes that
its entries declare add up to more than `cap`. a sparse entry stores a few
bytes and declares the size of the file that the unpack writes, so 230
bytes of gzip unpacked to 7 GiB under the cap of [`Capped`], which counts
the stored bytes. no package and no tool needs one. */
pub fn check_tar<R: Read>(tar: R, cap: u64) -> Result<()> {
    let mut declared = 0u128;
    for entry in tar::Archive::new(tar).entries()? {
        let entry = entry?;
        let header = entry.header();
        if header.entry_type().is_gnu_sparse() {
            return Err(Error::other(format!(
                "the archive holds the sparse file {}, which embr does not unpack",
                String::from_utf8_lossy(&entry.path_bytes()).escape_debug()
            )));
        }
        declared += u128::from(header.size()?);
        check_declared(declared, cap)?;
    }
    Ok(())
}

/** takes from `path` every mode bit outside 0755: setuid, setgid, sticky
and the write bits of group and others. an archive sets the mode of each
file it holds, and a file that everyone may write is a file that another
user of the computer can change before it runs. */
pub fn mask_mode(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let meta = std::fs::symlink_metadata(path)?;
        let mode = meta.permissions().mode();
        // a link has no mode of its own, and a change would go to its target
        if !meta.file_type().is_symlink() && mode & 0o7777 & !0o755 != 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o755))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// [`mask_mode`] for `dir` and everything under it. links are left as they are.
pub fn mask_modes(dir: &std::path::Path) -> Result<()> {
    mask_mode(dir)?;
    if std::fs::symlink_metadata(dir)?.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            mask_modes(&entry?.path())?;
        }
    }
    Ok(())
}

fn check_declared(declared: u128, cap: u64) -> Result<()> {
    if declared > u128::from(cap) {
        return Err(too_large(cap));
    }
    Ok(())
}

/// a reader that fails once more than `cap` bytes have come out of it.
pub struct Capped<R> {
    inner: R,
    left: u64,
    cap: u64,
}

impl<R: Read> Capped<R> {
    pub fn new(inner: R, cap: u64) -> Self {
        Capped {
            inner,
            left: cap,
            cap,
        }
    }
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let read = self.inner.read(buffer)?;
        match self.left.checked_sub(read as u64) {
            Some(left) => {
                self.left = left;
                Ok(read)
            }
            None => Err(too_large(self.cap)),
        }
    }
}

/// bytes in memory that refuse to grow past `cap`.
pub struct CappedBytes {
    pub bytes: Vec<u8>,
    cap: u64,
}

impl CappedBytes {
    pub fn new(cap: u64) -> Self {
        CappedBytes {
            bytes: Vec::new(),
            cap,
        }
    }
}

impl Write for CappedBytes {
    fn write(&mut self, buffer: &[u8]) -> Result<usize> {
        if self.bytes.len() as u64 + buffer.len() as u64 > self.cap {
            return Err(too_large(self.cap));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capped_reader_stops_past_its_cap() {
        let data = [1u8; 64];
        let mut out = Vec::new();
        Capped::new(&data[..], 64).read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), 64);
        let error = Capped::new(&data[..], 63)
            .read_to_end(&mut Vec::new())
            .unwrap_err();
        assert!(error.to_string().contains(TOO_LARGE), "{error}");
    }

    #[test]
    fn capped_bytes_stop_at_their_cap() {
        let mut bytes = CappedBytes::new(4);
        bytes.write_all(b"abcd").unwrap();
        assert!(bytes.write_all(b"e").is_err());
        assert_eq!(bytes.bytes, b"abcd");
    }

    #[test]
    fn declared_sizes_over_the_cap_are_refused() {
        assert!(check_declared(10, 10).is_ok());
        assert!(check_declared(11, 10).is_err());

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("zeroes", options).unwrap();
        writer.write_all(&vec![0u8; 1 << 20]).unwrap();
        let packed = writer.finish().unwrap().into_inner();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(packed)).unwrap();
        assert!(check_zip(&mut archive, 1 << 20).is_ok());
        assert!(check_zip(&mut archive, (1 << 20) - 1).is_err());
    }

    /// a tar of the two regular files `small` and one more header, `extra`.
    fn tar_with(extra: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "a.luau", &b"abcd"[..])
            .unwrap();
        extra(&mut builder);
        builder.into_inner().unwrap()
    }

    #[test]
    fn a_sparse_tar_entry_is_refused_and_declared_sizes_count() {
        let plain = tar_with(|_| {});
        assert!(check_tar(plain.as_slice(), 4).is_ok());
        let error = check_tar(plain.as_slice(), 3).unwrap_err();
        assert!(error.to_string().contains(TOO_LARGE), "{error}");

        // 512 stored bytes that declare a file of 7 GiB
        let sparse = tar_with(|builder| {
            let mut header = tar::Header::new_gnu();
            header.set_path("big.luau").unwrap();
            header.set_mode(0o644);
            header.set_size(512);
            header.set_entry_type(tar::EntryType::GNUSparse);
            let real: u64 = 7 << 30;
            let gnu = header.as_gnu_mut().unwrap();
            gnu.sparse[0]
                .offset
                .copy_from_slice(format!("{:011o}\0", real - 512).as_bytes());
            gnu.sparse[0]
                .numbytes
                .copy_from_slice(format!("{:011o}\0", 512).as_bytes());
            gnu.realsize
                .copy_from_slice(format!("{real:011o}\0").as_bytes());
            header.set_cksum();
            builder.append(&header, &[b'A'; 512][..]).unwrap();
        });
        let error = check_tar(sparse.as_slice(), PACKAGE_BYTES).unwrap_err();
        assert!(error.to_string().contains("sparse"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn modes_are_cut_to_0755() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join("embr-test-mask-modes");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        for (file, before, after) in [
            ("sub/open", 0o777, 0o755),
            ("sub/suid", 0o4755, 0o755),
            ("plain", 0o644, 0o644),
            ("shared", 0o666, 0o644),
        ] {
            let path = dir.join(file);
            std::fs::write(&path, "x").unwrap();
            // A build sandbox, such as the one of Nix, refuses a setuid
            // bit. The case has nothing to check there.
            match std::fs::set_permissions(&path, std::fs::Permissions::from_mode(before)) {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => continue,

                set => set.unwrap(),
            }
            mask_modes(&dir).unwrap();
            assert_eq!(mode(&path), after, "{file}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// a gzip of zeroes is about a thousandth of what it unpacks to.
    #[test]
    fn a_gzip_bomb_fails_at_the_cap_and_not_at_the_end() {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&vec![0u8; 8 << 20]).unwrap();
        let packed = encoder.finish().unwrap();
        assert!(packed.len() < 64 << 10);

        let decoder = flate2::read::GzDecoder::new(packed.as_slice());
        let mut out = Vec::new();
        let error = Capped::new(decoder, 1 << 20)
            .read_to_end(&mut out)
            .unwrap_err();
        assert!(error.to_string().contains(TOO_LARGE));
        assert!(out.len() <= 1 << 20);
    }
}
