pub mod edit;

use crate::error::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

pub const MANIFEST_FILE: &str = "lpm.toml";

/// Key under [indices] used for dependencies that name no index.
pub const DEFAULT_INDEX_NAME: &str = "default";

/// lpm's own package index, used for dependencies that name no index when the
/// project doesn't define a `default` one. Pesde-format entries whose
/// `download` URLs point at the registry CDN; written only by the lpm API as
/// part of publishing, read by the CLI like any other git index.
pub const DEFAULT_INDEX_URL: &str = "https://github.com/luaupm/index";

#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub package: Package,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(default, skip_serializing_if = "Config::is_default")]
    pub config: Config,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub indices: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, Dependency>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Tool>,
    /// Shell commands runnable with `lpm run <name>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scripts: BTreeMap<String, String>,
    /// What `lpm studio open` opens in Roblox Studio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub studio: Option<Studio>,
}

/// Per-environment install locations; each defaults to "packages/<env>".
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Config {
    #[serde(
        rename = "shared-packages-out",
        skip_serializing_if = "Option::is_none"
    )]
    pub shared_packages_out: Option<String>,
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
        self.shared_packages_out.is_none()
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Files or directories (not globs) that go into the published archive;
    /// everything else is skipped. Empty means "everything sensible".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Paths dropped from the published archive after `include` applies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Target {
    pub environment: Environment,
    /// Entry point of the package, e.g. "src/init.luau".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
}

/// Where a package's Luau code runs. Output folders under packages/ use the
/// serialized (lowercase) form.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    /// Luau code that must be run in Roblox
    Shared,
    /// Roblox server-side code only
    Server,
    /// Requires the Lune runtime
    Lune,
    /// Standalone Luau, runnable with the luau CLI
    Luau,
    /// Requires the Lute runtime
    Lute,
}

impl Environment {
    pub const ALL: [Environment; 5] = [
        Environment::Shared,
        Environment::Server,
        Environment::Lune,
        Environment::Luau,
        Environment::Lute,
    ];

    pub fn dir_name(self) -> &'static str {
        match self {
            Environment::Shared => "shared",
            Environment::Server => "server",
            Environment::Lune => "lune",
            Environment::Luau => "luau",
            Environment::Lute => "lute",
        }
    }

    /// Translates a pesde `target.environment` value.
    pub fn from_pesde(environment: &str) -> Result<Self, Error> {
        match environment {
            "roblox" => Ok(Environment::Shared),
            "roblox_server" => Ok(Environment::Server),
            "lune" => Ok(Environment::Lune),
            "luau" => Ok(Environment::Luau),
            "lute" => Ok(Environment::Lute),
            other => Err(Error::UnsupportedEnvironment(other.to_string())),
        }
    }

    /// Translates a wally `realm` value.
    pub fn from_wally_realm(realm: &str) -> Result<Self, Error> {
        match realm {
            "shared" => Ok(Environment::Shared),
            "server" => Ok(Environment::Server),
            other => Err(Error::UnsupportedEnvironment(other.to_string())),
        }
    }

    /// Parses lpm's own environment names ("shared", "lune", ...).
    pub fn from_lpm(environment: &str) -> Result<Self, Error> {
        match environment {
            "shared" => Ok(Environment::Shared),
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

/// The [studio] table: either a published place (both IDs) or a local place
/// file.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Studio {
    /// Universe (experience) ID the place belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub universe: Option<u64>,
    /// Place ID inside the universe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<u64>,
    /// Path to a .rbxl/.rbxlx place file, relative to lpm.toml.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Anything else written under [studio]. The table is hand-edited often
    /// and a typo'd key would otherwise read as "unconfigured", so `target()`
    /// reports these — but only there, keeping a [studio] typo from failing
    /// unrelated commands the way `deny_unknown_fields` would.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// A validated [studio] table: the one thing `lpm studio open` should open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StudioTarget {
    /// A published place, opened through the roblox-studio: protocol.
    Place { universe: u64, place: u64 },
    /// A local place file, opened through the OS file association.
    File(String),
}

impl Studio {
    /// Reduces the table to what to open. Everything that can be wrong with a
    /// hand-written [studio] is caught here, before any launch attempt.
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
pub struct Dependency {
    /// Package identifier in "scope/name" form.
    pub name: String,
    /// Semver requirement; "^" alone means "latest".
    pub version: String,
    /// Key into [indices]; None means the default luaupm index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
}

/// A GitHub-released binary tool, written in lpm.toml as the single string
/// "owner/repo@version" under [tools] (key = alias).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// GitHub repository in "owner/repo" form.
    pub repository: String,
    /// Exact release version, without a leading 'v'.
    pub version: String,
}

impl Tool {
    /// Parses an "owner/repo@version" tool spec.
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

    /// Splits an "owner/repo" GitHub repository name. Unlike index package
    /// names (see `split_package_name`), GitHub owners and repos may contain
    /// uppercase letters and dots (e.g. "JohnnyMorganz/StyLua"), so only the
    /// shape is validated: exactly one '/', both halves non-empty.
    pub fn split_repository(repository: &str) -> Result<(&str, &str), Error> {
        match repository.split_once('/') {
            Some((owner, repo)) if !owner.is_empty() && !repo.is_empty() && !repo.contains('/') => {
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
    /// Loads the manifest from the current directory.
    pub fn load() -> Result<Self, Error> {
        Self::load_from(Path::new(MANIFEST_FILE))
    }

    pub fn load_from(path: &Path) -> Result<Self, Error> {
        if !path.exists() {
            return Err(Error::ManifestMissing);
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// Folder an environment's packages (and their link files) install to.
    pub fn packages_out(&self, environment: Environment) -> std::path::PathBuf {
        let configured = match environment {
            Environment::Shared => &self.config.shared_packages_out,
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

    /// Resolves a dependency's `index` key to an index URL. No key means the
    /// `default` entry under [indices] when the project defines one, and lpm's
    /// own index otherwise.
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

    /// The command a `[scripts]` entry runs. Editing the manifest goes
    /// through `edit::ManifestDoc`; this is the read side, used by `lpm run`.
    pub fn script(&self, name: &str) -> Result<&str, Error> {
        self.scripts
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| Error::ScriptMissing(name.to_string()))
    }
}

/// Shape of a GitHub username: 1-39 characters, alphanumeric or dashes, with
/// no leading, trailing, or consecutive dash. `[package] authors` must pass
/// this — the registry appends authors to the scope's owner list on publish
/// (co-ownership) and 400s anything that isn't a username. Shape only; whether
/// the account exists is not checked anywhere, so typos still bite.
pub fn is_github_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 39
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// Splits a "scope/name" package identifier. Wally allows dashes, so parts
/// accept lowercase letters, digits, and dashes/underscores.
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

/// Parses a manifest version requirement. A bare "^" (or "*") means "latest".
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

        assert_eq!(manifest.package.name, "scope/name");
        assert_eq!(
            manifest.target.as_ref().unwrap().environment,
            Environment::Lune
        );
        assert_eq!(manifest.dependencies["Chief"].index, None);
        assert_eq!(
            manifest.dependencies["Other"].index.as_deref(),
            Some("wally")
        );
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

        // No `default` key: bare dependencies fall back to lpm's own index.
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
            shared-packages-out = "src/ReplicatedStorage/Packages"
            "#,
        )
        .unwrap();

        assert_eq!(
            manifest.packages_out(Environment::Shared),
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
    }

    #[test]
    fn translates_environments() {
        assert_eq!(
            Environment::from_pesde("roblox").unwrap(),
            Environment::Shared
        );
        assert_eq!(
            Environment::from_pesde("roblox_server").unwrap(),
            Environment::Server
        );
        assert_eq!(Environment::from_pesde("lune").unwrap(), Environment::Lune);
        assert!(Environment::from_pesde("nonsense").is_err());
        assert_eq!(
            Environment::from_wally_realm("shared").unwrap(),
            Environment::Shared
        );
        assert_eq!(
            Environment::from_wally_realm("server").unwrap(),
            Environment::Server
        );
        assert!(Environment::from_wally_realm("lune").is_err());
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
    fn include_and_exclude_round_trip() {
        let manifest: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"
            include = ["src", "lpm.toml", "README.md"]
            exclude = ["src/tests"]
            "#,
        )
        .unwrap();

        assert_eq!(manifest.package.include, ["src", "lpm.toml", "README.md"]);
        assert_eq!(manifest.package.exclude, ["src/tests"]);

        let serialized = toml::to_string(&manifest).unwrap();
        let parsed: Manifest = toml::from_str(&serialized).unwrap();
        assert_eq!(parsed.package.include, manifest.package.include);
        assert_eq!(parsed.package.exclude, manifest.package.exclude);

        // Absent lists stay absent on write.
        let bare: Manifest = toml::from_str(
            r#"
            [package]
            name = "scope/name"
            version = "0.1.0"
            "#,
        )
        .unwrap();
        assert!(bare.package.include.is_empty());
        let serialized = toml::to_string(&bare).unwrap();
        assert!(!serialized.contains("include"));
        assert!(!serialized.contains("exclude"));
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

        // Both forms at once is ambiguous, even when the ID half is partial.
        assert!(matches!(
            studio(Some(1), Some(2), Some("a.rbxl")).target(),
            Err(Error::StudioConflict)
        ));
        assert!(matches!(
            studio(Some(1), None, Some("a.rbxl")).target(),
            Err(Error::StudioConflict)
        ));

        // One ID without the other.
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

        // Nothing at all, zero IDs, and a blank file path.
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

        // Surrounding whitespace in the path is tolerated.
        assert_eq!(
            studio(None, None, Some(" place.rbxl ")).target().unwrap(),
            StudioTarget::File("place.rbxl".to_string())
        );
    }

    #[test]
    fn studio_flags_unknown_keys_without_failing_the_manifest() {
        // A typo'd key must not break `Manifest::load` — that would take
        // `install`/`add`/`run` down with it — but `target()` names it.
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
    fn parses_version_requirements() {
        let latest = parse_version_req("^").unwrap();
        assert!(latest.matches(&semver::Version::new(99, 0, 0)));
        let caret = parse_version_req("^1.2").unwrap();
        assert!(caret.matches(&semver::Version::new(1, 9, 0)));
        assert!(!caret.matches(&semver::Version::new(2, 0, 0)));
        assert!(parse_version_req("not a version").is_err());
    }
}
