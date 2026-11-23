//! The ingest loops. A pass re-authenticates (refreshing the Bearer only when needed and
//! persisting the rolled refresh token first), syncs new activities with their FIT files
//! and per-second samples, and refreshes a trailing window of each daily metric. Backfill
//! walks the whole history under a daily request budget.

use chrono::{NaiveDate, TimeDelta, Utc};
use garmin_client::{ClientError, Session};
use garmin_core::store::{activities, daily, raw, samples, session as session_store, sync};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPool;

use crate::fit;
use crate::normalize;
use crate::source::Source;

pub const LOOP: &str = "sync";
/// The daily metric kinds backfilled and polled (keys into the client's DAILY_PATHS).
pub const DAILY_KINDS: &[&str] = &[
    "summary",
    "sleep",
    "hrv",
    "heart_rate",
    "stress",
    "steps",
    "body_battery",
    "respiration",
    "spo2",
    "training_readiness",
    "vo2max",
    "weight",
];
/// How many trailing days each poll re-fetches (Garmin recomputes recent days).
const POLL_TRAILING_DAYS: i64 = 3;
/// How far back backfill reaches when no floor is configured.
const DEFAULT_FLOOR_YEARS: i64 = 20;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("the Garmin session was rejected")]
    Rejected,
    #[error("rate limited")]
    Throttled { retry_after: Option<u64> },
    #[error("client error: {0}")]
    Client(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl From<ClientError> for RunError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Rejected => RunError::Rejected,
            ClientError::Throttled { retry_after } => RunError::Throttled { retry_after },
            other => RunError::Client(other.to_string()),
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct PassReport {
    pub activities: u64,
    pub samples: u64,
    pub daily: u64,
}

pub struct Runner<S> {
    pool: PgPool,
    account: String,
    budget: i64,
    source: S,
    session: Session,
}

impl<S: Source> Runner<S> {
    pub fn new(pool: PgPool, account: String, budget: i64, source: S, session: Session) -> Self {
        Self {
            pool,
            account,
            budget,
            source,
            session,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Refresh the Bearer only when it is missing or near expiry, persisting the rolled
    /// session (new refresh token) **before** any further request.
    async fn authenticate(&mut self) -> Result<(), RunError> {
        if self.session.needs_refresh(Utc::now()) {
            let rotated = self.source.refresh(&self.session).await?;
            let mut conn = self.pool.acquire().await?;
            session_store::save(&mut conn, &self.account, &json!(rotated)).await?;
            self.session = rotated;
        }
        // Some daily paths need the account's displayName; resolve it once and persist.
        if self.session.garmin_user_id.is_none() {
            let id = self.source.user_id(&self.session).await?;
            self.session.garmin_user_id = Some(id);
            let mut conn = self.pool.acquire().await?;
            session_store::save(&mut conn, &self.account, &json!(self.session)).await?;
        }
        Ok(())
    }

    /// Spend one request's budget; false when the day's ceiling is reached.
    async fn budget(&self) -> Result<bool, RunError> {
        Ok(sync::take_budget(&self.pool, LOOP, 1, self.budget).await?)
    }

    /// Upsert an activity and, if its file is not stored yet, fetch the FIT (-> blob +
    /// per-second samples) and its weather. A FIT that will not parse keeps the activity
    /// and the blob; only the samples are skipped.
    async fn ingest_activity(&mut self, summary: &Value) -> Result<u64, RunError> {
        let Ok(act) = normalize::activity(summary) else {
            return Ok(0);
        };
        let mut conn = self.pool.acquire().await?;
        activities::upsert(&mut conn, &act).await?;
        let mut sample_rows = 0u64;
        if !activities::has_file(&self.pool, act.id).await? {
            if !self.budget().await? {
                return Ok(0);
            }
            match self.source.activity_file(&self.session, act.id).await {
                Ok(bytes) => {
                    let sha = format!("{:x}", Sha256::digest(&bytes));
                    activities::save_file(&mut conn, act.id, "fit", &bytes, &sha).await?;
                    match fit::parse_samples(&bytes) {
                        Ok(samples) => {
                            sample_rows =
                                samples::insert_batch(&mut conn, act.id, &samples).await?;
                        }
                        Err(e) => {
                            tracing::warn!(activity = act.id, %e, "kept the FIT, skipped samples")
                        }
                    }
                }
                // No file for this activity (manual entry, or not downloadable): fine.
                Err(ClientError::Http(404)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(sample_rows)
    }

    /// Fetch and store one daily kind for one date.
    async fn ingest_daily(&mut self, kind: &str, date: NaiveDate) -> Result<bool, RunError> {
        if !self.budget().await? {
            return Ok(false);
        }
        match self
            .source
            .daily(&self.session, kind, &date.to_string())
            .await
        {
            Ok(v) => {
                let mut conn = self.pool.acquire().await?;
                daily::upsert(&mut conn, kind, date, &v).await?;
                raw::upsert(&mut conn, kind, &date.to_string(), &v).await?;
                Ok(true)
            }
            Err(ClientError::Http(404)) => Ok(true), // no data that day; counts as done
            Err(e) => Err(e.into()),
        }
    }

    /// One poll pass: new activities since the newest stored, and a trailing window of
    /// every daily kind.
    pub async fn pass(&mut self) -> Result<PassReport, RunError> {
        self.authenticate().await?;
        let newest = activities::newest_start(&self.pool).await?;
        let mut report = PassReport::default();
        let page = self.source.activities(&self.session, 0, 20).await?;
        for summary in &page {
            let start = normalize::activity(summary).ok().map(|a| a.start_ts);
            if let (Some(s), Some(n)) = (start, newest)
                && s <= n
            {
                continue;
            }
            report.samples += self.ingest_activity(summary).await?;
            report.activities += 1;
        }
        let today = Utc::now().date_naive();
        for kind in DAILY_KINDS {
            for back in 0..POLL_TRAILING_DAYS {
                if self
                    .ingest_daily(kind, today - TimeDelta::days(back))
                    .await?
                {
                    report.daily += 1;
                }
            }
        }
        sync::record_ok(&self.pool, LOOP).await?;
        Ok(report)
    }

    /// One backfill step under the budget. Walks the activity list older and older
    /// (cursor = next page start), then each daily kind back day by day to the floor.
    /// Returns true when everything is backfilled.
    pub async fn backfill_step(&mut self) -> Result<bool, RunError> {
        if sync::all_backfill_done(&self.pool, &all_kinds()).await? {
            return Ok(true);
        }
        self.authenticate().await?;
        // Activities first.
        let (cur, done) = sync::cursor_get(&self.pool, "activities")
            .await?
            .unwrap_or_default();
        if !done {
            if !self.budget().await? {
                return Ok(false);
            }
            let start: i64 = cur.parse().unwrap_or(0);
            let page = self.source.activities(&self.session, start, 20).await?;
            if page.is_empty() {
                sync::cursor_set(&self.pool, "activities", &start.to_string(), true).await?;
            } else {
                for summary in &page {
                    self.ingest_activity(summary).await?;
                }
                sync::cursor_set(
                    &self.pool,
                    "activities",
                    &(start + page.len() as i64).to_string(),
                    false,
                )
                .await?;
            }
            return Ok(false);
        }
        // Then each daily kind, oldest-unfilled date backwards.
        let floor =
            Utc::now().date_naive() - chrono::Months::new((DEFAULT_FLOOR_YEARS * 12) as u32);
        for kind in DAILY_KINDS {
            let (c, kdone) = sync::cursor_get(&self.pool, kind)
                .await?
                .unwrap_or_default();
            if kdone {
                continue;
            }
            let next: NaiveDate = c
                .parse()
                .unwrap_or_else(|_| Utc::now().date_naive() - TimeDelta::days(POLL_TRAILING_DAYS));
            if next < floor {
                sync::cursor_set(&self.pool, kind, &next.to_string(), true).await?;
                continue;
            }
            if !self.budget().await? {
                return Ok(false);
            }
            self.ingest_daily(kind, next).await?;
            sync::cursor_set(
                &self.pool,
                kind,
                &(next - TimeDelta::days(1)).to_string(),
                false,
            )
            .await?;
            return Ok(false);
        }
        Ok(sync::all_backfill_done(&self.pool, &all_kinds()).await?)
    }
}

fn all_kinds() -> Vec<&'static str> {
    let mut k = vec!["activities"];
    k.extend_from_slice(DAILY_KINDS);
    k
}

/// Load the session from the DB, or seed it from a file on first start.
pub async fn load_session(
    pool: &PgPool,
    account: &str,
    seed_path: Option<&str>,
) -> Result<Option<Session>, RunError> {
    if let Some(value) = session_store::load(pool, account).await? {
        return Ok(Some(serde_json::from_value(value).map_err(|e| {
            RunError::Client(format!("stored session is malformed: {e}"))
        })?));
    }
    if let Some(path) = seed_path
        && let Ok(text) = std::fs::read_to_string(path)
        && let Ok(value) = serde_json::from_str::<Value>(&text)
        && let Ok(seed) = serde_json::from_value::<Session>(value.clone())
    {
        let mut conn = pool.acquire().await?;
        session_store::save(&mut conn, account, &value).await?;
        return Ok(Some(seed));
    }
    Ok(None)
}
