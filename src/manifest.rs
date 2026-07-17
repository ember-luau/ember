use crate::error::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

pub const MANIFEST_FILE: &str = "lpm.toml";

/// The index searched when a dependency does not name one.
pub const DEFAULT_INDEX_URL: &str = "https://github.com/luaupm/index";
pub const DEFAULT_INDEX_NAME: &str = "default";

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
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Target {
    pub environment: Environment,
    /// Entry point of the package, e.g. "init.luau".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
}

/// Where a package's Luau code runs. Directory names under .lpm/packages/
/// use the serialized (lowercase) form.
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

    /// Resolves a dependency's `index` key to an index URL.
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
            main = "init.luau"

            [indices]
            wally = "https://github.com/UpliftGames/wally-index"

            [dependencies]
            Chief = { name = "chief/core", version = "^" }
            Other = { name = "user/other_package", version = "^", index = "wally" }
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
    fn default_index_key_overrides_builtin_url() {
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
    fn parses_version_requirements() {
        let latest = parse_version_req("^").unwrap();
        assert!(latest.matches(&semver::Version::new(99, 0, 0)));
        let caret = parse_version_req("^1.2").unwrap();
        assert!(caret.matches(&semver::Version::new(1, 9, 0)));
        assert!(!caret.matches(&semver::Version::new(2, 0, 0)));
        assert!(parse_version_req("not a version").is_err());
    }
}
