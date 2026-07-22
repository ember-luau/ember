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
}

// Boxed by hand because ureq::Error is large enough that carrying it inline
// would bloat every Result<_, Error>.
impl From<ureq::Error> for HttpError {
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::Status(404, _) => HttpError::NotFound,
            ureq::Error::Status(403 | 429, _) => HttpError::RateLimited,
            other => HttpError::Request(Box::new(other)),
        }
    }
}
