use thiserror::Error;

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("request failed: {0}")]
    Request(Box<ureq::Error>),

    #[error("failed to read response body: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to parse response as JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("resource not found (404)")]
    NotFound,

    #[error("too many redirects while fetching {url}")]
    TooManyRedirects { url: String },

    #[error("redirect from {url} had no Location header")]
    RedirectMissingLocation { url: String },

    #[error("rate limited")]
    RateLimited,

    /** ureq names the header and its value in its own error, and the
    value is a token. */
    #[error(
        "a request header holds a character that HTTP does not allow; a token in EMBER_TOKEN, GITHUB_TOKEN or the stored login has a space or a line break in it"
    )]
    BadHeader,

    #[error("{url} is not in a cache, and --offline lets embr download nothing")]
    Offline { url: String },

    #[error("the response is larger than {} MiB, the most embr downloads", limit >> 20)]
    BodyTooLarge { limit: u64 },
}

/* Boxed because ureq::Error is big enough to bloat every Result<_, Error>
if carried inline. */
impl From<ureq::Error> for HttpError {
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::Status(404, _) => HttpError::NotFound,
            ureq::Error::Status(403 | 429, _) => HttpError::RateLimited,
            ureq::Error::Transport(transport) if transport.kind() == ureq::ErrorKind::BadHeader => {
                HttpError::BadHeader
            }
            // the name lookup is where `--offline` stops a request, see `net::offline`
            ureq::Error::Transport(transport)
                if crate::net::offline::is_enabled()
                    && transport.kind() == ureq::ErrorKind::Dns =>
            {
                HttpError::Offline {
                    url: transport
                        .url()
                        .map(|url| url.to_string())
                        .unwrap_or_default(),
                }
            }
            other => HttpError::Request(Box::new(other)),
        }
    }
}
