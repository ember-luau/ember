use crate::error::Error;
use crate::index;
use crate::lockfile::{LockedPackage, Lockfile};
use crate::manifest::{Environment, Manifest};
use crate::resolver;
use crate::ui;
use clap::Args;
use std::fs;
use std::path::Path;

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Install exactly what lpm.lock records, without re-resolving
    #[arg(long)]
    pub locked: bool,
}

struct Job {
    name: String,
    version: String,
    environment: Option<Environment>,
    source: index::DownloadSource,
    index_url: String,
    link: String,
}

pub fn run(args: InstallArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;

    let jobs: Vec<Job> = if args.locked {
        Lockfile::load()?
            .packages
            .into_iter()
            .map(|package| Job {
                name: package.name,
                version: package.version,
                environment: Some(package.environment),
                source: package.source,
                index_url: package.index,
                link: package.link,
            })
            .collect()
    } else {
        println!("Resolving dependencies");
        resolver::resolve(&manifest, true)?
            .into_iter()
            .map(|package| Job {
                name: package.name,
                version: package.version.to_string(),
                environment: package.environment,
                source: package.source,
                index_url: package.index_url,
                link: package.link,
            })
            .collect()
    };

    // Installs are reproduced from scratch each run: every environment's
    // configured output folder is rebuilt even when there is nothing to
    // install, so removing the last dependency leaves no stale packages.
    for environment in Environment::ALL {
        let out = manifest.packages_out(environment);
        if out.exists() {
            fs::remove_dir_all(&out)?;
        }
    }

    // Extraction happens before the environment (and therefore the output
    // folder) is always known, so stage in a project-local temp dir; a rename
    // then moves it into place (same filesystem as the outputs).
    let staging = Path::new(".lpm-staging").to_path_buf();
    let mut locked = Vec::new();
    for job in jobs {
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        index::download(&job.source, &staging)?;
        flatten_single_dir(&staging)?;

        // Indices usually know the environment; otherwise ask the files
        // (lpm.toml -> pesde.toml -> wally.toml).
        let environment = match job.environment {
            Some(environment) => environment,
            None => detect_environment(&staging)
                .ok_or_else(|| Error::UnknownPackageEnvironment(job.name.clone()))?,
        };

        // Real package contents live under <out>/.lpm/<scope>_<name>/; a
        // <out>/<link>.luau file re-exports the package's entry point so
        // consumers can `require(".../<link>")`.
        let folder = job.name.replace('/', "_");
        let out = manifest.packages_out(environment);
        let storage = out.join(".lpm").join(&folder);
        fs::create_dir_all(storage.parent().expect("storage dir has a parent"))?;
        if storage.exists() {
            fs::remove_dir_all(&storage)?;
        }
        fs::rename(&staging, &storage)?;

        match detect_entry(&storage) {
            Some(entry) => {
                let link_path = out.join(format!("{}.luau", job.link));
                fs::write(&link_path, link_contents(&folder, &entry))?;
            }
            None => eprintln!(
                "warning: could not find an entry point for {}; no link file generated",
                job.name
            ),
        }

        ui::print_success(&format!(
            "{}@{} → {}/{}",
            job.name, job.version, environment, job.link
        ));
        locked.push(LockedPackage {
            name: job.name,
            version: job.version,
            environment,
            link: job.link,
            index: job.index_url,
            source: job.source,
        });
    }

    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }

    let count = locked.len();
    if !args.locked {
        Lockfile::new(locked).save()?;
    }

    println!(
        "Installed {count} package{}",
        if count == 1 { "" } else { "s" }
    );
    Ok(())
}

/// Body of a generated link file, e.g. `return require("./.lpm/scope_pkg/lib")`.
fn link_contents(folder: &str, entry: &str) -> String {
    format!("return require(\"./.lpm/{folder}/{entry}\")\n")
}

/// Finds a package's entry point relative to its root, without a file
/// extension (Luau string requires reject them). Checked in order:
/// lpm.toml `[target].main`, pesde.toml `[target].lib`, a Rojo
/// default.project.json tree `$path`, then conventional init file locations.
fn detect_entry(dir: &Path) -> Option<String> {
    let read_toml = |file: &str| -> Option<toml::Value> {
        fs::read_to_string(dir.join(file)).ok()?.parse().ok()
    };

    if let Some(main) = read_toml("lpm.toml")
        .as_ref()
        .and_then(|value| Some(value.get("target")?.get("main")?.as_str()?.to_string()))
    {
        return Some(strip_entry_extension(&main));
    }
    if let Some(lib) = read_toml("pesde.toml")
        .as_ref()
        .and_then(|value| Some(value.get("target")?.get("lib")?.as_str()?.to_string()))
    {
        return Some(strip_entry_extension(&lib));
    }
    if let Some(path) = fs::read_to_string(dir.join("default.project.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| Some(json.get("tree")?.get("$path")?.as_str()?.to_string()))
    {
        return Some(strip_entry_extension(&path));
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
            return Some(strip_entry_extension(candidate));
        }
    }
    None
}

/// Normalizes an entry path for a string require: forward slashes, no leading
/// "./", no .luau/.lua extension (a bare folder resolves its init file).
fn strip_entry_extension(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path.trim_start_matches("./").trim_matches('/');
    path.strip_suffix(".luau")
        .or_else(|| path.strip_suffix(".lua"))
        .unwrap_or(path)
        .to_string()
}

/// Archives sometimes wrap everything in a single top-level folder
/// (e.g. GitHub release tarballs); unwrap it so package files sit at the root.
fn flatten_single_dir(dir: &Path) -> Result<(), Error> {
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

/// Reads an extracted package's own manifest to find its environment:
/// lpm.toml `[target].environment`, then pesde.toml `[target].environment`
/// (translated), then wally.toml `[package].realm` (translated).
fn detect_environment(dir: &Path) -> Option<Environment> {
    let read_toml = |file: &str| -> Option<toml::Value> {
        fs::read_to_string(dir.join(file)).ok()?.parse().ok()
    };
    let target_environment = |value: &toml::Value| -> Option<String> {
        Some(
            value
                .get("target")?
                .get("environment")?
                .as_str()?
                .to_string(),
        )
    };

    if let Some(environment) = read_toml("lpm.toml").as_ref().and_then(target_environment) {
        return Environment::from_lpm(&environment).ok();
    }
    if let Some(environment) = read_toml("pesde.toml")
        .as_ref()
        .and_then(target_environment)
    {
        return Environment::from_pesde(&environment).ok();
    }
    if let Some(realm) = read_toml("wally.toml")
        .as_ref()
        .and_then(|value| Some(value.get("package")?.get("realm")?.as_str()?.to_string()))
    {
        return Environment::from_wally_realm(&realm).ok();
    }
    None
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
        assert_eq!(detect_environment(&lpm), Some(Environment::Lune));

        let pesde = base.join("pesde");
        write_package(&pesde, "pesde.toml", "[target]\nenvironment = \"roblox\"");
        assert_eq!(detect_environment(&pesde), Some(Environment::Shared));

        let wally = base.join("wally");
        write_package(&wally, "wally.toml", "[package]\nrealm = \"server\"");
        assert_eq!(detect_environment(&wally), Some(Environment::Server));

        let none = base.join("none");
        fs::create_dir_all(&none).unwrap();
        assert_eq!(detect_environment(&none), None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn lpm_manifest_takes_priority_over_wally() {
        let base = std::env::temp_dir().join("lpm-test-detect-priority");
        let _ = fs::remove_dir_all(&base);

        write_package(&base, "wally.toml", "[package]\nrealm = \"server\"");
        write_package(&base, "lpm.toml", "[target]\nenvironment = \"luau\"");
        assert_eq!(detect_environment(&base), Some(Environment::Luau));

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
        assert_eq!(detect_entry(&a).as_deref(), Some("src/main"));

        // pesde.toml lib next.
        let b = base.join("b");
        write_package(&b, "pesde.toml", "[target]\nlib = \"lib.luau\"");
        assert_eq!(detect_entry(&b).as_deref(), Some("lib"));

        // Rojo project tree path (a folder; its init file resolves at require time).
        let c = base.join("c");
        write_package(
            &c,
            "default.project.json",
            r#"{"name": "pkg", "tree": {"$path": "src"}}"#,
        );
        assert_eq!(detect_entry(&c).as_deref(), Some("src"));

        // Conventional fallbacks.
        let d = base.join("d");
        write_package(&d.join("src"), "init.lua", "");
        assert_eq!(detect_entry(&d).as_deref(), Some("src/init"));

        let e = base.join("e");
        fs::create_dir_all(&e).unwrap();
        assert_eq!(detect_entry(&e), None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn link_files_require_the_stored_package() {
        assert_eq!(
            link_contents("evaera_promise", "lib"),
            "return require(\"./.lpm/evaera_promise/lib\")\n"
        );
        assert_eq!(
            strip_entry_extension("./src\\init.luau"),
            "src/init".to_string()
        );
        assert_eq!(strip_entry_extension("lib.lua"), "lib".to_string());
        assert_eq!(strip_entry_extension("src"), "src".to_string());
    }
}
