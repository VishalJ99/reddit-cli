use thiserror::Error;

#[derive(Debug, Error)]
pub enum RdtError {
    #[error(
        "reddit edge block detected; try `rdt auth check`, configure RDT_COOKIE, or force `--rss` degraded mode"
    )]
    EdgeBlocked,
    #[error("no reddit_session cookie configured; set RDT_COOKIE or cookie in config.toml")]
    CookieMissing,
    #[error("HTTP {status} from {url}: {body}")]
    HttpStatus {
        status: u16,
        url: String,
        body: String,
    },
    #[error("rate limited by Reddit after retrying {url}")]
    RateLimited { url: String },
    #[error("refusing non-Reddit URL: {0}")]
    NonRedditUrl(String),
    #[error("{0}")]
    Message(String),
}

pub type RdtResult<T> = Result<T, RdtError>;
