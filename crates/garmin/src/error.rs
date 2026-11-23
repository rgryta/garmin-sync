//! Client errors. A rejected credential is distinct from a transport failure so the
//! runner can tell "re-seed me" from "try again later".

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The refresh token is no longer accepted (Garmin returned 400/401 on the refresh).
    /// Only a human re-seed fixes this.
    #[error("the Garmin session was rejected")]
    Rejected,
    /// Rate-limited; back off and retry.
    #[error("rate limited by Garmin{}", .retry_after.map(|s| format!(" (retry after {s}s)")).unwrap_or_default())]
    Throttled { retry_after: Option<u64> },
    /// A non-success HTTP status from the API.
    #[error("Garmin returned HTTP {0}")]
    Http(u16),
    /// A response that did not match the shape the parser expects.
    #[error("unexpected response shape: {0}")]
    Shape(String),
    /// A FIT file that could not be parsed.
    #[error("could not parse the FIT file: {0}")]
    Fit(String),
    /// A request the allow-list does not name — a bug, never reached in production.
    #[error("refused a request outside the read allow-list: {0}")]
    NotAllowed(String),
    /// Network or TLS failure.
    #[error("transport error: {0}")]
    Transport(String),
}
