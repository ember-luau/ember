/*! `embr execute`, alias `x`. downloads a GitHub-released executable if
needed and hands the terminal to it, npx-style. `embx` is this command under
its own name. `self install` drops a copy of embr called embx beside the
tool shims, and main routes that argv[0] here. nothing is ever written to
a manifest. repeat runs inside the TTL come from ~/.ember/tools with zero
network, and past it a failed release lookup falls back to what's stored. */

use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::project::manifest::{EMBER_PREFIX, NPM_PREFIX, Tool, split_package_name};
use crate::sys::{paths, process};
use crate::tools;
use clap::Args;
use std::env;
use std::ffi::OsString;

const AFTER_LONG_HELP: &str = "\
What <SPEC> can be, first match winning:
  a [tools] alias   stylua                 runs the exact version pinned by
                                           the surrounding project or the
                                           global tools file
  a shorthand       create-chief-project,  the names embr knows, then the
                    rojo, stylua, ...      ones the registry adds; the same
                                           list `embr tool add` accepts
  a repository      JohnnyMorganz/StyLua   any GitHub repo with releases
  an Ember tool     ember:acme/tool        a tool on the Ember registry
Add @version for an exact release: stylua@2.0.2.

Unpinned names without @version run the latest release. That answer is
cached and re-checked at the index TTL cadence (EMBER_INDEX_TTL_SECS,
default 300s); --refresh asks again now, and does nothing for pinned or
@version specs, those are already exact. Binaries live under
~/.ember/tools and are reused; no manifest is touched.

Everything after <SPEC> is passed to the executable, hyphens included;
embr's own flags go before it. The exit code is the executable's.

embx runs code fetched from GitHub by name, the same trust decision as
installing a dependency.

Examples:
  embx create-chief-project my-game
  embx stylua --check .
  embr x JohnnyMorganz/wally-package-types@1.2.0 sourcemap.json";

#[derive(Args, Debug)]
#[command(after_long_help = AFTER_LONG_HELP)]
pub struct ExecuteArgs {
    /// What to run, then arguments passed through to it
    #[arg(
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "SPEC"
    )]
    pub command: Vec<OsString>,

    /// Ask GitHub or Ember for the latest release even if a cached answer is fresh
    #[arg(long)]
    pub refresh: bool,
}

/** Resolves the spec, makes sure the executable is stored, and hands the
terminal over. the returned code is the executable's own. on unix this
never returns, the process is replaced outright. */
pub fn run(args: ExecuteArgs) -> Result<i32, Error> {
    let mut command = args.command.into_iter();
    let raw = command.next().expect("SPEC is a required argument");
    let spec = raw
        .to_str()
        .ok_or_else(|| Error::ExecuteSpecInvalid(raw.to_string_lossy().into_owned()))
        .and_then(parse_spec)?;

    // aliases are bare names, owner/repo is always the explicit form
    let pinned = if spec.name.contains('/') {
        None
    } else {
        tools::shim::find_alias(
            &spec.name,
            &env::current_dir()?,
            &paths::global_tools_file()?,
        )?
    };
    let (repository, version) = pick(&spec, pinned.as_ref())?;

    // a tool that npm publishes runs through node, see `tools::npm`
    if let Some(package) = repository.strip_prefix(NPM_PREFIX) {
        let version = match version {
            Some(version) => version,
            None => tools::npm::latest(package)?,
        };
        tools::npm::install(package, &version)?;
        let mut executable = tools::npm::command(&spec.name, package, &version)?;
        executable.args(command);
        return process::exec(executable);
    }

    let github = GithubAPI::new();
    let stored = match &version {
        Some(version) => tools::ensure_stored(&repository, version, &github)?,
        None => tools::ensure_latest(&repository, &github, args.refresh)?,
    };

    let mut executable = std::process::Command::new(&stored);
    executable.args(command);
    process::exec(executable)
}

/// a parsed <SPEC>, what to run and, when given, which exact version.
struct Spec {
    name: String,
    /// no leading 'v', like `Tool::version`
    version: Option<String>,
}

fn parse_spec(raw: &str) -> Result<Spec, Error> {
    let invalid = || Error::ExecuteSpecInvalid(raw.to_string());

    let trimmed = raw.trim();
    let (name, version) = match trimmed.split_once('@') {
        Some((name, version)) => {
            /* strip one 'v' prefix, and only off a number. "v2.0.2" means
            2.0.2, but a tag like "voyager-1" must survive intact */
            let version = match version.strip_prefix('v') {
                Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
                _ => version,
            };
            // a bare "@v" is a truncated version, not a tag named "v"
            if version.is_empty() || version == "v" || version.contains('@') {
                return Err(invalid());
            }
            (name, Some(version.to_string()))
        }
        None => (trimmed, None),
    };

    if name.is_empty() {
        return Err(invalid());
    }
    if let Some(ember) = name.strip_prefix(EMBER_PREFIX) {
        split_package_name(ember).map_err(|_| invalid())?;
    } else if name.contains('/') {
        Tool::split_repository(name).map_err(|_| invalid())?;
    }

    Ok(Spec {
        name: name.to_string(),
        version,
    })
}

/** the precedence ladder, a pinned alias, then a shorthand, then the
explicit owner/repo form. returns the repository and the version now
settled, None meaning "latest". */
fn pick(spec: &Spec, pinned: Option<&Tool>) -> Result<(String, Option<String>), Error> {
    if let Some(tool) = pinned {
        /* an explicit @version disagreeing with the pin is a conflict to
        surface, not something to silently settle either way */
        if let Some(requested) = &spec.version
            && requested != &tool.version
        {
            return Err(Error::ExecutePinConflict {
                alias: spec.name.clone(),
                pinned: tool.version.clone(),
                requested: requested.clone(),
            });
        }
        return Ok((tool.repository.clone(), Some(tool.version.clone())));
    }

    if spec.name.contains('/') {
        return Ok((spec.name.clone(), spec.version.clone()));
    }

    match tools::shorthand_repository(&spec.name) {
        Some(full) => Ok((full, spec.version.clone())),
        None => Err(Error::ExecuteUnknownName(spec.name.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(raw: &str) -> Spec {
        parse_spec(raw).unwrap()
    }

    #[test]
    fn parses_specs_in_every_shape() {
        let parsed = spec("stylua");
        assert_eq!(parsed.name, "stylua");
        assert_eq!(parsed.version, None);

        // versions may carry a leading 'v', it never reaches storage paths
        let parsed = spec("stylua@v2.0.2");
        assert_eq!(parsed.version.as_deref(), Some("2.0.2"));

        // ...but only off a number, named tags keep their 'v'
        let parsed = spec("acme/probe@voyager-1");
        assert_eq!(parsed.version.as_deref(), Some("voyager-1"));

        let parsed = spec("JohnnyMorganz/StyLua@2.0.2");
        assert_eq!(parsed.name, "JohnnyMorganz/StyLua");
        assert_eq!(parsed.version.as_deref(), Some("2.0.2"));

        // an Ember tool runs from Ember, latest or exact
        let parsed = spec("ember:acme/lint@1.0.0");
        assert_eq!(parsed.name, "ember:acme/lint");
        assert_eq!(parsed.version.as_deref(), Some("1.0.0"));
        let (repository, version) = pick(&spec("ember:acme/lint"), None).unwrap();
        assert_eq!((repository.as_str(), version), ("ember:acme/lint", None));
    }

    #[test]
    fn rejects_malformed_specs() {
        for raw in [
            "",
            "   ",
            "@1.0",
            "stylua@",
            "stylua@v",
            "a@1@2",
            "owner/",
            "/repo",
            "a/b/c",
            "owner/repo@",
            "ember:Acme/lint",
            "ember:acme",
            "ember:../lint",
        ] {
            assert!(
                matches!(parse_spec(raw), Err(Error::ExecuteSpecInvalid(_))),
                "spec {raw:?} should be rejected"
            );
        }
    }

    #[test]
    fn pinned_aliases_win_and_conflicts_surface() {
        let pin = Tool {
            repository: "JohnnyMorganz/StyLua".to_string(),
            version: "2.0.0".to_string(),
        };

        // no version, the pin decides both repo and version
        let (repository, version) = pick(&spec("stylua"), Some(&pin)).unwrap();
        assert_eq!(repository, "JohnnyMorganz/StyLua");
        assert_eq!(version.as_deref(), Some("2.0.0"));

        // restating the pinned version is fine
        assert!(pick(&spec("stylua@2.0.0"), Some(&pin)).is_ok());

        // a disagreeing version is a conflict, not a silent override
        assert!(matches!(
            pick(&spec("stylua@2.2.1"), Some(&pin)),
            Err(Error::ExecutePinConflict { .. })
        ));
    }

    #[test]
    fn shorthands_then_explicit_repositories() {
        let (repository, version) = pick(&spec("create-chief-project"), None).unwrap();
        assert_eq!(repository, "ryancundiff/create-chief-project");
        assert_eq!(version, None);

        let (repository, version) = pick(&spec("rojo@7.4.4"), None).unwrap();
        assert_eq!(repository, "rojo-rbx/rojo");
        assert_eq!(version.as_deref(), Some("7.4.4"));

        // explicit repos pass through untouched, casing included
        let (repository, _) = pick(&spec("Acme/Tool"), None).unwrap();
        assert_eq!(repository, "Acme/Tool");

        // unknown bare names never guess
        assert!(matches!(
            pick(&spec("wally"), None),
            Err(Error::ExecuteUnknownName(_))
        ));
    }
}
