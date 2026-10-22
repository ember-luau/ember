pub mod edit;

use crate::error::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

pub const MANIFEST_FILE: &str = "ember.toml";

/// the manifest's name before the ember rename. still read, never written.
pub const LEGACY_MANIFEST_FILE: &str = "lpm.toml";

/** The manifest to read inside `dir`.

Prefers ember.toml, falls back to lpm.toml, so a project written before the
rename keeps resolving without being touched. When neither is there it returns
the ember.toml path, so the caller reports the current name as missing rather
than the old one. */
pub fn manifest_in(dir: &Path) -> std::path::PathBuf {
    let current = dir.join(MANIFEST_FILE);
    if current.exists() {
        return current;
    }
    let legacy = dir.join(LEGACY_MANIFEST_FILE);
    if legacy.exists() { legacy } else { current }
}

/// Whether `file` is a manifest under either name.
pub fn is_manifest(file: &Path) -> bool {
    matches!(
        file.to_str(),
        Some(name) if name == MANIFEST_FILE || name == LEGACY_MANIFEST_FILE
    )
}

/// [indices] key used for dependencies that name no index.
pub const DEFAULT_INDEX_NAME: &str = "default";

/** ember's own package index, the fallback for dependencies naming no index when
the project defines no `default` one. pesde-format entries whose `download` URLs
point at the registry CDN. written only by the ember API at publish time, read by
the CLI like any other git index. */
pub const DEFAULT_INDEX_URL: &str = "https://github.com/ember-luau/index";

#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    /** who this package is, for the registry. optional: a manifest that only
    consumes packages -- a game, an app, anything that never publishes -- has
    nothing to put here. `embr publish` is the one command that demands it, see
    [`Error::PackageMissing`]. */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Package>,
    /** what the package ships, `[target]` for one target or `[[target]]`
    for several. the first is the project's own: install, the fit warnings,
    init, workspaces, and `embr add` read only that one. read it through
    [`Manifest::target`] and [`Manifest::targets`]. */
    #[serde(
        rename = "target",
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "serialize_targets",
        deserialize_with = "deserialize_targets"
    )]
    pub(crate) targets: Vec<Target>,
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
    a repo-relative .patch file, written by `embr patch commit`. root
    manifest only, same as [overrides]. a dependency's own table is never
    consulted, and publish strips it. keys parse with [`patch_key`],
    values validate with [`patch_path`]. */
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patches: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Tool>,
    /// shell commands runnable with `embr run <name>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scripts: BTreeMap<String, Script>,
    /// what `embr studio open` opens in Roblox Studio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub studio: Option<Studio>,
}

/** A `[scripts]` entry: one shell command, or several to run at once.

```toml
[scripts]
build = "rojo build -o game.rbxl"
serve = ["larvae process -w", "rojo serve .larvae/build.project.json"]
```

The array form is what the `concurrently` npm package exists to provide, and
it earns its place here for the same reason: a dev loop is usually two or
three long-running processes that have to come up together, and expressing
that as `a & b & wait` is neither portable to Windows nor able to tag whose
output is whose.

The two forms are not the same execution model, which is the thing to know
about them. One command inherits embr's stdio outright, so a TTY stays a TTY
and interactive programs work. Several have their output read through pipes
so each line can be tagged, which costs them that. See `sys::process`. */
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum Script {
    /// one shell command.
    Single(String),
    /// several shell commands, all started at once.
    Parallel(Vec<String>),
}

impl Script {
    /// the command lines this entry runs, in order. one for [`Script::Single`].
    pub fn commands(&self) -> &[String] {
        match self {
            Script::Single(command) => std::slice::from_ref(command),
            Script::Parallel(commands) => commands,
        }
    }
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
    #[serde(rename = "seal-packages-out", skip_serializing_if = "Option::is_none")]
    pub seal_packages_out: Option<String>,
    #[serde(rename = "zune-packages-out", skip_serializing_if = "Option::is_none")]
    pub zune_packages_out: Option<String>,
    /// how the requires embr writes look, see [`RequireStyle`].
    #[serde(default, skip_serializing_if = "RequireStyle::is_string")]
    pub requires: RequireStyle,
}

/** how embr writes the requires it generates: link files, and package
requires it points at their new place (#43). */
#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RequireStyle {
    /** `require("./packages/roblox/x")`, which every runtime reads. embr
    also turns a package's own `script.Parent.X` requires into strings. */
    #[default]
    String,
    /** `require(script.Parent.packages.roblox.x)`, Roblox's dot requires.
    a package's own instance requires stay as it wrote them. */
    Instance,
}

impl RequireStyle {
    fn is_string(&self) -> bool {
        *self == RequireStyle::String
    }
}

impl Config {
    fn is_default(&self) -> bool {
        self.roblox_packages_out.is_none()
            && self.server_packages_out.is_none()
            && self.lune_packages_out.is_none()
            && self.luau_packages_out.is_none()
            && self.lute_packages_out.is_none()
            && self.seal_packages_out.is_none()
            && self.zune_packages_out.is_none()
            && self.requires.is_string()
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
    /** a picture of the package, a .png, .jpg, or .webp file inside it. the
    registry shows it in place of the box icon. it always ships. */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
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
    path-or-glob entries. ember.toml always ships. */
    #[serde(default, alias = "exclude", skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,
    /** what the shipped files are written in. independent of `environment`:
    an Alloy package for Lune is `lune` plus `alloy`. */
    #[serde(default, skip_serializing_if = "Language::is_luau")]
    pub language: Language,
}

impl Target {
    /** the name that the index keys this target by. it is the environment
    alone for Luau, else `<environment>-<language>`, such as `roblox-alloy`. */
    pub fn id(&self) -> String {
        match self.language {
            Language::Luau => self.environment.to_string(),
            language => format!("{}-{language}", self.environment),
        }
    }
}

/// writes one target as a `[target]` table and several as `[[target]]`.
fn serialize_targets<S: serde::Serializer>(
    targets: &[Target],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match targets {
        [target] => target.serialize(serializer),
        targets => targets.serialize(serializer),
    }
}

/** reads `[target]` or `[[target]]` into one list, and refuses two targets
with the same ID: the index has one entry for each ID. */
fn deserialize_targets<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Target>, D::Error> {
    use serde::de::Error as _;
    let targets = match toml::Value::deserialize(deserializer)? {
        toml::Value::Array(items) => items
            .into_iter()
            .map(Target::deserialize)
            .collect::<Result<Vec<_>, _>>(),
        table => Target::deserialize(table).map(|target| vec![target]),
    }
    .map_err(D::Error::custom)?;
    let mut ids = std::collections::HashSet::new();
    for target in &targets {
        let id = target.id();
        if !ids.insert(id.clone()) {
            return Err(D::Error::custom(format!(
                "two targets have the ID '{id}'; give each one a different environment or language"
            )));
        }
    }
    Ok(targets)
}

/** the language of a package's own files. Alloy and Loom build to plain
Luau, so their packages install wherever a Luau one does. Luwu runs only on
a Luwu host, so its packages need a Luwu project. */
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Luau,
    /// a strict superset of Luau, built by `alloy build`
    Alloy,
    /// a Roblox DSL, built by `loom build`
    Loom,
    Luwu,
}

impl Language {
    pub fn is_luau(&self) -> bool {
        *self == Language::Luau
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Language::Luau => "luau",
            Language::Alloy => "alloy",
            Language::Loom => "loom",
            Language::Luwu => "luwu",
        }
    }

    /// the language that `as_str` spells `name`. None for a name embr does not know.
    pub fn parse(name: &str) -> Option<Language> {
        [
            Language::Luau,
            Language::Alloy,
            Language::Loom,
            Language::Luwu,
        ]
        .into_iter()
        .find(|language| language.as_str() == name)
    }

    /** the compiler whose `build` writes this language's Luau before a
    publish. None for languages that ship their sources as they are. */
    pub fn builder(self) -> Option<&'static str> {
        match self {
            Language::Alloy => Some("alloy"),
            Language::Loom => Some("loom"),
            Language::Luau | Language::Luwu => None,
        }
    }

    /** whether a project in `self` takes a package in `package`: its own
    language, or plain Luau, the base every language reads. languages do
    not reach into each other, so a Luau project takes no Alloy package. */
    pub fn accepts(self, package: Language) -> bool {
        package == self || package == Language::Luau
    }

    /// the name people write: "Luau", "Alloy", "Loom", "Luwu".
    pub fn title(self) -> &'static str {
        match self {
            Language::Luau => "Luau",
            Language::Alloy => "Alloy",
            Language::Loom => "Loom",
            Language::Luwu => "Luwu",
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
    /** Roblox server-side only. it takes pesde's name, `roblox_server`, and
    reads `server`, the name before, everywhere. its packages folder keeps the
    name `server`, see [`Environment::dir_name`]. */
    #[serde(rename = "roblox_server", alias = "server")]
    Server,
    /// needs the Lune runtime
    Lune,
    /// standalone Luau, runnable with the luau CLI
    Luau,
    /// needs the Lute runtime
    Lute,
    /// needs the seal runtime, which loads `.luau` files only
    Seal,
    /// needs the Zune runtime
    Zune,
}

impl Environment {
    pub const ALL: [Environment; 7] = [
        Environment::Roblox,
        Environment::Server,
        Environment::Lune,
        Environment::Luau,
        Environment::Lute,
        Environment::Seal,
        Environment::Zune,
    ];

    /** the folder under packages/ that this environment's packages install
    to. it differs from [`Environment::as_str`] only for `roblox_server`,
    whose folder stays `server` so existing Rojo project paths keep working. */
    pub fn dir_name(self) -> &'static str {
        match self {
            Environment::Roblox => "roblox",
            Environment::Server => "server",
            Environment::Lune => "lune",
            Environment::Luau => "luau",
            Environment::Lute => "lute",
            Environment::Seal => "seal",
            Environment::Zune => "zune",
        }
    }

    /// the name that embr writes and prints, such as `roblox_server`.
    pub fn as_str(self) -> &'static str {
        match self {
            Environment::Server => "roblox_server",
            other => other.dir_name(),
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

    /** parses embr's own environment names like "roblox" and "lune". `shared`
    is the pre-rename spelling of `roblox`, and `server` of `roblox_server`. */
    pub fn from_embr(environment: &str) -> Result<Self, Error> {
        match environment {
            "roblox" | "shared" => Ok(Environment::Roblox),
            "roblox_server" | "server" => Ok(Environment::Server),
            "lune" => Ok(Environment::Lune),
            "luau" => Ok(Environment::Luau),
            "lute" => Ok(Environment::Lute),
            "seal" => Ok(Environment::Seal),
            "zune" => Ok(Environment::Zune),
            other => Err(Error::UnsupportedEnvironment(other.to_string())),
        }
    }

    /** whether a project in `self` can use a package published for
    `package`. portable Luau runs anywhere, a Roblox server runs shared
    Roblox code (wally's realm rule), and Lune runs Roblox code through
    its `@lune/roblox` DOM. */
    pub fn accepts(self, package: Environment) -> bool {
        self == package
            || package == Environment::Luau
            || (matches!(self, Environment::Server | Environment::Lune)
                && package == Environment::Roblox)
    }
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
    /// path to a .rbxl/.rbxlx place file, relative to ember.toml.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /** anything else written under [studio]. the table gets hand-edited a lot and
    a typo'd key would otherwise read as "unconfigured", so `target()` reports
    these. only there, though. `deny_unknown_fields` would let a [studio] typo
    fail unrelated commands. */
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// a validated [studio] table, the one thing `embr studio open` should open.
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
        /// key into [indices]. None means the default embr index.
        #[serde(skip_serializing_if = "Option::is_none")]
        index: Option<String>,
        /// see [`Dependency::target`]. kept unparsed so a typo reports itself
        /// instead of failing the untagged match with "no variant".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        /// see [`Dependency::entry`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<String>,
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
        /// see [`Dependency::entry`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<String>,
    },
}

fn default_workspace_version() -> String {
    "^".to_string()
}

impl Dependency {
    /** the `target` an entry states, e.g.
    `Chief = { name = "chief/core", version = "^", target = "roblox_server" }`.

    it overrides which environment tree a *direct* dependency installs
    under, so a package published for Roblox can be pulled into the server
    root instead of the project's own, and it picks the target of a
    multi-target index entry to match. parsed by [`dependency_target`],
    which is also where the roblox/server restriction lives.

    direct only, and that is a rule about the tree, not a shortcut. every
    other entry reaches the resolver with a context already inherited from
    the root of its subtree, and moving one out of that tree would strand
    its dependents: they link what is installed alongside them and nothing
    else. so `target` on a specifier embr only ever meets transitively --
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

    /** the `entry` an entry states, e.g.
    `Vow = { name = "synttx/vow", version = "^", entry = "src/vow" }`.

    which module inside the package its link file points at, for packages
    that name none themselves and ship nothing embr can guess from. it beats
    both what the package declares and the guess, so it is also the way out
    of a guess that picked the wrong file.

    keyed by package, not by alias, so it reaches the same package wherever
    it turns up: state it on a direct dependency and a dependent's nested
    link to that package uses it too. */
    pub fn entry(&self) -> Option<&str> {
        match self {
            Dependency::Registry { entry, .. } | Dependency::Workspace { entry, .. } => {
                entry.as_deref()
            }
        }
    }

    /// the package this entry names, whichever shape it takes.
    pub fn package(&self) -> &str {
        match self {
            Dependency::Registry { name, .. } => name,
            Dependency::Workspace { workspace, .. } => workspace,
        }
    }
}

/** parses a dependency's `target` override, see [`Dependency::target`].
`alias` only names the entry in the error.

only the two Roblox trees can be chosen. the rest are runtimes -- moving a
Roblox package into `lune` would put it where nothing that runs it lives,
and embr would have no way to tell that from a typo. */
pub fn dependency_target(
    alias: &str,
    dependency: &Dependency,
) -> Result<Option<Environment>, Error> {
    let Some(target) = dependency.target() else {
        return Ok(None);
    };
    match Environment::from_embr(target.trim()) {
        Ok(environment) => Ok(Some(environment)),
        Err(_) => Err(Error::DependencyTargetInvalid {
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

semantics under embr's flat install model, plainly. an override redirects
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

/// a GitHub-released binary tool, written in ember.toml as the single string "owner/repo@version" under [tools], the key being the alias.
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
    ~/.ember/tools on windows. */
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
    /// loads the manifest from the current directory, under either name.
    pub fn load() -> Result<Self, Error> {
        Self::load_from(&manifest_in(Path::new("")))
    }

    /// the project's own target, the first one. None without a [target].
    pub fn target(&self) -> Option<&Target> {
        self.targets.first()
    }

    /// every target, in the order that the manifest gives them.
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// member globs from [target] workspace. empty = not a workspace root.
    pub fn workspace_members(&self) -> &[String] {
        self.target()
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
            Environment::Seal => &self.config.seal_packages_out,
            Environment::Zune => &self.config.zune_packages_out,
        };
        match configured {
            Some(dir) => std::path::PathBuf::from(dir),
            None => std::path::Path::new("packages").join(environment.dir_name()),
        }
    }

    /** the require style for packages that install under `context`. only
    Roblox has a `script` to climb from, so the other trees keep strings. */
    pub fn require_style(&self, context: Environment) -> RequireStyle {
        match context {
            Environment::Roblox | Environment::Server => self.config.requires,
            _ => RequireStyle::String,
        }
    }

    /** the `entry` this manifest states for the package `name`, see
    [`Dependency::entry`]. matched case-insensitively, the way package names
    resolve everywhere else. */
    pub fn entry_override(&self, name: &str) -> Option<&str> {
        self.dependencies
            .values()
            .find(|dependency| dependency.package().eq_ignore_ascii_case(name))
            .and_then(Dependency::entry)
    }

    /** resolves a dependency's `index` key to an index URL. no key = the `default`
    entry under [indices] when the project defines one, embr's own index otherwise. */
    pub fn index_url(&self, index: Option<&str>) -> Result<&str, Error> {
        match index {
            /* the project's own default, then the machine's from
            `embr self default-index`, then ember's */
            None => Ok(self
                .indices
                .get(DEFAULT_INDEX_NAME)
                .map(String::as_str)
                .or_else(|| crate::project::settings::default_index())
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

    /// "scope/name@version", how embr names this package in its own output. None without a [package] table -- a consuming-only project has no such name.
    pub fn id(&self) -> Option<String> {
        self.package
            .as_ref()
            .map(|package| format!("{}@{}", package.name, package.version))
    }

    /** the `[scripts]` entry named `name`. read side only, used by `embr run`.
    edits go through `edit::ManifestDoc`.

    An entry with nothing in it, `serve = []`, is refused rather than run as a
    no-op: it can only be a half-finished edit, and succeeding silently is the
    one outcome that would hide that. */
    pub fn script(&self, name: &str) -> Result<&Script, Error> {
        let script = self
            .scripts
            .get(name)
            .ok_or_else(|| Error::ScriptMissing(name.to_string()))?;
        if script.commands().is_empty() {
            return Err(Error::ScriptEmpty(name.to_string()));
        }
        Ok(script)
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

    /// an empty dir holding the given manifest names.
    fn scratch(name: &str, files: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("embr-test-manifest-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), "[dependencies]\n").unwrap();
        }
        dir
    }

    #[test]
    fn the_current_manifest_wins_over_the_legacy_one() {
        let dir = scratch("prefers-current", &[MANIFEST_FILE, LEGACY_MANIFEST_FILE]);
        assert_eq!(manifest_in(&dir), dir.join(MANIFEST_FILE));
    }

    #[test]
    fn a_project_from_before_the_rename_still_resolves() {
        let dir = scratch("legacy-only", &[LEGACY_MANIFEST_FILE]);
        assert_eq!(manifest_in(&dir), dir.join(LEGACY_MANIFEST_FILE));
    }

    #[test]
    fn with_no_manifest_at_all_the_current_name_is_reported() {
        let dir = scratch("neither", &[]);
        // so "no ember.toml found" names the file the user should expect
        assert_eq!(manifest_in(&dir), dir.join(MANIFEST_FILE));
    }

    #[test]
    fn both_manifest_names_pack_and_publish_as_manifests() {
        assert!(is_manifest(Path::new(MANIFEST_FILE)));
        assert!(is_manifest(Path::new(LEGACY_MANIFEST_FILE)));
        assert!(!is_manifest(Path::new("src/init.luau")));
        // a nested manifest is a dependency's, not this project's
        assert!(!is_manifest(Path::new("packages/dep/ember.toml")));
    }

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
        assert_eq!(manifest.target().unwrap().environment, Environment::Lune);
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
        assert_eq!(
            manifest.script("build").unwrap().commands(),
            ["rojo build -o game.rbxl"]
        );
        assert!(matches!(
            manifest.script("test"),
            Err(Error::ScriptMissing(_))
        ));
    }

    #[test]
    fn scripts_take_a_string_or_a_list() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"

            [scripts]
            build = "rojo build -o game.rbxl"
            serve = ["larvae process -w", "rojo serve .larvae/build.project.json"]
            empty = []
            "#,
        )
        .unwrap();

        // one command either way, so callers never special-case the shape
        assert_eq!(
            manifest.script("build").unwrap().commands(),
            ["rojo build -o game.rbxl"]
        );
        assert_eq!(
            manifest.script("serve").unwrap().commands(),
            ["larvae process -w", "rojo serve .larvae/build.project.json"]
        );

        // an empty list is an unfinished edit, not a script that does nothing
        assert!(matches!(
            manifest.script("empty"),
            Err(Error::ScriptEmpty(name)) if name == "empty"
        ));
        assert!(matches!(
            manifest.script("absent"),
            Err(Error::ScriptMissing(_))
        ));

        // both forms survive a write, each keeping the shape it was written in
        let serialized = toml::to_string(&manifest).unwrap();
        let reparsed: Manifest = toml::from_str(&serialized).unwrap();
        assert_eq!(reparsed.scripts, manifest.scripts);
        assert!(matches!(reparsed.scripts["build"], Script::Single(_)));
        assert!(matches!(reparsed.scripts["serve"], Script::Parallel(_)));
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

        // no `default` key, bare deps fall back to embr's own index.
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

    /// #43: `[config] requires` picks the style, and only Roblox trees take instances.
    #[test]
    fn require_style_applies_to_roblox_trees() {
        let parse = |config: &str| {
            toml::from_str::<Manifest>(&format!(
                "[package]\nname = \"scope/name\"\nversion = \"0.1.0\"\n\n[config]\n{config}"
            ))
        };

        let manifest = parse("requires = \"instance\"\n").unwrap();
        assert_eq!(
            manifest.require_style(Environment::Roblox),
            RequireStyle::Instance
        );
        assert_eq!(
            manifest.require_style(Environment::Server),
            RequireStyle::Instance
        );
        assert_eq!(
            manifest.require_style(Environment::Lune),
            RequireStyle::String
        );

        let manifest = parse("").unwrap();
        assert_eq!(
            manifest.require_style(Environment::Roblox),
            RequireStyle::String
        );
        assert!(parse("requires = \"dots\"\n").is_err());
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
        // and is rewritten under the new name whenever embr writes the manifest
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
    parsing, in every reader -- while `roblox` is the only spelling embr
    writes, and the only folder name it installs into. */
    #[test]
    fn shared_is_still_read_as_roblox_everywhere() {
        assert_eq!(
            Environment::from_embr("shared").unwrap(),
            Environment::Roblox
        );
        assert_eq!(
            Environment::from_embr("roblox").unwrap(),
            Environment::Roblox
        );
        assert_eq!(Environment::Roblox.dir_name(), "roblox");
        assert_eq!(Environment::Roblox.to_string(), "roblox");

        let manifest: Manifest = toml::from_str(
            "[package]\nname = \"scope/name\"\nversion = \"0.1.0\"\n\n\
             [target]\nenvironment = \"shared\"\n",
        )
        .unwrap();
        assert_eq!(manifest.target().unwrap().environment, Environment::Roblox);
        let serialized = toml::to_string(&manifest).unwrap();
        assert!(
            serialized.contains(r#"environment = "roblox""#),
            "{serialized}"
        );
    }

    /** `roblox_server` is pesde's name, and `server` the name before it.
    the old name parses in every reader, embr writes only the new one, and
    the packages folder keeps the old name so Rojo paths do not move. */
    #[test]
    fn server_is_still_read_as_roblox_server() {
        assert_eq!(
            Environment::from_embr("server").unwrap(),
            Environment::Server
        );
        assert_eq!(
            Environment::from_embr("roblox_server").unwrap(),
            Environment::Server
        );
        assert_eq!(Environment::Server.to_string(), "roblox_server");
        assert_eq!(Environment::Server.dir_name(), "server");

        let old: Manifest = toml::from_str(
            "[config]\nserver-packages-out = \"src/ServerPackages\"\n\n\
             [target]\nenvironment = \"server\"\nlanguage = \"alloy\"\n\n\
             [dependencies]\nChief = { name = \"chief/core\", version = \"^\", target = \"server\" }\n",
        )
        .unwrap();
        let target = old.target().unwrap();
        assert_eq!(target.environment, Environment::Server);
        assert_eq!(target.id(), "roblox_server-alloy");
        assert_eq!(
            dependency_target("Chief", &old.dependencies["Chief"]).unwrap(),
            Some(Environment::Server)
        );
        assert_eq!(
            old.packages_out(Environment::Server),
            std::path::PathBuf::from("src/ServerPackages")
        );
        let bare: Manifest = toml::from_str("").unwrap();
        assert_eq!(
            bare.packages_out(Environment::Server),
            std::path::PathBuf::from("packages").join("server")
        );

        // a new write takes the new name, and keeps the config key
        let written = toml::to_string(&old).unwrap();
        assert!(
            written.contains(r#"environment = "roblox_server""#),
            "{written}"
        );
        assert!(written.contains("server-packages-out"), "{written}");
        let reparsed: Manifest = toml::from_str(&written).unwrap();
        assert_eq!(reparsed.target().unwrap().environment, Environment::Server);
        // the fit rule is unchanged: a roblox_server project takes roblox packages
        assert!(Environment::Server.accepts(Environment::Roblox));
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

    /** an `entry` states which module of a package its link points at, for the
    ones that name none themselves, `synttx/vow` being the case that asked for
    it. keyed by package rather than alias, so it reaches that package wherever
    it turns up, including a dependent's nested link to it. */
    #[test]
    fn entries_are_stated_per_package() {
        let manifest: Manifest = toml::from_str(
            r#"
            [dependencies]
            Vow = { name = "synttx/vow", version = "^", entry = "src/vow" }
            Plain = { name = "acme/plain", version = "^" }
            Member = { workspace = "acme/member", entry = "src" }
            "#,
        )
        .unwrap();

        assert_eq!(manifest.entry_override("synttx/vow"), Some("src/vow"));
        // package names resolve case-insensitively everywhere else too
        assert_eq!(manifest.entry_override("Synttx/Vow"), Some("src/vow"));
        assert_eq!(manifest.entry_override("acme/member"), Some("src"));
        // an entry nobody stated, and a package this manifest never mentions
        assert_eq!(manifest.entry_override("acme/plain"), None);
        assert_eq!(manifest.entry_override("acme/absent"), None);

        // survives a write, and stays absent for the entries that stated none
        let serialized = toml::to_string(&manifest).unwrap();
        assert!(serialized.contains(r#"entry = "src/vow""#), "{serialized}");
        let reparsed: Manifest = toml::from_str(&serialized).unwrap();
        assert_eq!(reparsed.entry_override("synttx/vow"), Some("src/vow"));
        assert_eq!(reparsed.entry_override("acme/plain"), None);
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

        /* any environment can be targeted: a `target` that matches the
        package marks a cross-environment dependency as on purpose */
        assert_eq!(target("Runtime").unwrap(), Some(Environment::Lune));
        /* a typo is refused and names the entry: an untagged-enum "matched
        no variant" would not */
        assert!(
            matches!(target("Typo"), Err(Error::DependencyTargetInvalid { alias: named, .. }) if named == "Typo"),
            "Typo should be rejected"
        );

        // the key round-trips, so `embr publish` doesn't silently drop it
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
        assert!(root.target().unwrap().main.is_none());
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
            includes = ["src", "ember.toml", "README.md"]
            excludes = ["src/tests"]
            "#,
        )
        .unwrap();

        let target = manifest.target().unwrap();
        assert_eq!(target.includes, ["src", "ember.toml", "README.md"]);
        assert_eq!(target.excludes, ["src/tests"]);

        let serialized = toml::to_string(&manifest).unwrap();
        let parsed: Manifest = toml::from_str(&serialized).unwrap();
        let reparsed = parsed.target().unwrap();
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
        assert_eq!(aliased.target().unwrap().includes, ["src"]);
        assert_eq!(aliased.target().unwrap().excludes, ["tests"]);

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
        assert!(bare.target().unwrap().includes.is_empty());
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

    #[test]
    fn language_defaults_to_luau_and_round_trips() {
        let plain: Manifest = toml::from_str("[target]\nenvironment = \"lune\"\n").unwrap();
        assert_eq!(plain.target().unwrap().language, Language::Luau);
        // the default stays out of the file, so existing manifests keep their text
        assert!(!toml::to_string(&plain).unwrap().contains("language"));

        let alloy: Manifest =
            toml::from_str("[target]\nenvironment = \"lune\"\nlanguage = \"alloy\"\n").unwrap();
        assert_eq!(alloy.target().unwrap().language, Language::Alloy);
        assert!(
            toml::to_string(&alloy)
                .unwrap()
                .contains(r#"language = "alloy""#)
        );

        assert!(
            toml::from_str::<Manifest>("[target]\nenvironment = \"lune\"\nlanguage = \"rust\"\n")
                .is_err()
        );
    }

    #[test]
    fn targets_take_one_table_or_several() {
        // one [target] stays one table when embr writes the manifest
        let one: Manifest =
            toml::from_str("[target]\nenvironment = \"roblox\"\nmain = \"src/init.luau\"\n")
                .unwrap();
        assert_eq!(one.targets().len(), 1);
        let written = toml::to_string(&one).unwrap();
        assert!(
            written.contains("[target]") && !written.contains("[[target]]"),
            "{written}"
        );

        // several stay an array of tables, in their order
        let several: Manifest = toml::from_str(
            r#"
            [package]
            name = "acme/multi"
            version = "1.0.0"

            [[target]]
            environment = "roblox"
            main = "src/init.luau"

            [[target]]
            environment = "roblox"
            language = "alloy"
            main = "out/init.luau"

            [[target]]
            environment = "luau"
            language = "luwu"
            main = "src/init.luwu"

            [dependencies]
            Signal = { name = "acme/signal", version = "^1" }
            "#,
        )
        .unwrap();
        let ids = |manifest: &Manifest| -> Vec<String> {
            manifest.targets().iter().map(Target::id).collect()
        };
        assert_eq!(ids(&several), ["roblox", "roblox-alloy", "luau-luwu"]);
        // the first target is the project's own
        assert_eq!(several.target().unwrap().id(), "roblox");

        let written = toml::to_string(&several).unwrap();
        assert_eq!(written.matches("[[target]]").count(), 3, "{written}");
        let reparsed: Manifest = toml::from_str(&written).unwrap();
        assert_eq!(ids(&reparsed), ids(&several));
        assert_eq!(reparsed.targets()[1].main.as_deref(), Some("out/init.luau"));
        assert_eq!(reparsed.dependencies.len(), 1);

        // no target stays absent
        let none: Manifest = toml::from_str("").unwrap();
        assert!(none.target().is_none() && none.targets().is_empty());
        assert!(!toml::to_string(&none).unwrap().contains("target"));
    }

    #[test]
    fn two_targets_with_one_id_are_refused() {
        // `shared` is `roblox`, so these two are one ID
        let error = toml::from_str::<Manifest>(
            "[[target]]\nenvironment = \"roblox\"\n\n[[target]]\nenvironment = \"shared\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("two targets have the ID 'roblox'"),
            "{error}"
        );

        // one environment in two languages is two IDs
        assert!(
            toml::from_str::<Manifest>(
                "[[target]]\nenvironment = \"roblox\"\n\n\
                 [[target]]\nenvironment = \"roblox\"\nlanguage = \"loom\"\n",
            )
            .is_ok()
        );
        // a bad value inside an array table still names itself
        let error = toml::from_str::<Manifest>("[[target]]\nenvironment = \"mars\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("mars"), "{error}");
    }

    #[test]
    fn target_ids_name_the_language_unless_it_is_luau() {
        let id = |environment: &str, language: &str| {
            toml::from_str::<Manifest>(&format!(
                "[target]\nenvironment = \"{environment}\"\nlanguage = \"{language}\"\n"
            ))
            .unwrap()
            .target()
            .unwrap()
            .id()
        };
        assert_eq!(id("roblox", "luau"), "roblox");
        assert_eq!(id("shared", "luau"), "roblox");
        assert_eq!(id("lune", "luau"), "lune");
        assert_eq!(id("roblox", "alloy"), "roblox-alloy");
        assert_eq!(id("roblox", "loom"), "roblox-loom");
        assert_eq!(id("luau", "luwu"), "luau-luwu");
    }

    #[test]
    fn seal_and_zune_install_to_their_own_folders() {
        let manifest: Manifest = toml::from_str(
            "[config]\nzune-packages-out = \"deps\"\n\n[target]\nenvironment = \"seal\"\n",
        )
        .unwrap();
        assert_eq!(manifest.target().unwrap().environment, Environment::Seal);
        assert_eq!(
            manifest.packages_out(Environment::Seal),
            std::path::PathBuf::from("packages").join("seal")
        );
        assert_eq!(
            manifest.packages_out(Environment::Zune),
            std::path::PathBuf::from("deps")
        );
        for environment in Environment::ALL {
            assert_eq!(
                Environment::from_embr(environment.as_str()).unwrap(),
                environment
            );
        }
    }

    #[test]
    fn fit_rules() {
        use Environment::*;
        // the same environment, portable Luau, and wally's realm rule fit
        assert!(Lune.accepts(Lune));
        assert!(Roblox.accepts(Luau));
        assert!(Seal.accepts(Luau));
        assert!(Server.accepts(Roblox));
        // nothing else does
        // Lune runs Roblox code through its DOM
        assert!(Lune.accepts(Roblox));
        assert!(!Lune.accepts(Server));
        assert!(!Lute.accepts(Roblox));
        assert!(!Roblox.accepts(Server));
        assert!(!Roblox.accepts(Lune));
        assert!(!Luau.accepts(Lune));
        assert!(!Seal.accepts(Lute));

        let all = [
            Language::Luau,
            Language::Alloy,
            Language::Loom,
            Language::Luwu,
        ];
        for project in all {
            for package in all {
                // its own language, or plain Luau, the base every language reads
                assert_eq!(
                    project.accepts(package),
                    package == project || package == Language::Luau,
                    "{project} taking {package}"
                );
            }
        }
    }
}
