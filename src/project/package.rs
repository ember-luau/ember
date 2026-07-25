//! An extracted package on disk: where its entry point is, which environment
//! it targets, and the link file that points at it. This is the part of
//! installing that reads packages rather than orchestrating downloads, and it
//! has to understand foreign manifests (pesde.toml, wally.toml, Rojo project
//! files) because that is all a published package carries.

use crate::error::Error;
use crate::project::manifest::Environment;
use std::fs;
use std::path::Path;

/// Body of a generated link file, e.g. `return require("./.lpm/scope_pkg/lib")`.
pub fn link_contents(folder: &str, entry: &str) -> String {
    format!("return require(\"./.lpm/{folder}/{entry}\")\n")
}

/// Finds a package's entry point relative to its root, without a file
/// extension (Luau string requires reject them). Checked in order:
/// lpm.toml `[target].main`, pesde.toml `[target].lib`, a Rojo
/// default.project.json tree `$path`, then conventional init file locations.
pub fn entry_point(dir: &Path) -> Option<String> {
    if let Some(main) = toml_string(dir, "lpm.toml", &["target", "main"]) {
        return Some(normalize_entry(&main));
    }
    if let Some(lib) = toml_string(dir, "pesde.toml", &["target", "lib"]) {
        return Some(normalize_entry(&lib));
    }
    if let Some(path) = fs::read_to_string(dir.join("default.project.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| Some(json.get("tree")?.get("$path")?.as_str()?.to_string()))
    {
        return Some(normalize_entry(&path));
    }

    for candidate in [
        "init.luau",
        "init.lua",
        "src/init.luau",
        "src/init.lua",
        "lib/init.luau",
        "lib/init.lua",
    ] {
        if dir.join(candidate).exists() {
            return Some(normalize_entry(candidate));
        }
    }
    None
}

/// Reads an extracted package's own manifest to find its environment:
/// lpm.toml `[target].environment`, then pesde.toml `[target].environment`
/// (translated), then wally.toml `[package].realm` (translated).
pub fn environment(dir: &Path) -> Option<Environment> {
    if let Some(name) = toml_string(dir, "lpm.toml", &["target", "environment"]) {
        return Environment::from_lpm(&name).ok();
    }
    if let Some(name) = toml_string(dir, "pesde.toml", &["target", "environment"]) {
        return Environment::from_pesde(&name).ok();
    }
    if let Some(realm) = toml_string(dir, "wally.toml", &["package", "realm"]) {
        return Environment::from_wally_realm(&realm).ok();
    }
    None
}

/// Archives sometimes wrap everything in a single top-level folder
/// (e.g. GitHub release tarballs); unwrap it so package files sit at the root.
pub fn flatten_single_dir(dir: &Path) -> Result<(), Error> {
    let entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    let [only] = entries.as_slice() else {
        return Ok(());
    };
    if !only.file_type()?.is_dir() {
        return Ok(());
    }

    let inner = only.path();
    for entry in fs::read_dir(&inner)? {
        let entry = entry?;
        fs::rename(entry.path(), dir.join(entry.file_name()))?;
    }
    fs::remove_dir(inner)?;
    Ok(())
}

/// A string at `keys` in one of the package's manifests, if that file exists,
/// parses, and has it. Missing is the normal case, so nothing here is an error.
fn toml_string(dir: &Path, file: &str, keys: &[&str]) -> Option<String> {
    let mut value: toml::Value = fs::read_to_string(dir.join(file)).ok()?.parse().ok()?;
    for key in keys {
        value = value.get(key)?.clone();
    }
    value.as_str().map(str::to_string)
}

/// Normalizes an entry path for a string require: forward slashes, no leading
/// "./", no .luau/.lua extension (a bare folder resolves its init file).
fn normalize_entry(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path.trim_start_matches("./").trim_matches('/');
    path.strip_suffix(".luau")
        .or_else(|| path.strip_suffix(".lua"))
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_package(dir: &Path, file: &str, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(file), contents).unwrap();
    }

    #[test]
    fn detects_environment_from_manifests() {
        let base = std::env::temp_dir().join("lpm-test-detect-env");
        let _ = fs::remove_dir_all(&base);

        let lpm = base.join("lpm");
        write_package(&lpm, "lpm.toml", "[target]\nenvironment = \"lune\"");
        assert_eq!(environment(&lpm), Some(Environment::Lune));

        let pesde = base.join("pesde");
        write_package(&pesde, "pesde.toml", "[target]\nenvironment = \"roblox\"");
        assert_eq!(environment(&pesde), Some(Environment::Shared));

        let wally = base.join("wally");
        write_package(&wally, "wally.toml", "[package]\nrealm = \"server\"");
        assert_eq!(environment(&wally), Some(Environment::Server));

        let none = base.join("none");
        fs::create_dir_all(&none).unwrap();
        assert_eq!(environment(&none), None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn lpm_manifest_takes_priority_over_wally() {
        let base = std::env::temp_dir().join("lpm-test-detect-priority");
        let _ = fs::remove_dir_all(&base);

        write_package(&base, "wally.toml", "[package]\nrealm = \"server\"");
        write_package(&base, "lpm.toml", "[target]\nenvironment = \"luau\"");
        assert_eq!(environment(&base), Some(Environment::Luau));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn flattens_single_wrapper_directory() {
        let base = std::env::temp_dir().join("lpm-test-flatten");
        let _ = fs::remove_dir_all(&base);
        let wrapper = base.join("pkg-1.0.0");
        fs::create_dir_all(wrapper.join("src")).unwrap();
        fs::write(wrapper.join("init.luau"), "return {}").unwrap();

        flatten_single_dir(&base).unwrap();
        assert!(base.join("init.luau").exists());
        assert!(base.join("src").exists());
        assert!(!base.join("pkg-1.0.0").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn detects_entry_points_in_priority_order() {
        let base = std::env::temp_dir().join("lpm-test-detect-entry");
        let _ = fs::remove_dir_all(&base);

        // lpm.toml main wins over everything.
        let a = base.join("a");
        write_package(&a, "lpm.toml", "[target]\nmain = \"src/main.luau\"");
        write_package(&a, "init.luau", "");
        assert_eq!(entry_point(&a).as_deref(), Some("src/main"));

        // pesde.toml lib next.
        let b = base.join("b");
        write_package(&b, "pesde.toml", "[target]\nlib = \"lib.luau\"");
        assert_eq!(entry_point(&b).as_deref(), Some("lib"));

        // Rojo project tree path (a folder; its init file resolves at require time).
        let c = base.join("c");
        write_package(
            &c,
            "default.project.json",
            r#"{"name": "pkg", "tree": {"$path": "src"}}"#,
        );
        assert_eq!(entry_point(&c).as_deref(), Some("src"));

        // Conventional fallbacks.
        let d = base.join("d");
        write_package(&d.join("src"), "init.lua", "");
        assert_eq!(entry_point(&d).as_deref(), Some("src/init"));

        let e = base.join("e");
        fs::create_dir_all(&e).unwrap();
        assert_eq!(entry_point(&e), None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn link_files_require_the_stored_package() {
        assert_eq!(
            link_contents("evaera_promise", "lib"),
            "return require(\"./.lpm/evaera_promise/lib\")\n"
        );
        assert_eq!(normalize_entry("./src\\init.luau"), "src/init".to_string());
        assert_eq!(normalize_entry("lib.lua"), "lib".to_string());
        assert_eq!(normalize_entry("src"), "src".to_string());
    }
}
