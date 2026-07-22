use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("An lpm.toml manifest already exists in this directory")]
    ManifestExists,

    #[error("Could not determine your home directory")]
    NoHomeDir,

    #[error("lpm is not installed ({} does not exist)", .0.display())]
    NotInstalled(PathBuf),

    #[error("No releases have been published for {0} yet")]
    NoReleases(String),

    #[error("Release {1} for repository {0} doesn't exist")]
    NoSuchRelease(String, String),

    #[error("Release v{version} has no asset named {asset} for this platform")]
    MissingAsset {
        version: semver::Version,
        asset: String,
    },

    #[error("No lpm.toml manifest found in the current directory")]
    ManifestMissing,

    #[error("No tool exists with name '{0}'")]
    ToolMissing(String),

    #[error("Invalid tool spec '{0}': expected 'owner/repo@version'")]
    InvalidToolSpec(String),

    #[error("No release asset of {tool}@{version} matches this platform")]
    NoMatchingAsset { tool: String, version: String },

    #[error("No executable found in the downloaded release of {0}")]
    NoExecutableInAsset(String),

    #[error("Invalid package name '{0}': expected 'scope/name' (lowercase)")]
    InvalidPackageName(String),

    #[error("Package {name} was not found in index {index}")]
    PackageNotFound { name: String, index: String },

    #[error("No version of {name} matches requirement '{req}'")]
    NoMatchingVersion { name: String, req: String },

    #[error("Index '{0}' is not defined under [indices] in lpm.toml")]
    UnknownIndex(String),

    #[error("Unsupported environment '{0}'")]
    UnsupportedEnvironment(String),

    #[error("Could not determine the environment of {0}")]
    UnknownPackageEnvironment(String),

    #[error("Dependency conflict: {name} is required as both '{first}' and '{second}'")]
    DependencyConflict {
        name: String,
        first: String,
        second: String,
    },

    #[error("Failed to fetch index {url}: {reason}")]
    IndexFetch { url: String, reason: String },

    #[error("lpm.lock is missing; run `lpm install` without --locked to create it")]
    LockfileMissing,

    #[error("{0} is not implemented yet")]
    Unimplemented(&'static str),

    #[error("Invalid lpm.toml: {0}")]
    ManifestInvalid(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    TomlDeserialize(#[from] toml::de::Error),

    #[error(transparent)]
    TomlEdit(#[from] toml_edit::TomlError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),

    #[error(transparent)]
    Prompt(#[from] inquire::InquireError),

    #[error(transparent)]
    TomlSerialize(#[from] toml::ser::Error),

    #[error(transparent)]
    Semver(#[from] semver::Error),

    // Uses the http errors made
    #[error(transparent)]
    Http(#[from] crate::http::error::HttpError),
}
