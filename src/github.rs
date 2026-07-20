const BASE_URL: &str = "https://api.github.com";
const JSON_HEADER_TYPE: &str = "application/vnd.github.v3+json";

// TODO:
// Add logic for checking tools on github via author/name
// Implement a function to install package release versions and return them so the tool command can link and do whatever else
// Implement version checking and updating functionality

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("unrecognized access token format - must begin with `ghp_` or `gho_`.")]
    UnrecognizedAccessToken,
    #[error("no latest release was found for tool '{0}'")]
    LatestReleaseNotFound(Box<ToolId>),
    #[error("no release was found for tool '{0}'")]
    ReleaseNotFound(Box<ToolSpec>),
    #[error("other error: {0}")]
    Other(String),
}

pub struct GithubAPI {
    auth: bool
}

impl GithubAPI {

}