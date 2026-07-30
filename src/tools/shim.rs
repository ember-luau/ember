/*! How a tool alias becomes a running binary. shims are copies of lpm named
after their alias, aftman-style. invoked under such a name, lpm looks
up which repo@version the surrounding project pins the alias to and
hands off to the stored binary. that lookup is what scopes tools to
their projects. */

use super::stored_executable;
use crate::error::Error;
use crate::project::manifest::{MANIFEST_FILE, Tool};
use crate::sys::paths;
use crate::sys::process;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/** The alias this process was invoked under, or None when running as the
real CLI. "lpm" itself and "lpm-*" stems count as the CLI because
release assets are named "lpm-{os}-{arch}". "lpx" counts too, it's the
`lpm execute` launcher, routed separately in main. */
pub fn shim_alias() -> Option<String> {
    let exe = env::current_exe().ok()?;
    alias_from_stem(exe.file_stem()?.to_str()?)
}

fn alias_from_stem(stem: &str) -> Option<String> {
    let lower = stem.to_ascii_lowercase();
    if lower == "lpm" || lower.starts_with("lpm-") || lower == "lpx" {
        None
    } else {
        Some(stem.to_string())
    }
}

/** whether this process was started as the lpx launcher, a copy of lpm
that `self install` drops in ~/.lpm/bin, in which case every CLI
argument belongs to `lpm execute`. */
pub fn invoked_as_lpx() -> bool {
    let Ok(exe) = env::current_exe() else {
        return false;
    };
    exe.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("lpx"))
}

/** Runs `alias` as a tool shim. on unix the tool replaces this process. the
exit code only comes back on platforms without exec, meaning windows. */
pub fn run(alias: &str) -> Result<i32, Error> {
    let tool = resolve_alias(alias, &env::current_dir()?, &paths::global_tools_file()?)?;
    let stored = stored_executable(&tool.repository, &tool.version)?;
    if !stored.exists() {
        return Err(Error::ToolNotInstalled(alias.to_string()));
    }

    let mut command = std::process::Command::new(&stored);
    command.args(env::args_os().skip(1));
    process::exec(command)
}

/// the shim an alias is invoked through, e.g. ~/.lpm/bin/stylua.
pub fn path(alias: &str) -> Result<PathBuf, Error> {
    Ok(paths::bin_dir()?.join(format!("{alias}{}", env::consts::EXE_SUFFIX)))
}

/// writes the alias shim, a copy of the running lpm executable.
pub fn write(shim: &Path) -> Result<(), Error> {
    fs::create_dir_all(shim.parent().expect("bin dir has a parent"))?;
    fs::copy(env::current_exe()?, shim)?;
    super::make_executable(shim)
}

/** What `alias` currently resolves to on PATH when that is NOT our shim.
another toolchain manager like aftman, rokit, foreman, or a stray copy
shadowing the lpm-managed tool, so users get that manager's errors
instead of what lpm installed. */
pub fn shadowing_executable(alias: &str) -> Option<PathBuf> {
    let bin = paths::bin_dir().ok()?;
    let file = format!("{alias}{}", env::consts::EXE_SUFFIX);
    for dir in env::split_paths(&env::var_os("PATH")?) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(&file);
        if candidate.is_file() {
            // first hit wins, same as OS command lookup
            return if paths::same_file(&dir, &bin) || dir == bin {
                None
            } else {
                Some(candidate)
            };
        }
    }
    None
}

/** Which tool an alias means. the nearest lpm.toml walking up from `start`
whose [tools] lists it wins, then the global tools file. None when
nothing pins it, what `lpm execute` asks before trying shorthands. */
pub fn find_alias(alias: &str, start: &Path, global: &Path) -> Result<Option<Tool>, Error> {
    for dir in start.ancestors() {
        if let Some(tools) = tools_in(&dir.join(MANIFEST_FILE))?
            && let Some(tool) = get_tool(&tools, alias)
        {
            return Ok(Some(tool.clone()));
        }
    }
    if let Some(tools) = tools_in(global)?
        && let Some(tool) = get_tool(&tools, alias)
    {
        return Ok(Some(tool.clone()));
    }
    Ok(None)
}

/// `find_alias`, with absence as the error a shim exits with.
fn resolve_alias(alias: &str, start: &Path, global: &Path) -> Result<Tool, Error> {
    find_alias(alias, start, global)?.ok_or_else(|| Error::ToolNotManaged(alias.to_string()))
}

/** Tools visible from `start` through project manifests, the union over
ancestor lpm.tomls, nearest manifest winning per alias. same scope `run`
resolves aliases against. */
pub fn project_tools(start: &Path) -> Result<BTreeMap<String, Tool>, Error> {
    let mut merged = BTreeMap::new();
    for dir in start.ancestors() {
        if let Some(tools) = tools_in(&dir.join(MANIFEST_FILE))? {
            for (alias, tool) in tools {
                merged.entry(alias).or_insert(tool);
            }
        }
    }
    Ok(merged)
}

/// tools pinned by the global tools file, empty when it doesn't exist yet.
pub fn global_tools() -> Result<BTreeMap<String, Tool>, Error> {
    Ok(tools_in(&paths::global_tools_file()?)?.unwrap_or_default())
}

/** Just the [tools] table, parsed leniently so the shim can read both
project manifests and the global tools file, which has no [package]. */
#[derive(Deserialize)]
struct ToolsTable {
    #[serde(default)]
    tools: BTreeMap<String, Tool>,
}

fn tools_in(path: &Path) -> Result<Option<BTreeMap<String, Tool>>, Error> {
    if !path.exists() {
        return Ok(None);
    }
    let table: ToolsTable = toml::from_str(&fs::read_to_string(path)?)?;
    Ok(Some(table.tools))
}

/** Alias lookup, exact key first. windows resolves commands
case-insensitively, so "Rojo" can hit a manifest key of "rojo". */
fn get_tool<'a>(tools: &'a BTreeMap<String, Tool>, alias: &str) -> Option<&'a Tool> {
    tools.get(alias).or_else(|| {
        tools
            .iter()
            .find_map(|(key, tool)| key.eq_ignore_ascii_case(alias).then_some(tool))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_aliases_exclude_lpm_itself() {
        assert_eq!(alias_from_stem("lpm"), None);
        assert_eq!(alias_from_stem("LPM"), None);
        // release assets run before `self install` renames them.
        assert_eq!(alias_from_stem("lpm-windows-x86_64"), None);
        // the lpx launcher is the CLI too, never a tool shim.
        assert_eq!(alias_from_stem("lpx"), None);
        assert_eq!(alias_from_stem("LPX"), None);
        assert_eq!(alias_from_stem("rojo"), Some("rojo".to_string()));
        assert_eq!(alias_from_stem("StyLua"), Some("StyLua".to_string()));
    }

    #[test]
    fn resolves_aliases_from_nearest_manifest_then_global() {
        let base = std::env::temp_dir().join("lpm-test-resolve-alias");
        let _ = fs::remove_dir_all(&base);

        let project = base.join("project");
        let nested = project.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            project.join(MANIFEST_FILE),
            "[package]\nname = \"scope/name\"\nversion = \"0.1.0\"\n\n[tools]\nrojo = \"rojo-rbx/rojo@7.4.4\"\n",
        )
        .unwrap();
        let global = base.join("tools.toml");
        fs::write(
            &global,
            "[tools]\nstylua = \"johnnymorganz/stylua@2.0.0\"\n",
        )
        .unwrap();

        // project manifests win, found from any directory below them.
        let tool = resolve_alias("rojo", &nested, &global).unwrap();
        assert_eq!(tool.repository, "rojo-rbx/rojo");
        assert_eq!(tool.version, "7.4.4");

        // aliases the project doesn't list fall back to the global file.
        let tool = resolve_alias("stylua", &nested, &global).unwrap();
        assert_eq!(tool.repository, "johnnymorganz/stylua");

        // windows command lookup is case-insensitive, resolution must be too.
        assert!(resolve_alias("Rojo", &nested, &global).is_ok());

        assert!(matches!(
            resolve_alias("wally", &nested, &global),
            Err(Error::ToolNotManaged(_))
        ));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn project_tools_merge_with_nearest_manifest_winning() {
        let base = std::env::temp_dir().join("lpm-test-project-tools");
        let _ = fs::remove_dir_all(&base);

        let inner = base.join("project");
        let start = inner.join("src");
        fs::create_dir_all(&start).unwrap();
        fs::write(
            base.join(MANIFEST_FILE),
            "[tools]\nrojo = \"rojo-rbx/rojo@7.4.4\"\nstylua = \"johnnymorganz/stylua@2.0.0\"\n",
        )
        .unwrap();
        fs::write(
            inner.join(MANIFEST_FILE),
            "[tools]\nrojo = \"rojo-rbx/rojo@7.5.1\"\n",
        )
        .unwrap();

        let tools = project_tools(&start).unwrap();
        assert_eq!(tools["rojo"].version, "7.5.1");
        assert_eq!(tools["stylua"].version, "2.0.0");

        let _ = fs::remove_dir_all(&base);
    }
}
