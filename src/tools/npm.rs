/*! tools that npm publishes, written `npm:package@version` under [tools].
some tools ship no binaries, only an npm package. Azul is one. npm installs
each pinned version into its own folder under ~/.ember/tools/npm, and the
shim runs the package's bin script with node. so these tools need Node.js
on PATH, to install and to run. */

use super::Staging;
use crate::error::Error;
use crate::sys::paths;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/** ~/.ember/tools/npm/{package}/{version}. a scoped package keeps its scope
with '+' for the '/', which npm never allows in a name. GitHub tools sit
beside the `npm` folder as `{owner}_{repo}`, and no GitHub owner is
called `npm` with a '_' after it, so the two never meet. */
pub fn storage_dir(package: &str, version: &str) -> Result<PathBuf, Error> {
    Ok(package_root(package)?.join(version))
}

/// ~/.ember/tools/npm/{package}, every stored version of one package.
pub fn package_root(package: &str) -> Result<PathBuf, Error> {
    Ok(paths::tools_dir()?
        .join("npm")
        .join(package.replace('/', "+")))
}

/// the installed package inside one version's storage.
fn package_dir(storage: &Path, package: &str) -> PathBuf {
    storage.join("node_modules").join(package)
}

/// whether npm installed this version completely.
pub fn is_stored(package: &str, version: &str) -> Result<bool, Error> {
    Ok(package_dir(&storage_dir(package, version)?, package)
        .join("package.json")
        .is_file())
}

/** installs one version with npm, unless it is stored. true when npm ran.
npm installs into a staging folder first, so a failed install leaves no
half-written version behind. the Node.js check runs before the folder
moves into place, so a too-old Node.js stores nothing. */
pub fn install(package: &str, version: &str) -> Result<bool, Error> {
    if is_stored(package, version)? {
        return Ok(false);
    }
    let storage = storage_dir(package, version)?;
    let staging = Staging(paths::with_suffix(
        &storage,
        &format!(".{}.tmp", std::process::id()),
    ));
    if staging.0.exists() {
        fs::remove_dir_all(&staging.0)?;
    }
    fs::create_dir_all(&staging.0)?;

    let spec = format!("{package}@{version}");
    let output = npm()
        .args(install_args(&staging.0, &spec))
        .stdin(Stdio::null())
        .output()
        .map_err(not_found_is_node_missing)?;
    if !output.status.success() {
        return Err(Error::NpmFailed {
            spec,
            output: npm_error(&output.stderr),
        });
    }

    let manifest = read_manifest(&package_dir(&staging.0, package))
        .ok_or_else(|| Error::NpmNoBin(package.to_string()))?;
    let required = manifest
        .pointer("/engines/node")
        .and_then(Value::as_str)
        .map(str::to_string);
    check_node(&spec, required.as_deref())?;

    // a concurrent run may have stored this version while npm worked
    if is_stored(package, version)? {
        return Ok(true);
    }
    if storage.exists() {
        fs::remove_dir_all(&storage)?;
    }
    fs::create_dir_all(storage.parent().expect("storage dir has a parent"))?;
    fs::rename(&staging.0, &storage)?;
    Ok(true)
}

/** the npm arguments that install `spec` into `prefix`, apart from every
other project. no package.json and no lockfile get written there, and the
dev dependencies of the tool stay out. */
fn install_args(prefix: &Path, spec: &str) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["install".into(), "--prefix".into()];
    args.push(prefix.into());
    for flag in [
        "--no-save",
        "--no-package-lock",
        "--no-audit",
        "--no-fund",
        "--omit=dev",
        "--loglevel=error",
    ] {
        args.push(flag.into());
    }
    args.push(spec.into());
    args
}

/** the command that runs a stored tool: node and the bin script that
`alias` names. the caller adds the arguments. */
pub fn command(alias: &str, package: &str, version: &str) -> Result<Command, Error> {
    let dir = package_dir(&storage_dir(package, version)?, package);
    let manifest = read_manifest(&dir).ok_or_else(|| Error::ToolNotInstalled(alias.to_string()))?;
    let script = bin_script(manifest.get("bin"), alias, package)
        .ok_or_else(|| Error::NpmNoBin(package.to_string()))?;
    let mut command = Command::new("node");
    command.arg(dir.join(script));
    Ok(command)
}

fn read_manifest(dir: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(dir.join("package.json")).ok()?).ok()
}

/** the bin script `alias` runs. a string `bin` is the one command of the
package. a map gives the entry named `alias`, else its only entry, else
the entry named after the package. */
fn bin_script(bin: Option<&Value>, alias: &str, package: &str) -> Option<String> {
    match bin? {
        Value::String(script) => Some(script.clone()),
        Value::Object(entries) => entries
            .get(alias)
            .or_else(|| {
                (entries.len() == 1)
                    .then(|| entries.values().next())
                    .flatten()
            })
            .or_else(|| entries.get(short_name(package)))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/** the alias `embr tool add` gives a package: its only command, so
`azul-sync` becomes `azul`, else the package name without its scope. */
fn default_alias(bin: Option<&Value>, package: &str) -> String {
    match bin {
        Some(Value::Object(entries)) if entries.len() == 1 => {
            entries.keys().next().expect("one entry").clone()
        }
        _ => short_name(package).to_string(),
    }
}

fn short_name(package: &str) -> &str {
    package.rsplit('/').next().unwrap_or(package)
}

/** asks npm which version `wanted` means, the latest when None, and the
alias that version's command takes. nothing is installed. */
pub fn resolve(package: &str, wanted: Option<&str>) -> Result<(String, String), Error> {
    let spec = format!("{package}@{}", wanted.unwrap_or("latest"));
    let output = npm()
        .args(["view", &spec, "version", "bin", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(not_found_is_node_missing)?;
    let answer: Option<Value> = serde_json::from_slice(&output.stdout).ok();
    /* npm answers one object, or a list of them when the spec matches
    several versions. the last one is the newest */
    let answer = match answer {
        Some(Value::Array(mut answers)) => answers.pop(),
        answer => answer,
    };
    let version = answer
        .as_ref()
        .and_then(|answer| answer.get("version"))
        .and_then(Value::as_str);
    match version {
        Some(version) if output.status.success() => Ok((
            version.to_string(),
            default_alias(answer.as_ref().and_then(|a| a.get("bin")), package),
        )),
        _ => Err(Error::NpmFailed {
            spec,
            output: npm_error(&output.stderr),
        }),
    }
}

/// the newest version of a package, for `embr tool update`.
pub fn latest(package: &str) -> Result<String, Error> {
    Ok(resolve(package, None)?.0)
}

/// npm's error lines, without the one that points at its debug log.
fn npm_error(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| !line.contains("A complete log of this run"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// npm is a .cmd script on Windows, which `Command` does not find by its bare name.
fn npm() -> Command {
    Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
}

fn not_found_is_node_missing(error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        Error::NodeMissing
    } else {
        Error::Io(error)
    }
}

/** refuses a Node.js older than the package's `engines.node`. a range the
semver crate cannot read, such as `^18 || >=20`, is not checked here. npm
reads it, and the tool reports its own problem. */
fn check_node(spec: &str, required: Option<&str>) -> Result<(), Error> {
    let output = Command::new("node")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(not_found_is_node_missing)?;
    let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match node_fits(&found, required) {
        true => Ok(()),
        false => Err(Error::NodeTooOld {
            tool: spec.to_string(),
            found,
            required: required.unwrap_or_default().to_string(),
        }),
    }
}

/// whether `node --version` output such as "v20.19.0" meets `required`.
fn node_fits(found: &str, required: Option<&str>) -> bool {
    let Some(required) = required.and_then(|required| semver::VersionReq::parse(required).ok())
    else {
        return true;
    };
    semver::Version::parse(found.trim_start_matches('v'))
        .is_ok_and(|version| required.matches(&version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn storage_keeps_npm_tools_apart_from_github_tools() {
        let dir = storage_dir("azul-sync", "2.4.0").unwrap();
        assert!(
            dir.ends_with(
                Path::new("tools")
                    .join("npm")
                    .join("azul-sync")
                    .join("2.4.0")
            )
        );
        // a scope never becomes a second folder level
        let scoped = storage_dir("@acme/cli", "1.0.0").unwrap();
        assert!(scoped.ends_with(Path::new("npm").join("@acme+cli").join("1.0.0")));
    }

    #[test]
    fn install_writes_into_its_own_prefix_only() {
        let args = install_args(Path::new("/stage"), "azul-sync@2.4.0");
        let args: Vec<_> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(args[..3], ["install", "--prefix", "/stage"]);
        assert!(args.contains(&"--no-save"));
        assert!(args.contains(&"--no-package-lock"));
        assert!(args.contains(&"--omit=dev"));
        assert_eq!(args.last(), Some(&"azul-sync@2.4.0"));
    }

    #[test]
    fn picks_the_bin_script_an_alias_names() {
        let azul = json!({ "azul": "dist/cli.js" });
        // the alias, else the only entry
        assert_eq!(
            bin_script(Some(&azul), "azul", "azul-sync").as_deref(),
            Some("dist/cli.js")
        );
        assert_eq!(
            bin_script(Some(&azul), "sync", "azul-sync").as_deref(),
            Some("dist/cli.js")
        );

        let several = json!({ "tool": "a.js", "tool-dev": "b.js" });
        assert_eq!(
            bin_script(Some(&several), "tool-dev", "tool").as_deref(),
            Some("b.js")
        );
        // no entry named after the alias: the package's own
        assert_eq!(
            bin_script(Some(&several), "other", "@acme/tool").as_deref(),
            Some("a.js")
        );
        assert_eq!(bin_script(Some(&several), "other", "acme"), None);

        // a string bin is the one command
        assert_eq!(
            bin_script(Some(&json!("cli.js")), "x", "x").as_deref(),
            Some("cli.js")
        );
        assert_eq!(bin_script(None, "azul", "azul-sync"), None);
    }

    #[test]
    fn the_default_alias_is_the_command_name() {
        let azul = json!({ "azul": "dist/cli.js" });
        assert_eq!(default_alias(Some(&azul), "azul-sync"), "azul");
        let several = json!({ "a": "a.js", "b": "b.js" });
        assert_eq!(default_alias(Some(&several), "@acme/tool"), "tool");
        assert_eq!(default_alias(None, "@acme/tool"), "tool");
    }

    #[test]
    fn checks_node_against_engines() {
        assert!(node_fits("v20.19.0", Some(">=20.19.0")));
        assert!(node_fits("v26.12.1", Some(">=20.19.0")));
        assert!(!node_fits("v18.20.4", Some(">=20.19.0")));
        // no requirement, or one the semver crate cannot read, passes
        assert!(node_fits("v18.20.4", None));
        assert!(node_fits("v18.20.4", Some("^18 || >=20")));
        // a version that does not parse cannot meet a real requirement
        assert!(!node_fits("", Some(">=20.19.0")));
    }
}
