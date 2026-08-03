pub mod edit;

use crate::error::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

pub const MANIFEST_FILE: &str = "lpm.toml";

/// [indices] key used for dependencies that name no index.
pub const DEFAULT_INDEX_NAME: &str = "default";

/** lpm's own package index, the fallback for dependencies naming no index when
the project defines no `default` one. pesde-format entries whose `download` URLs
point at the registry CDN. written only by the lpm API at publish time, read by
the CLI like any other git index. */
pub const DEFAULT_INDEX_URL: &str = "https://github.com/luaupm/index";

#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    /** who this package is, for the registry. optional: a manifest that only
    consumes packages -- a game, an app, anything that never publishes -- has
    nothing to put here. `lpm publish` is the one command that demands it, see
    [`Error::PackageMissing`]. */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Package>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(default, skip_serializing_if = "Config::is_default")]
    pub config: Config,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub indices: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, Dependency>,
    /// replacements for dependencies of dependencies. see [`Override`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, Override>,
    /** diffs applied to dependencies at install, "scope/name@version" ->
    a repo-relative .patch file, written by `lpm patch commit`. root
    manifest only, same as [overrides]. a dependency's own table is never
    consulted, and publish strips it. keys parse with [`patch_key`],
    values validate with [`patch_path`]. */
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patches: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Tool>,
    /// shell commands runnable with `lpm run <name>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scripts: BTreeMap<String, String>,
    /// what `lpm studio open` opens in Roblox Studio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub studio: Option<Studio>,
}

/// per-environment install locations, each defaults to "packages/<env>".
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Config {
    /// `shared-packages-out` is the pre-rename spelling, see [`Environment::Roblox`].
    #[serde(
        rename = "roblox-packages-out",
        alias = "shared-packages-out",
        skip_serializing_if = "Option::is_none"
    )]
    pub roblox_packages_out: Option<String>,
    #[serde(
        rename = "server-packages-out",
        skip_serializing_if = "Option::is_none"
    )]
    pub server_packages_out: Option<String>,
    #[serde(rename = "lune-packages-out", skip_serializing_if = "Option::is_none")]
    pub lune_packages_out: Option<String>,
    #[serde(rename = "luau-packages-out", skip_serializing_if = "Option::is_none")]
    pub luau_packages_out: Option<String>,
    #[serde(rename = "lute-packages-out", skip_serializing_if = "Option::is_none")]
    pub lute_packages_out: Option<String>,
}

impl Config {
    fn is_default(&self) -> bool {
        self.roblox_packages_out.is_none()
            && self.server_packages_out.is_none()
            && self.lune_packages_out.is_none()
            && self.luau_packages_out.is_none()
            && self.lute_packages_out.is_none()
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Package {
    pub name: String,
    pub version: String,
    /** never published. workspace roots that only exist to hold members set this
    so publish skips them. chief's root manifest is the archetype. */
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Target {
    pub environment: Environment,
    /** entry point, e.g. "src/init.luau". not needed when `workspace` is set,
    a workspace root has no code of its own to require. */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    /// member globs relative to this manifest, like `workspace = ["packages/*"]`.
    /// `!` negates, literal "." includes the root. having members makes this
    /// a workspace root, publish/install there run for every member. pesde's
    /// spelling `workspace_members` is accepted as an alias. read through
    /// [`Manifest::workspace_members`].
    #[serde(
        rename = "workspace",
        alias = "workspace_members",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub workspace: Vec<String>,
    /// what goes into the published archive, plain file/dir paths or globs
    /// like `src/*`. a dir covers everything under it. empty means everything
    /// sensible, the default walk minus VCS/output dirs.
    #[serde(default, alias = "include", skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
    /** subtracted from whatever `includes` or the default walk selected, same
    path-or-glob entries. lpm.toml always ships. */
    #[serde(default, alias = "exclude", skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,
}

/// where a package's Luau code runs. output folders under packages/ use the serialized lowercase form.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    /** Luau code that must run in Roblox, on either side.
    spelled `shared` before this rename, after wally's realm. that name read
    as "shared between packages" often enough to be worth losing, so `roblox`
    is what everything writes now -- but `shared` still parses everywhere,
    since every already-published manifest and lockfile says it. */
    #[serde(alias = "shared")]
    Roblox,
    /// Roblox server-side only
    Server,
    /// needs the Lune runtime
    Lune,
    /// standalone Luau, runnable with the luau CLI
    Luau,
    /// needs the Lute runtime
    Lute,
}

impl Environment {
    pub const ALL: [Environment; 5] = [
        Environment::Roblox,
        Environment::Server,
        Environment::Lune,
        Environment::Luau,
        Environment::Lute,
    ];

    pub fn dir_name(self) -> &'static str {
        match self {
            Environment::Roblox => "roblox",
            Environment::Server => "server",
            Environment::Lune => "lune",
            Environment::Luau => "luau",
            Environment::Lute => "lute",
        }
    }

    /// translates a pesde `target.environment` value.
    pub fn from_pesde(environment: &str) -> Result<Self, Error> {
        match environment {
            "roblox" => Ok(Environment::Roblox),
            "roblox_server" => Ok(Environment::Server),
            "lune" => Ok(Environment::Lune),
            "luau" => Ok(Environment::Luau),
            "lute" => Ok(Environment::Lute),
            other => Err(Error::UnsupportedEnvironment(other.to_string())),
        }
    }

    /// translates a wally `realm` value.
    pub fn from_wally_realm(realm: &str) -> Result<Self, Error> {
        match realm {
            "shared" => Ok(Environment::Roblox),
            "server" => Ok(Environment::Server),
            other => Err(Error::UnsupportedEnvironment(other.to_string())),
        }
    }

    /// parses lpm's own environment names like "roblox" and "lune". `shared` is the pre-rename spelling of `roblox`.
    pub fn from_lpm(environment: &str) -> Result<Self, Error> {
        match environment {
            "roblox" | "shared" => Ok(Environment::Roblox),
            "server" => Ok(Environment::Server),
            "lune" => Ok(Environment::Lune),
            "luau" => Ok(Environment::Luau),
            "lute" => Ok(Environment::Lute),
            other => Err(Error::UnsupportedEnvironment(other.to_string())),
        }
    }
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir_name())
    }
}

/// the [studio] table, either a published place with both IDs or a local place file.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Studio {
    /// universe (experience) ID the place belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub universe: Option<u64>,
    /// place ID inside the universe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<u64>,
    /// path to a .rbxl/.rbxlx place file, relative to lpm.toml.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /** anything else written under [studio]. the table gets hand-edited a lot and
    a typo'd key would otherwise read as "unconfigured", so `target()` reports
    these. only there, though. `deny_unknown_fields` would let a [studio] typo
    fail unrelated commands. */
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// a validated [studio] table, the one thing `lpm studio open` should open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StudioTarget {
    /// published place, opened through the roblox-studio: protocol.
    Place { universe: u64, place: u64 },
    /// local place file, opened through the OS file association.
    File(String),
}

impl Studio {
    /// reduces the table to what to open. everything that can be wrong with a hand-written [studio] gets caught here, before any launch attempt.
    pub fn target(&self) -> Result<StudioTarget, Error> {
        if let Some((key, _)) = self.unknown.first_key_value() {
            return Err(Error::StudioUnknownKey(key.clone()));
        }
        if self.file.is_some() && (self.universe.is_some() || self.place.is_some()) {
            return Err(Error::StudioConflict);
        }

        match (self.file.as_deref(), self.universe, self.place) {
            (Some(file), ..) => {
                let file = file.trim();
                if file.is_empty() {
                    return Err(Error::StudioEmptyFile);
                }
                Ok(StudioTarget::File(file.to_string()))
            }
            (None, Some(universe), Some(place)) => {
                for (key, id) in [("universe", universe), ("place", place)] {
                    if id == 0 {
                        return Err(Error::StudioInvalidId(key));
                    }
                }
                Ok(StudioTarget::Place { universe, place })
            }
            (None, Some(_), None) => Err(Error::StudioIncomplete {
                has: "universe",
                needs: "place",
            }),
            (None, None, Some(_)) => Err(Error::StudioIncomplete {
                has: "place",
                needs: "universe",
            }),
            (None, None, None) => Err(Error::StudioUnconfigured),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum Dependency {
    /// package from an index: `{ name = "scope/pkg", version = "^", index = "wally" }`.
    Registry {
        /// "scope/name" identifier.
        name: String,
        /// semver requirement. "^" alone means "latest".
        version: String,
        /// key into [indices]. None means the default luaupm index.
        #[serde(skip_serializing_if = "Option::is_none")]
        index: Option<String>,
        /// see [`Dependency::target`]. kept unparsed so a typo reports itself
        /// instead of failing the untagged match with "no variant".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    /** another member of this workspace, pesde-style:
    `{ workspace = "scope/pkg", version = "^" }`. linked in place during
    development. `version` is a version TYPE like `^`, `~`, `=`, `*`, or a full
    requirement, applied to the member's current version only at publish time,
    see [`workspace_version_req`]. */
    Workspace {
        workspace: String,
        #[serde(default = "default_workspace_version")]
        version: String,
        /// see [`Dependency::target`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
}

fn default_workspace_version() -> String {
    "^".to_string()
}

impl Dependency {
    /** the `target` an entry states, e.g.
    `Chief = { name = "chief/core", version = "^", target = "server" }`.

    it overrides which environment tree a *direct* dependency installs
    under, so a package published for Roblox can be pulled into the server
    root instead of the project's own, and it picks the target of a
    multi-target index entry to match. parsed by [`dependency_target`],
    which is also where the roblox/server restriction lives.

    direct only, and that is a rule about the tree, not a shortcut. every
    other entry reaches the resolver with a context already inherited from
    the root of its subtree, and moving one out of that tree would strand
    its dependents: they link what is installed alongside them and nothing
    else. so `target` on a specifier lpm only ever meets transitively --
    an `[overrides]` value, or a workspace member's own dependency while
    the *root* installs -- has no effect. it is honored when that member
    installs itself, where the same entry is a direct dependency. */
    pub fn target(&self) -> Option<&str> {
        match self {
            Dependency::Registry { target, .. } | Dependency::Workspace { target, .. } => {
                target.as_deref()
            }
        }
    }
}

/** parses a dependency's `target` override, see [`Dependency::target`].
`alias` only names the entry in the error.

only the two Roblox trees can be chosen. the rest are runtimes -- moving a
Roblox package into `lune` would put it where nothing that runs it lives,
and lpm would have no way to tell that from a typo. */
pub fn dependency_target(
    alias: &str,
    dependency: &Dependency,
) -> Result<Option<Environment>, Error> {
    let Some(target) = dependency.target() else {
        return Ok(None);
    };
    match Environment::from_lpm(target.trim()) {
        Ok(environment @ (Environment::Roblox | Environment::Server)) => Ok(Some(environment)),
        _ => Err(Error::DependencyTargetInvalid {
            alias: alias.to_string(),
            target: target.to_string(),
        }),
    }
}

/** an [overrides] value, what an alias path should resolve to instead of
what the dependency's own manifest asked for. keys name an edge. in
`"Foo.Bar"`, `Foo` is an alias under this manifest's [dependencies] and
`Bar` is the alias Foo's own manifest declares. aliases are case-sensitive
and spelled exactly as each parent wrote them, package names are not what
paths are made of. values are either the alias of one of this manifest's
own [dependencies] entries, `"Foo.Bar" = "Bar"` meaning use mine, or a
full specifier.

semantics under lpm's flat install model, plainly. an override redirects
that one edge. other dependents that ask for the original package by
name keep it, and both packages then coexist in the set, each linked
where it was asked for. only the root manifest's [overrides] is
consulted, a workspace member's own table applies when the member
itself installs, and a published package's is inert. a package's edges
are walked once, on its first discovery, root aliases seeding in
alphabetical order, so a three-segment path must spell that first
route. segments below an overridden edge address the *replacement's*
dependencies. */
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum Override {
    /// alias of an entry under this manifest's own [dependencies]
    Alias(String),
    /// a dependency specifier, same shapes as a [dependencies] value
    Specifier(Dependency),
}

/** expands an [overrides] key into alias paths, dot-separated chains from
a direct dependency down. one key can cover several paths separated by
commas, `"foo.bar, baz.bar" = ...`. a path needs at least two segments,
a single alias would name one of your own [dependencies], which you can
just edit. aliases that themselves contain dots or whitespace can't be
addressed. pick the specifier form under a different alias if that ever
bites. */
pub fn override_paths(key: &str) -> Result<Vec<Vec<String>>, Error> {
    key.split(',')
        .map(|path| {
            let segments: Vec<String> = path.trim().split('.').map(str::to_string).collect();
            let valid = segments.len() >= 2
                && segments
                    .iter()
                    .all(|segment| !segment.is_empty() && !segment.contains(char::is_whitespace));
            if !valid {
                return Err(Error::OverrideBadPath(key.to_string()));
            }
            Ok(segments)
        })
        .collect()
}

/** splits a `[patches]` key, "scope/name@1.2.3" -> (name, exact version).
split at the first '@', so build metadata like `acme/foo@1.0.0+build.5` simply
lives inside the version. exact because a patch is a claim about one
published archive. a range would make "which bytes" depend on resolution. */
pub fn patch_key(key: &str) -> Result<(String, semver::Version), Error> {
    let invalid = |reason: String| Error::PatchKeyInvalid {
        key: key.to_string(),
        reason,
    };
    let Some((name, version)) = key.split_once('@') else {
        return Err(invalid("expected 'scope/name@version'".to_string()));
    };
    if split_package_name(name).is_err() {
        return Err(invalid(format!(
            "'{name}' is not a 'scope/name' package name"
        )));
    }
    let version = semver::Version::parse(version)
        .map_err(|error| invalid(format!("'{version}' is not an exact version ({error})")))?;
    Ok((name.to_string(), version))
}

/** validates a `[patches]` value, a relative path that stays inside the
project. absolute paths and `..` escapes are refused. a patch is part of
the repo, or installs stop being reproducible for anyone else. */
pub fn patch_path(key: &str, path: &str) -> Result<std::path::PathBuf, Error> {
    use std::path::Component;

    let invalid = || Error::PatchPathInvalid {
        key: key.to_string(),
        path: path.to_string(),
    };
    let relative = Path::new(path);
    if path.is_empty() || relative.is_absolute() {
        return Err(invalid());
    }
    let mut depth: i64 = 0;
    for component in relative.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Err(invalid()); // climbed out of the project
                }
            }
            // windows drive prefixes and root markers are absolute in spirit
            Component::Prefix(_) | Component::RootDir => return Err(invalid()),
        }
    }
    Ok(relative.to_path_buf())
}

/** the registry requirement a workspace dependency publishes as, pesde's rules
exactly. `^`/`~`/`=` prefix the member's current version, `^` + `1.2.3` ->
`^1.2.3`. `*` stays "any". anything else must already be a full semver
requirement and passes through unchanged. */
pub fn workspace_version_req(
    version: &str,
    member_version: &semver::Version,
) -> Result<String, Error> {
    let version = version.trim();
    Ok(match version {
        "*" => "*".to_string(),
        "" | "^" => format!("^{member_version}"),
        "~" => format!("~{member_version}"),
        "=" => format!("={member_version}"),
        other => {
            semver::VersionReq::parse(other)?;
            other.to_string()
        }
    })
}

/// a GitHub-released binary tool, written in lpm.toml as the single string "owner/repo@version" under [tools], the key being the alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// "owner/repo" GitHub repository.
    pub repository: String,
    /// exact release version, no leading 'v'.
    pub version: String,
}

impl Tool {
    /// parses an "owner/repo@version" tool spec.
    pub fn parse(spec: &str) -> Result<Self, Error> {
        let invalid = || Error::InvalidToolSpec(spec.to_string());

        let (repository, version) = spec.trim().split_once('@').ok_or_else(invalid)?;
        if version.is_empty() || version.contains('@') {
            return Err(invalid());
        }
        Self::split_repository(repository).map_err(|_| invalid())?;

        Ok(Tool {
            repository: repository.to_string(),
            version: version.to_string(),
        })
    }

    /** splits an "owner/repo" GitHub repository name. unlike index package names,
    see `split_package_name`, GitHub owners/repos may contain uppercase and dots
    like "JohnnyMorganz/StyLua", so mostly the shape is validated, exactly one '/',
    both halves non-empty. backslashes and dot-only components are rejected.
    GitHub never allows them, and either could steer the storage path out of
    ~/.lpm/tools on windows. */
    pub fn split_repository(repository: &str) -> Result<(&str, &str), Error> {
        let valid =
            |half: &str| !half.is_empty() && half != "." && half != ".." && !half.contains('\\');
        match repository.split_once('/') {
            Some((owner, repo)) if valid(owner) && valid(repo) && !repo.contains('/') => {
                Ok((owner, repo))
            }
            _ => Err(Error::InvalidToolSpec(repository.to_string())),
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.repository, self.version)
    }
}

impl Serialize for Tool {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Tool {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let spec = String::deserialize(deserializer)?;
        Tool::parse(&spec).map_err(serde::de::Error::custom)
    }
}

impl Manifest {
    /// loads the manifest from the current directory.
    pub fn load() -> Result<Self, Error> {
        Self::load_from(Path::new(MANIFEST_FILE))
    }

    /// member globs from [target] workspace. empty = not a workspace root.
    pub fn workspace_members(&self) -> &[String] {
        self.target
            .as_ref()
            .map(|target| target.workspace.as_slice())
            .unwrap_or(&[])
    }

    pub fn load_from(path: &Path) -> Result<Self, Error> {
        if !path.exists() {
            return Err(Error::ManifestMissing);
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// folder an environment's packages (and their link files) install to.
    pub fn packages_out(&self, environment: Environment) -> std::path::PathBuf {
        let configured = match environment {
            Environment::Roblox => &self.config.roblox_packages_out,
            Environment::Server => &self.config.server_packages_out,
            Environment::Lune => &self.config.lune_packages_out,
            Environment::Luau => &self.config.luau_packages_out,
            Environment::Lute => &self.config.lute_packages_out,
        };
        match configured {
            Some(dir) => std::path::PathBuf::from(dir),
            None => std::path::Path::new("packages").join(environment.dir_name()),
        }
    }

    /** resolves a dependency's `index` key to an index URL. no key = the `default`
    entry under [indices] when the project defines one, lpm's own index otherwise. */
    pub fn index_url(&self, index: Option<&str>) -> Result<&str, Error> {
        match index {
            None => Ok(self
                .indices
                .get(DEFAULT_INDEX_NAME)
                .map(String::as_str)
                .unwrap_or(DEFAULT_INDEX_URL)),
            Some(key) => self
                .indices
                .get(key)
                .map(String::as_str)
                .ok_or_else(|| Error::UnknownIndex(key.to_string())),
        }
    }

    /// the [package] name, when the manifest declares one at all.
    pub fn name(&self) -> Option<&str> {
        self.package.as_ref().map(|package| package.name.as_str())
    }

    /// "scope/name@version", how lpm names this package in its own output. None without a [package] table -- a consuming-only project has no such name.
    pub fn id(&self) -> Option<String> {
        self.package
            .as_ref()
            .map(|package| format!("{}@{}", package.name, package.version))
    }

    /// the command a `[scripts]` entry runs. read side only, used by `lpm run`. edits go through `edit::ManifestDoc`.
    pub fn script(&self, name: &str) -> Result<&str, Error> {
        self.scripts
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| Error::ScriptMissing(name.to_string()))
    }
}

/** shape of a GitHub username, 1-39 chars, alphanumeric or dashes, no leading/
trailing/consecutive dash. `[package] authors` must pass this since the registry
appends authors to the scope's owner list on publish for co-ownership and 400s
anything that isn't a username. shape only, account existence is never checked,
so typos still bite. */
pub fn is_github_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 39
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// splits a "scope/name" package identifier. wally allows dashes, so parts accept lowercase letters, digits, dashes, underscores.
pub fn split_package_name(name: &str) -> Result<(&str, &str), Error> {
    let is_valid_part = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    };

    match name.split('/').collect::<Vec<_>>().as_slice() {
        [scope, package] if is_valid_part(scope) && is_valid_part(package) => Ok((scope, package)),
        _ => Err(Error::InvalidPackageName(name.to_string())),
    }
}

/// parses a manifest version requirement. bare "^" (or "*") means "latest".
pub fn parse_version_req(req: &str) -> Result<semver::VersionReq, Error> {
    let trimmed = req.trim();
    if trimmed == "^" || trimmed == "*" || trimmed.is_empty() {
        Ok(semver::VersionReq::STAR)
    } else {
        Ok(semver::VersionReq::parse(trimmed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_manifest() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"
            authors = ["Someone"]
            license = "MIT"

            [target]
            environment = "lune"
            main = "src/init.luau"

            [indices]
            wally = "https://github.com/UpliftGames/wally-index"

            [dependencies]
            Chief = { name = "chief/core", version = "^" }
            Other = { name = "user/other_package", version = "^", index = "wally" }

            [tools]
            stylua = "johnnymorganz/stylua@2.0.0"
            StyLua = "JohnnyMorganz/StyLua@2.1.0"

            [scripts]
            build = "rojo build -o game.rbxl"
            "#,
        )
        .unwrap();

        assert_eq!(manifest.name(), Some("scope/name"));
        assert_eq!(manifest.id().as_deref(), Some("scope/name@0.1.0"));
        assert_eq!(
            manifest.target.as_ref().unwrap().environment,
            Environment::Lune
        );
        assert!(matches!(
            &manifest.dependencies["Chief"],
            Dependency::Registry { index: None, .. }
        ));
        assert!(matches!(
            &manifest.dependencies["Other"],
            Dependency::Registry { index: Some(index), .. } if index == "wally"
        ));
        assert_eq!(manifest.tools["stylua"].repository, "johnnymorganz/stylua");
        assert_eq!(manifest.tools["stylua"].version, "2.0.0");
        assert_eq!(manifest.tools["StyLua"].repository, "JohnnyMorganz/StyLua");
        assert_eq!(manifest.tools["StyLua"].version, "2.1.0");
        assert_eq!(manifest.script("build").unwrap(), "rojo build -o game.rbxl");
        assert!(matches!(
            manifest.script("test"),
            Err(Error::ScriptMissing(_))
        ));
    }

    #[test]
    fn resolves_index_urls() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [indices]
            wally = "https://example.com/wally-index"
            "#,
        )
        .unwrap();

        // no `default` key, bare deps fall back to lpm's own index.
        assert_eq!(manifest.index_url(None).unwrap(), DEFAULT_INDEX_URL);
        assert_eq!(
            manifest.index_url(Some("wally")).unwrap(),
            "https://example.com/wally-index"
        );
        assert!(matches!(
            manifest.index_url(Some("missing")),
            Err(Error::UnknownIndex(_))
        ));
    }

    #[test]
    fn default_index_key_resolves_bare_dependencies() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [indices]
            default = "https://example.com/my-index"
            "#,
        )
        .unwrap();

        assert_eq!(
            manifest.index_url(None).unwrap(),
            "https://example.com/my-index"
        );
    }

    #[test]
    fn packages_out_defaults_and_overrides() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [config]
            roblox-packages-out = "src/ReplicatedStorage/Packages"
            "#,
        )
        .unwrap();

        assert_eq!(
            manifest.packages_out(Environment::Roblox),
            std::path::PathBuf::from("src/ReplicatedStorage/Packages")
        );
        assert_eq!(
            manifest.packages_out(Environment::Luau),
            std::path::PathBuf::from("packages").join("luau")
        );
        assert_eq!(
            manifest.packages_out(Environment::Lute),
            std::path::PathBuf::from("packages").join("lute")
        );

        // the pre-rename key still points the same environment somewhere else
        let legacy: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [config]
            shared-packages-out = "src/ReplicatedStorage/Packages"
            "#,
        )
        .unwrap();
        assert_eq!(
            legacy.packages_out(Environment::Roblox),
            std::path::PathBuf::from("src/ReplicatedStorage/Packages")
        );
        // and is rewritten under the new name whenever lpm writes the manifest
        let serialized = toml::to_string(&legacy).unwrap();
        assert!(serialized.contains("roblox-packages-out"), "{serialized}");
        assert!(!serialized.contains("shared-packages-out"), "{serialized}");
    }

    #[test]
    fn translates_environments() {
        assert_eq!(
            Environment::from_pesde("roblox").unwrap(),
            Environment::Roblox
        );
        assert_eq!(
            Environment::from_pesde("roblox_server").unwrap(),
            Environment::Server
        );
        assert_eq!(Environment::from_pesde("lune").unwrap(), Environment::Lune);
        assert!(Environment::from_pesde("nonsense").is_err());
        assert_eq!(
            Environment::from_wally_realm("shared").unwrap(),
            Environment::Roblox
        );
        assert_eq!(
            Environment::from_wally_realm("server").unwrap(),
            Environment::Server
        );
        assert!(Environment::from_wally_realm("lune").is_err());
    }

    /** the rename's compatibility contract. `shared` is what every manifest,
    lockfile and index entry published before it says, so it has to keep
    parsing, in every reader -- while `roblox` is the only spelling lpm
    writes, and the only folder name it installs into. */
    #[test]
    fn shared_is_still_read_as_roblox_everywhere() {
        assert_eq!(
            Environment::from_lpm("shared").unwrap(),
            Environment::Roblox
        );
        assert_eq!(
            Environment::from_lpm("roblox").unwrap(),
            Environment::Roblox
        );
        assert_eq!(Environment::Roblox.dir_name(), "roblox");
        assert_eq!(Environment::Roblox.to_string(), "roblox");

        let manifest: Manifest = toml::from_str(
            "[package]\nname = \"scope/name\"\nversion = \"0.1.0\"\n\n\
             [target]\nenvironment = \"shared\"\n",
        )
        .unwrap();
        assert_eq!(
            manifest.target.as_ref().unwrap().environment,
            Environment::Roblox
        );
        let serialized = toml::to_string(&manifest).unwrap();
        assert!(
            serialized.contains(r#"environment = "roblox""#),
            "{serialized}"
        );
    }

    /** [package] is only what a *publishable* package needs. a project that
    just consumes packages has no name or version to state, and every
    command but `publish` must work without one. */
    #[test]
    fn manifests_without_a_package_table_are_projects() {
        let manifest: Manifest =
            toml::from_str("[dependencies]\nChief = { name = \"chief/core\", version = \"^\" }\n")
                .unwrap();

        assert!(manifest.package.is_none());
        assert_eq!(manifest.name(), None);
        assert_eq!(manifest.id(), None);
        assert_eq!(manifest.dependencies.len(), 1);

        // an absent table stays absent on write, it isn't invented as empty
        let serialized = toml::to_string(&manifest).unwrap();
        assert!(!serialized.contains("[package]"), "{serialized}");
        // ...and an entirely empty manifest is a valid project too
        let empty: Manifest = toml::from_str("").unwrap();
        assert!(empty.package.is_none() && empty.dependencies.is_empty());
    }

    #[test]
    fn dependency_targets_are_limited_to_the_roblox_trees() {
        let manifest: Manifest = toml::from_str(
            r#"
            [dependencies]
            Chief = { name = "chief/core", version = "^", target = "server" }
            Legacy = { name = "acme/legacy", version = "^", target = "shared" }
            Plain = { name = "acme/plain", version = "^" }
            Member = { workspace = "acme/member", target = "server" }
            Runtime = { name = "acme/runtime", version = "^", target = "lune" }
            Typo = { name = "acme/typo", version = "^", target = "sever" }
            "#,
        )
        .unwrap();

        let target = |alias: &str| dependency_target(alias, &manifest.dependencies[alias]);
        assert_eq!(target("Chief").unwrap(), Some(Environment::Server));
        // `shared` reads as `roblox` here like everywhere else
        assert_eq!(target("Legacy").unwrap(), Some(Environment::Roblox));
        assert_eq!(target("Plain").unwrap(), None);
        // workspace specifiers take one too, so it can't be silently ignored
        assert_eq!(target("Member").unwrap(), Some(Environment::Server));

        /* a runtime environment and a typo are both refused, and both name
        the entry: an untagged-enum "matched no variant" would name neither */
        for alias in ["Runtime", "Typo"] {
            assert!(
                matches!(target(alias), Err(Error::DependencyTargetInvalid { alias: named, .. }) if named == alias),
                "{alias} should be rejected"
            );
        }

        // the key round-trips, so `lpm publish` doesn't silently drop it
        let serialized = toml::to_string(&manifest).unwrap();
        assert!(serialized.contains(r#"target = "server""#), "{serialized}");
        let reparsed: Manifest = toml::from_str(&serialized).unwrap();
        assert_eq!(
            dependency_target("Chief", &reparsed.dependencies["Chief"]).unwrap(),
            Some(Environment::Server)
        );
    }

    #[test]
    fn parses_workspace_manifests_and_dependencies() {
        /* the chief repo's shape, private root listing member globs, members
        depending on each other with workspace specifiers. */
        let root: Manifest = toml::from_str(
            r#"
            [package]
            name = "chief/root"
            version = "0.0.0"
            private = true

            [target]
            environment = "roblox"
            workspace = ["packages/*", "!packages/legacy"]
            "#,
        )
        .unwrap();
        assert!(root.package.as_ref().unwrap().private);
        // a root needs a [target] but no `main`. members carry the code.
        assert!(root.target.as_ref().unwrap().main.is_none());
        assert_eq!(root.workspace_members(), ["packages/*", "!packages/legacy"]);

        // pesde's spelling still parses so converted repos keep working.
        let aliased: Manifest = toml::from_str(
            r#"
            [package]
            name = "chief/root"
            version = "0.0.0"

            [target]
            environment = "roblox"
            workspace_members = ["packages/*"]
            "#,
        )
        .unwrap();
        assert_eq!(aliased.workspace_members(), ["packages/*"]);

        let member: Manifest = toml::from_str(
            r#"
            [package]
            name = "chief/dependencies"
            version = "0.1.0"

            [dependencies]
            core = { workspace = "chief/core", version = "^" }
            pinned = { workspace = "chief/bin" }
            "#,
        )
        .unwrap();
        assert!(matches!(
            &member.dependencies["core"],
            Dependency::Workspace { workspace, version, .. }
                if workspace == "chief/core" && version == "^"
        ));
        // version defaults to "^", pesde's default version type.
        assert!(matches!(
            &member.dependencies["pinned"],
            Dependency::Workspace { version, .. } if version == "^"
        ));

        // not-a-workspace manifests don't accidentally gain the fields.
        let plain: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"
            "#,
        )
        .unwrap();
        assert!(!plain.package.as_ref().unwrap().private);
        assert!(plain.workspace_members().is_empty());
        let serialized = toml::to_string(&plain).unwrap();
        assert!(!serialized.contains("private"));
        assert!(!serialized.contains("workspace"));
    }

    #[test]
    fn workspace_version_reqs_follow_pesde_rules() {
        let version = semver::Version::new(2, 1, 5);
        assert_eq!(workspace_version_req("^", &version).unwrap(), "^2.1.5");
        assert_eq!(workspace_version_req("~", &version).unwrap(), "~2.1.5");
        assert_eq!(workspace_version_req("=", &version).unwrap(), "=2.1.5");
        assert_eq!(workspace_version_req("*", &version).unwrap(), "*");
        assert_eq!(workspace_version_req("", &version).unwrap(), "^2.1.5");
        // a full requirement passes through unchanged.
        assert_eq!(workspace_version_req("^2.1.0", &version).unwrap(), "^2.1.0");
        assert!(workspace_version_req("not a req", &version).is_err());
    }

    #[test]
    fn patch_keys_split_at_the_first_at_sign() {
        let (name, version) = patch_key("chief/traits@0.2.0").unwrap();
        assert_eq!(name, "chief/traits");
        assert_eq!(version, semver::Version::new(0, 2, 0));
        // round trips, formatting the parts gives the key back
        assert_eq!(format!("{name}@{version}"), "chief/traits@0.2.0");

        /* build metadata lives inside the version, not treated as some
        suffix of the key */
        let (name, version) = patch_key("acme/foo@1.0.0+build.5").unwrap();
        assert_eq!(name, "acme/foo");
        assert_eq!(version.to_string(), "1.0.0+build.5");
        assert_eq!(format!("{name}@{version}"), "acme/foo@1.0.0+build.5");

        // prereleases are exact versions too
        assert!(patch_key("acme/foo@1.0.0-rc.1").is_ok());

        for bad in [
            "chief/traits",        // no @
            "chief/traits@",       // empty version
            "chief/traits@^0.2.0", // a range is not one archive
            "chief/traits@0.2",    // not a full version
            "traits@0.2.0",        // not scope/name
            "Chief/Traits@0.2.0",  // uppercase names are invalid
            "@0.2.0",
        ] {
            assert!(
                matches!(patch_key(bad), Err(Error::PatchKeyInvalid { .. })),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn patch_paths_stay_inside_the_project() {
        assert!(patch_path("k", "patches/chief_traits@0.2.0.patch").is_ok());
        assert!(patch_path("k", "./patches/x.patch").is_ok());
        // dipping down then back up is fine as long as it never escapes
        assert!(patch_path("k", "patches/../patches/x.patch").is_ok());

        for bad in ["", "/etc/x.patch", "../x.patch", "patches/../../x.patch"] {
            assert!(
                matches!(patch_path("k", bad), Err(Error::PatchPathInvalid { .. })),
                "{bad:?} should be rejected"
            );
        }
        #[cfg(windows)]
        assert!(patch_path("k", "C:\\x.patch").is_err());
    }

    #[test]
    fn recognizes_github_username_shapes() {
        for name in ["octocat", "Luau-PM", "a", "user123", "x-1-y"] {
            assert!(is_github_username(name), "{name:?} should be accepted");
        }
        for name in [
            "",
            "-octocat",
            "octocat-",
            "double--dash",
            "with space",
            "name@example.com",
            "Jane Doe <jane@example.com>",
            "under_score",
            &"a".repeat(40),
        ] {
            assert!(!is_github_username(name), "{name:?} should be rejected");
        }
    }

    #[test]
    fn splits_package_names() {
        assert_eq!(
            split_package_name("evaera/promise").unwrap(),
            ("evaera", "promise")
        );
        assert_eq!(
            split_package_name("scope/pkg-name_2").unwrap(),
            ("scope", "pkg-name_2")
        );
        assert!(split_package_name("noslash").is_err());
        assert!(split_package_name("Upper/case").is_err());
        assert!(split_package_name("a/b/c").is_err());
    }

    #[test]
    fn parses_tool_specs() {
        let tool = Tool::parse("  JohnnyMorganz/StyLua@2.0.0  ").unwrap();
        assert_eq!(tool.repository, "JohnnyMorganz/StyLua");
        assert_eq!(tool.version, "2.0.0");
        assert_eq!(tool.to_string(), "JohnnyMorganz/StyLua@2.0.0");
    }

    #[test]
    fn rejects_invalid_tool_specs() {
        for spec in [
            "norepo@1.0",
            "owner/repo",
            "owner/repo@",
            "a/b/c@1.0",
            "@1.0",
            "owner/@1.0",
            "/repo@1.0",
            "owner/repo@1.0@2.0",
            "",
            // path-shaped names must never reach storage_dir
            "../evil@1.0",
            "owner/..@1.0",
            "owner/re\\po@1.0",
        ] {
            assert!(
                matches!(Tool::parse(spec), Err(Error::InvalidToolSpec(_))),
                "spec {spec:?} should be rejected"
            );
        }
    }

    #[test]
    fn tool_round_trips_through_toml() {
        #[derive(Serialize, Deserialize)]
        struct Tools {
            stylua: Tool,
        }

        let tools = Tools {
            stylua: Tool {
                repository: "JohnnyMorganz/StyLua".to_string(),
                version: "2.0.0".to_string(),
            },
        };
        let serialized = toml::to_string(&tools).unwrap();
        assert_eq!(
            serialized.trim(),
            r#"stylua = "JohnnyMorganz/StyLua@2.0.0""#
        );
        let parsed: Tools = toml::from_str(&serialized).unwrap();
        assert_eq!(parsed.stylua, tools.stylua);
    }

    #[test]
    fn includes_and_excludes_round_trip() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [target]
            environment = "luau"
            includes = ["src", "lpm.toml", "README.md"]
            excludes = ["src/tests"]
            "#,
        )
        .unwrap();

        let target = manifest.target.as_ref().unwrap();
        assert_eq!(target.includes, ["src", "lpm.toml", "README.md"]);
        assert_eq!(target.excludes, ["src/tests"]);

        let serialized = toml::to_string(&manifest).unwrap();
        let parsed: Manifest = toml::from_str(&serialized).unwrap();
        let reparsed = parsed.target.as_ref().unwrap();
        assert_eq!(reparsed.includes, target.includes);
        assert_eq!(reparsed.excludes, target.excludes);

        // the singular spellings parse too.
        let aliased: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [target]
            environment = "luau"
            include = ["src"]
            exclude = ["tests"]
            "#,
        )
        .unwrap();
        assert_eq!(aliased.target.as_ref().unwrap().includes, ["src"]);
        assert_eq!(aliased.target.as_ref().unwrap().excludes, ["tests"]);

        // absent lists stay absent on write.
        let bare: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [target]
            environment = "luau"
            "#,
        )
        .unwrap();
        assert!(bare.target.as_ref().unwrap().includes.is_empty());
        let serialized = toml::to_string(&bare).unwrap();
        assert!(!serialized.contains("includes"));
        assert!(!serialized.contains("excludes"));
    }

    #[test]
    fn parses_studio_place_ids() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [studio]
            universe = 13058
            place = 1818
            "#,
        )
        .unwrap();

        assert_eq!(
            manifest.studio.unwrap().target().unwrap(),
            StudioTarget::Place {
                universe: 13058,
                place: 1818
            }
        );
    }

    #[test]
    fn parses_studio_file() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [studio]
            file = "place.rbxl"
            "#,
        )
        .unwrap();

        assert_eq!(
            manifest.studio.unwrap().target().unwrap(),
            StudioTarget::File("place.rbxl".to_string())
        );
    }

    #[test]
    fn studio_target_catches_hand_edit_mistakes() {
        let studio = |universe, place, file: Option<&str>| Studio {
            universe,
            place,
            file: file.map(str::to_string),
            ..Default::default()
        };

        // both forms at once is ambiguous, even with only half the IDs.
        assert!(matches!(
            studio(Some(1), Some(2), Some("a.rbxl")).target(),
            Err(Error::StudioConflict)
        ));
        assert!(matches!(
            studio(Some(1), None, Some("a.rbxl")).target(),
            Err(Error::StudioConflict)
        ));

        // one ID without the other.
        assert!(matches!(
            studio(Some(1), None, None).target(),
            Err(Error::StudioIncomplete {
                has: "universe",
                needs: "place"
            })
        ));
        assert!(matches!(
            studio(None, Some(2), None).target(),
            Err(Error::StudioIncomplete {
                has: "place",
                needs: "universe"
            })
        ));

        // nothing at all, zero IDs, blank file path.
        assert!(matches!(
            studio(None, None, None).target(),
            Err(Error::StudioUnconfigured)
        ));
        assert!(matches!(
            studio(Some(0), Some(2), None).target(),
            Err(Error::StudioInvalidId("universe"))
        ));
        assert!(matches!(
            studio(Some(1), Some(0), None).target(),
            Err(Error::StudioInvalidId("place"))
        ));
        assert!(matches!(
            studio(None, None, Some("   ")).target(),
            Err(Error::StudioEmptyFile)
        ));

        // surrounding whitespace in the path is tolerated.
        assert_eq!(
            studio(None, None, Some(" place.rbxl ")).target().unwrap(),
            StudioTarget::File("place.rbxl".to_string())
        );
    }

    #[test]
    fn studio_flags_unknown_keys_without_failing_the_manifest() {
        /* a typo'd key must not break `Manifest::load`, that would take
        install/add/run down with it, but `target()` names it. */
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [studio]
            universeId = 13058
            place = 1818
            "#,
        )
        .unwrap();

        assert!(matches!(
            manifest.studio.unwrap().target(),
            Err(Error::StudioUnknownKey(key)) if key == "universeId"
        ));
    }

    #[test]
    fn studio_stays_absent_when_unset() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"
            "#,
        )
        .unwrap();
        assert!(manifest.studio.is_none());
        assert!(!toml::to_string(&manifest).unwrap().contains("studio"));
    }

    #[test]
    fn expands_override_paths() {
        assert_eq!(
            override_paths("foo.bar").unwrap(),
            [vec!["foo".to_string(), "bar".to_string()]]
        );
        // one key can cover several paths, whitespace around commas is noise
        assert_eq!(
            override_paths("foo.bar, baz.qux.bar").unwrap(),
            [
                vec!["foo".to_string(), "bar".to_string()],
                vec!["baz".to_string(), "qux".to_string(), "bar".to_string()],
            ]
        );

        // a single alias, empty segments, and stray dots are all authoring errors
        for key in ["foo", "foo.", ".bar", "foo..bar", "", "a.b,c"] {
            assert!(
                matches!(override_paths(key), Err(Error::OverrideBadPath(_))),
                "key {key:?} should be rejected"
            );
        }
    }

    #[test]
    fn parses_override_values_in_both_forms() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [dependencies]
            Bar = { name = "acme/bar", version = "^2" }

            [overrides]
            "foo.bar" = "Bar"
            "foo.baz" = { name = "acme/qux", version = "^1", index = "wally" }
            "#,
        )
        .unwrap();

        assert!(matches!(
            &manifest.overrides["foo.bar"],
            Override::Alias(alias) if alias == "Bar"
        ));
        assert!(matches!(
            &manifest.overrides["foo.baz"],
            Override::Specifier(Dependency::Registry { name, index, .. })
                if name == "acme/qux" && index.as_deref() == Some("wally")
        ));

        // absent table stays absent on write
        let bare: Manifest =
            toml::from_str("[package]\nname = \"scope/name\"\nversion = \"0.1.0\"\n").unwrap();
        assert!(!toml::to_string(&bare).unwrap().contains("overrides"));
    }

    #[test]
    fn parses_version_requirements() {
        let latest = parse_version_req("^").unwrap();
        assert!(latest.matches(&semver::Version::new(99, 0, 0)));
        let caret = parse_version_req("^1.2").unwrap();
        assert!(caret.matches(&semver::Version::new(1, 9, 0)));
        assert!(!caret.matches(&semver::Version::new(2, 0, 0)));
        assert!(parse_version_req("not a version").is_err());
    }
}
