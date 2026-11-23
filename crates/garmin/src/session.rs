//! The session: the persistent rolling refresh token and the public client id the
//! ingester re-authenticates with, plus the current access token and its expiry. Stored
//! verbatim in `ingest_session.session`; its `Debug` is redacted so a stray log line
//! cannot print a credential.
//!
//! The refresh token rolls on every refresh (Garmin returns a new one), so persisting
//! the latest is what keeps the ingester autonomous. `client_id` is a public constant,
//! not a secret. The OAuth1 token/secret are an optional longer-lived fallback.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    /// The rolling OAuth2 refresh token (the credential to seed and persist).
    pub refresh_token: String,
    /// The public client id, e.g. `GARMIN_CONNECT_MOBILE_ANDROID_DI_2025Q2`.
    pub client_id: String,
    /// The current access token (the Bearer), or empty before the first refresh.
    #[serde(default)]
    pub access_token: String,
    /// When the access token expires; a past/absent value forces a refresh.
    #[serde(default)]
    pub access_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub oauth1_token: Option<String>,
    #[serde(default)]
    pub oauth1_secret: Option<String>,
    #[serde(default)]
    pub garmin_user_id: Option<String>,
}

impl Session {
    /// True when the access token is missing or within 60s of expiry.
    pub fn needs_refresh(&self, now: DateTime<Utc>) -> bool {
        self.access_token.is_empty()
            || self
                .access_expires_at
                .is_none_or(|exp| exp <= now + chrono::TimeDelta::seconds(60))
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("refresh_token", &"**********")
            .field("client_id", &self.client_id)
            .field("access_token", &"**********")
            .field("access_expires_at", &self.access_expires_at)
            .field(
                "oauth1_token",
                &self.oauth1_token.as_ref().map(|_| "**********"),
            )
            .field("garmin_user_id", &self.garmin_user_id)
            .finish()
    }
}
