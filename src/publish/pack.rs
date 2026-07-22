use crate::error::Error;
use crate::manifest::{Environment, MANIFEST_FILE, Manifest};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::path::{Path, PathBuf};

/// Never published: VCS state, staging leftovers, the lockfile, and build
/// output. Matched by name at any depth.
const SKIP_NAMES: [&str; 5] = [".git", ".lpm-staging", "lpm.lock", "target", "node_modules"];

/// Files to publish, relative to `root`, sorted so the archive is
/// deterministic. Configured packages-out folders are skipped alongside
/// `SKIP_NAMES`. `include`/`exclude` entries are plain relative file or
/// directory paths (a directory covers everything under it), not globs;
/// lpm.toml is always kept.
pub fn packed_files(root: &Path, manifest: &Manifest) -> Result<Vec<PathBuf>, Error> {
    let out_dirs: Vec<PathBuf> = Environment::ALL
        .into_iter()
        .map(|environment| manifest.packages_out(environment))
        .collect();

    let mut files = Vec::new();
    walk(root, root, &out_dirs, &mut files)?;

    let include = &manifest.package.include;
    let exclude = &manifest.package.exclude;
    files.retain(|file| {
        if file.as_path() == Path::new(MANIFEST_FILE) {
            return true;
        }
        if !include.is_empty() && !include.iter().any(|entry| file.starts_with(entry)) {
            return false;
        }
        !exclude.iter().any(|entry| file.starts_with(entry))
    });

    files.sort();
    Ok(files)
}

/// Packs the selected files into a gzipped tar. Entry paths are
/// forward-slash relative paths without a leading "./".
pub fn pack(root: &Path, manifest: &Manifest) -> Result<Vec<u8>, Error> {
    let files = packed_files(root, manifest)?;
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for file in &files {
        builder.append_path_with_name(root.join(file), tar_path(file))?;
    }
    Ok(builder.into_inner()?.finish()?)
}

fn walk(
    dir: &Path,
    root: &Path,
    out_dirs: &[PathBuf],
    files: &mut Vec<PathBuf>,
) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIP_NAMES.iter().any(|skip| name == *skip) {
            continue;
        }

        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("walked path is under root")
            .to_path_buf();
        if out_dirs.iter().any(|out| relative.starts_with(out)) {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk(&path, root, out_dirs, files)?;
        } else if file_type.is_file() {
            files.push(relative);
        }
    }
    Ok(())
}

fn tar_path(file: &Path) -> String {
    file.iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Read as _;

    fn parse_manifest(toml_text: &str) -> Manifest {
        toml::from_str(toml_text).unwrap()
    }

    fn write(root: &Path, file: &str, contents: &str) {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn skips_vcs_build_and_output_dirs() {
        let base = std::env::temp_dir().join("lpm-test-pack-skips");
        let _ = fs::remove_dir_all(&base);

        write(&base, "lpm.toml", "");
        write(&base, "src/init.luau", "return {}");
        write(&base, ".git/HEAD", "ref: refs/heads/master");
        write(&base, ".lpm-staging/rocket.tar.gz", "");
        write(&base, "lpm.lock", "version = 1");
        write(&base, "target/debug/lpm", "");
        write(&base, "node_modules/left-pad/index.js", "");
        write(&base, "packages/luau/Core.luau", "");
        write(&base, "Packages/Chief.luau", "");

        let manifest = parse_manifest(
            r#"
            [package]
            name = "acme/rocket"
            version = "1.0.0"

            [config]
            shared-packages-out = "Packages"
            "#,
        );

        let files = packed_files(&base, &manifest).unwrap();
        assert_eq!(
            files,
            vec![PathBuf::from("lpm.toml"), PathBuf::from("src/init.luau")]
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn include_and_exclude_are_plain_path_filters() {
        let base = std::env::temp_dir().join("lpm-test-pack-filters");
        let _ = fs::remove_dir_all(&base);

        write(&base, "lpm.toml", "");
        write(&base, "README.md", "");
        write(&base, "notes.txt", "");
        write(&base, "src/init.luau", "");
        write(&base, "src/tests/spec.luau", "");

        let manifest = parse_manifest(
            r#"
            [package]
            name = "acme/rocket"
            version = "1.0.0"
            include = ["src", "README.md"]
            exclude = ["src/tests"]
            "#,
        );

        let files = packed_files(&base, &manifest).unwrap();
        assert_eq!(
            files,
            vec![
                PathBuf::from("README.md"),
                PathBuf::from("lpm.toml"),
                PathBuf::from("src/init.luau"),
            ]
        );
        // Same tree, same list.
        assert_eq!(packed_files(&base, &manifest).unwrap(), files);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn tar_round_trips_with_forward_slash_paths() {
        let base = std::env::temp_dir().join("lpm-test-pack-roundtrip");
        let _ = fs::remove_dir_all(&base);

        write(&base, "lpm.toml", "[package]\nname = \"acme/rocket\"\n");
        write(&base, "src/init.luau", "return 1\n");
        write(&base, "docs/guide.md", "# rocket\n");

        let manifest = parse_manifest("[package]\nname = \"acme/rocket\"\nversion = \"1.0.0\"");

        let bytes = pack(&base, &manifest).unwrap();
        assert!(bytes.starts_with(&[0x1f, 0x8b]), "output must be gzipped");

        let mut unpacked = BTreeMap::new();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = String::from_utf8(entry.path_bytes().into_owned()).unwrap();
            assert!(!path.starts_with("./"), "unexpected ./ prefix in {path}");
            assert!(!path.contains('\\'), "backslash in tar path {path}");
            let mut contents = String::new();
            entry.read_to_string(&mut contents).unwrap();
            unpacked.insert(path, contents);
        }

        assert_eq!(unpacked.len(), 3);
        assert_eq!(unpacked["lpm.toml"], "[package]\nname = \"acme/rocket\"\n");
        assert_eq!(unpacked["src/init.luau"], "return 1\n");
        assert_eq!(unpacked["docs/guide.md"], "# rocket\n");

        let _ = fs::remove_dir_all(&base);
    }
}
