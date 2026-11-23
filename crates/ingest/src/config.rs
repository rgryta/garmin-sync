//! Ingester configuration from the environment.

use std::time::Duration;

use garmin_core::config::Secret;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{name} is not a number: {value:?}")]
    NotANumber { name: &'static str, value: String },
}

#[derive(Debug, Clone)]
pub struct IngestSettings {
    pub dsn: Secret,
    pub account: String,
    pub poll_interval: Duration,
    pub auth_retry: Duration,
    pub backfill_budget: i64,
    pub session_from: Option<String>,
}

fn secs(name: &'static str, default: u64) -> Result<Duration, ConfigError> {
    match std::env::var(name) {
        Ok(raw) => raw
            .trim()
            .parse()
            .map(Duration::from_secs)
            .map_err(|_| ConfigError::NotANumber { name, value: raw }),
        Err(_) => Ok(Duration::from_secs(default)),
    }
}

impl IngestSettings {
    pub fn from_env() -> Result<Self, ConfigError> {
        let dsn = std::env::var("GARMIN_DSN").map_err(|_| ConfigError::Missing("GARMIN_DSN"))?;
        let account =
            std::env::var("GARMIN_ACCOUNT").map_err(|_| ConfigError::Missing("GARMIN_ACCOUNT"))?;
        let backfill_budget = match std::env::var("GARMIN_BACKFILL_BUDGET") {
            Ok(raw) => raw.trim().parse().map_err(|_| ConfigError::NotANumber {
                name: "GARMIN_BACKFILL_BUDGET",
                value: raw,
            })?,
            Err(_) => 2000,
        };
        Ok(Self {
            dsn: Secret::new(dsn),
            account,
            poll_interval: secs("GARMIN_POLL_INTERVAL", 15 * 60)?,
            auth_retry: secs("GARMIN_AUTH_RETRY", 6 * 60 * 60)?,
            backfill_budget,
            session_from: std::env::var("GARMIN_SESSION_FROM")
                .ok()
                .filter(|s| !s.is_empty()),
        })
    }
}
