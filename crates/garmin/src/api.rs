//! The Garmin Connect private-API client. Every request's path is checked against a
//! read allow-list of services; the only non-GET is the token refresh. Auth is a Bearer
//! JWT the client refreshes itself from the rolling refresh token (`diauth .../oauth/
//! token`, grant_type=refresh_token) — no SSO, no 2FA, no browser fingerprint (Cloudflare
//! only guards the login page, not the API).

use chrono::{TimeDelta, Utc};
use serde_json::Value;

use crate::error::ClientError;
use crate::session::Session;

pub const DEFAULT_API: &str = "https://connectapi.garmin.com/";
pub const DEFAULT_DIAUTH: &str = "https://diauth.garmin.com/";

/// The app user-agent; some Garmin endpoints are picky about a missing one.
const UA: &str = "com.garmin.android.apps.connectmobile/5.31.1; networking-core/5.1.1; Android/36";

/// Read services the client may call (the path's first segment). The refresh POST goes to
/// diauth and is handled separately. Nothing here writes to the account.
pub const ALLOWED_SERVICES: &[&str] = &[
    "activity-service",
    "activitylist-service",
    "download-service",
    "usersummary-service",
    "wellness-service",
    "sleep-service",
    "hrv-service",
    "metrics-service",
    "biometric-service",
    "weight-service",
    "bloodpressure-service",
    "fitnessstats-service",
    "device-service",
    "userprofile-service",
    "personalrecord-service",
    "badge-service",
    "workout-service",
    "gear-service",
];

/// Daily kinds and their path templates. `{date}` is an ISO date; a few take a range as
/// `{from}/{to}`. Confirmed/extended against the live API at deploy time.
pub const DAILY_PATHS: &[(&str, &str)] = &[
    (
        "summary",
        "usersummary-service/usersummary/daily/{displayName}?calendarDate={date}",
    ),
    (
        "sleep",
        "sleep-service/sleep/dailySleepData?nonSleepBufferMinutes=60&date={date}",
    ),
    ("hrv", "hrv-service/hrv/{date}"),
    (
        "heart_rate",
        "wellness-service/wellness/dailyHeartRate/{displayName}?date={date}",
    ),
    ("stress", "wellness-service/wellness/dailyStress/{date}"),
    (
        "steps",
        "wellness-service/wellness/dailySummaryChart/{displayName}?date={date}",
    ),
    (
        "body_battery",
        "wellness-service/wellness/bodyBattery/events/{date}",
    ),
    (
        "respiration",
        "wellness-service/wellness/daily/respiration/{date}",
    ),
    ("spo2", "wellness-service/wellness/daily/spo2/{date}"),
    (
        "training_readiness",
        "metrics-service/metrics/trainingreadiness/{date}",
    ),
    ("vo2max", "metrics-service/metrics/maxmet/latest/{date}"),
    ("weight", "weight-service/weight/dayview/{date}"),
];

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    api: String,
    diauth: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub more: bool,
}

impl Client {
    pub fn new() -> Self {
        Self::with_bases(DEFAULT_API, DEFAULT_DIAUTH)
    }

    pub fn with_bases(api: impl Into<String>, diauth: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(UA)
            .build()
            .expect("a default reqwest client builds");
        let norm = |s: String| if s.ends_with('/') { s } else { format!("{s}/") };
        Self {
            http,
            api: norm(api.into()),
            diauth: norm(diauth.into()),
        }
    }

    /// Re-mint the access token from the rolling refresh token. Returns the rotated
    /// session (new access token + a new refresh token). A 400/401 means the refresh
    /// token is dead -> `Rejected`.
    pub async fn refresh(&self, s: &Session) -> Result<Session, ClientError> {
        let url = format!("{}di-oauth2-service/oauth/token", self.diauth);
        let resp = self
            .http
            .post(&url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", s.client_id.as_str()),
                ("refresh_token", s.refresh_token.as_str()),
            ])
            .send()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED
        {
            return Err(ClientError::Rejected);
        }
        if !status.is_success() {
            return Err(ClientError::Http(status.as_u16()));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ClientError::Shape(e.to_string()))?;
        let access = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::Shape("refresh returned no access_token".into()))?
            .to_owned();
        let expires_in = body
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(3600);
        let refresh = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| s.refresh_token.clone());
        Ok(Session {
            access_token: access,
            access_expires_at: Some(Utc::now() + TimeDelta::seconds(expires_in)),
            refresh_token: refresh,
            client_id: s.client_id.clone(),
            oauth1_token: s.oauth1_token.clone(),
            oauth1_secret: s.oauth1_secret.clone(),
            garmin_user_id: s.garmin_user_id.clone(),
        })
    }

    fn check_allowed(&self, path: &str) -> Result<(), ClientError> {
        let service = path.trim_start_matches('/').split('/').next().unwrap_or("");
        if ALLOWED_SERVICES.contains(&service) {
            Ok(())
        } else {
            Err(ClientError::NotAllowed(path.to_owned()))
        }
    }

    async fn send(&self, s: &Session, path: &str) -> Result<reqwest::Response, ClientError> {
        self.check_allowed(path)?;
        let url = format!("{}{}", self.api, path.trim_start_matches('/'));
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&s.access_token)
            .send()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClientError::Rejected);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());
            return Err(ClientError::Throttled { retry_after });
        }
        if !status.is_success() {
            return Err(ClientError::Http(status.as_u16()));
        }
        Ok(resp)
    }

    /// GET a JSON endpoint.
    pub async fn get_json(&self, s: &Session, path: &str) -> Result<Value, ClientError> {
        self.send(s, path)
            .await?
            .json()
            .await
            .map_err(|e| ClientError::Shape(e.to_string()))
    }

    /// GET a binary endpoint (the original activity file).
    pub async fn get_bytes(&self, s: &Session, path: &str) -> Result<Vec<u8>, ClientError> {
        let bytes = self
            .send(s, path)
            .await?
            .bytes()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(bytes.to_vec())
    }

    /// One page of the activity list, newest first.
    pub async fn activities(
        &self,
        s: &Session,
        start: i64,
        limit: i64,
    ) -> Result<Vec<Value>, ClientError> {
        let v = self
            .get_json(
                s,
                &format!(
                    "activitylist-service/activities/search/activities?start={start}&limit={limit}"
                ),
            )
            .await?;
        v.as_array()
            .cloned()
            .ok_or_else(|| ClientError::Shape("activity list is not an array".into()))
    }

    pub async fn activity(&self, s: &Session, id: i64) -> Result<Value, ClientError> {
        self.get_json(s, &format!("activity-service/activity/{id}"))
            .await
    }
    pub async fn activity_details(&self, s: &Session, id: i64) -> Result<Value, ClientError> {
        self.get_json(s, &format!("activity-service/activity/{id}/details"))
            .await
    }
    pub async fn activity_laps(&self, s: &Session, id: i64) -> Result<Value, ClientError> {
        self.get_json(s, &format!("activity-service/activity/{id}/splits"))
            .await
    }
    pub async fn activity_weather(&self, s: &Session, id: i64) -> Result<Value, ClientError> {
        self.get_json(s, &format!("activity-service/activity/{id}/weather"))
            .await
    }

    /// The original uploaded file (FIT) for an activity.
    pub async fn activity_file(&self, s: &Session, id: i64) -> Result<Vec<u8>, ClientError> {
        self.get_bytes(s, &format!("download-service/files/activity/{id}"))
            .await
    }

    /// A daily metric for one date, by kind (see [`DAILY_PATHS`]).
    pub async fn daily(&self, s: &Session, kind: &str, date: &str) -> Result<Value, ClientError> {
        let template = DAILY_PATHS
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, p)| *p)
            .ok_or_else(|| ClientError::NotAllowed(format!("daily kind {kind}")))?;
        let mut path = template.replace("{date}", date);
        if path.contains("{displayName}") {
            let dn = s.garmin_user_id.as_deref().ok_or_else(|| {
                ClientError::Shape(format!(
                    "daily kind {kind} needs a displayName but the session has none"
                ))
            })?;
            path = path.replace("{displayName}", dn);
        }
        self.get_json(s, &path).await
    }

    pub async fn profile(&self, s: &Session) -> Result<Value, ClientError> {
        self.get_json(s, "userprofile-service/socialProfile").await
    }

    /// The account's `displayName` (a GUID), which some daily paths require.
    pub async fn user_id(&self, s: &Session) -> Result<String, ClientError> {
        self.profile(s)
            .await?
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ClientError::Shape("socialProfile has no displayName".into()))
    }
    pub async fn devices(&self, s: &Session) -> Result<Value, ClientError> {
        self.get_json(s, "device-service/deviceregistration/devices")
            .await
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}
