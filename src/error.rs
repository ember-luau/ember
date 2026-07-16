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
    NoReleases(&'static str),

    #[error("Release v{version} has no asset named {asset} for this platform")]
    MissingAsset {
        version: semver::Version,
        asset: String,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Prompt(#[from] inquire::InquireError),

    #[error(transparent)]
    TomlSerialize(#[from] toml::ser::Error),

    #[error(transparent)]
    Semver(#[from] semver::Error),

    #[error(transparent)]
    Http(Box<ureq::Error>),
}

// Boxed by hand because ureq::Error is large enough that carrying it inline
// would bloat every Result<_, Error>.
impl From<ureq::Error> for Error {
    fn from(error: ureq::Error) -> Self {
        Error::Http(Box::new(error))
    }
}
