//! The ingest loops. A pass re-authenticates (refreshing the Bearer only when needed and
//! persisting the rolled refresh token first), syncs new activities with their FIT files
//! and per-second samples, and refreshes a trailing window of each daily metric. Backfill
//! walks the whole history under a daily request budget.

use std::time::{Duration, Instant};

use chrono::{NaiveDate, TimeDelta, Utc};
use garmin_client::{ClientError, Session};
use garmin_core::store::{
    activities, daily, devices, raw, samples, session as session_store, sync, wellness,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPool;

use crate::fit;
use crate::normalize;
use crate::source::Source;

pub const LOOP: &str = "sync";
/// The day's wellness download (zipped FIT files), backfilled and polled like a daily kind.
pub const WELLNESS: &str = "wellness";
/// The daily kinds backfilled and polled: keys into the client's DAILY_PATHS, plus
/// [`WELLNESS`].
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
    WELLNESS,
];
/// How many trailing days each poll re-fetches (Garmin recomputes recent days).
const POLL_TRAILING_DAYS: i64 = 3;
/// How far back backfill reaches when no activity says where the history starts.
const DEFAULT_FLOOR_YEARS: i64 = 20;
/// What backfill leaves of the day's budget, so the poll still runs on a busy day.
pub const POLL_RESERVE: i64 = 250;
/// How often a pass re-fetches the trailing window of daily metrics (13 kinds x 3 days).
const DAILY_POLL_EVERY: Duration = Duration::from_secs(4 * 60 * 60);

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

/// The outcome of one `backfill_step`, so the caller knows whether to keep going now,
/// wait for the daily budget to reset, or move on to polling.
#[derive(Debug, PartialEq)]
pub enum BackfillStep {
    /// Everything is backfilled; nothing more to do.
    Done,
    /// Did a unit of work and the budget still has room — call again right away.
    Progressed,
    /// The day's request budget is spent; wait before trying again.
    BudgetSpent,
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
    daily_polled: Option<Instant>,
}

impl<S: Source> Runner<S> {
    pub fn new(pool: PgPool, account: String, budget: i64, source: S, session: Session) -> Self {
        Self {
            pool,
            account,
            budget,
            source,
            session,
            daily_polled: None,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn source(&self) -> &S {
        &self.source
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
    async fn budget(&self, ceiling: i64) -> Result<bool, RunError> {
        Ok(sync::take_budget(&self.pool, LOOP, 1, ceiling).await?)
    }

    /// The part of the day's budget backfill may spend.
    fn backfill_ceiling(&self) -> i64 {
        (self.budget - POLL_RESERVE).max(0)
    }

    /// Where backfill stops: the day before the oldest activity, since a watch records
    /// nothing before it was worn. Without activities, a fixed number of years back.
    async fn floor(&self) -> Result<NaiveDate, RunError> {
        Ok(match activities::oldest_start(&self.pool).await? {
            Some(start) => start.date_naive() - TimeDelta::days(1),
            None => {
                Utc::now().date_naive() - chrono::Months::new((DEFAULT_FLOOR_YEARS * 12) as u32)
            }
        })
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
            if !self.budget(self.budget).await? {
                return Ok(0);
            }
            match self.source.activity_file(&self.session, act.id).await {
                Ok(bytes) => {
                    let (format, stored) = fit::unwrap_download(bytes);
                    let sha = sha256_hex(&stored);
                    activities::save_file(&mut conn, act.id, &format, &stored, &sha).await?;
                    if format == "fit" {
                        match fit::parse_samples(&stored) {
                            Ok(samples) => {
                                sample_rows =
                                    samples::insert_batch(&mut conn, act.id, &samples).await?;
                            }
                            Err(e) => {
                                tracing::warn!(activity = act.id, %e, "kept the FIT, skipped samples")
                            }
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
    async fn ingest_daily(
        &mut self,
        kind: &str,
        date: NaiveDate,
        ceiling: i64,
    ) -> Result<bool, RunError> {
        if !self.budget(ceiling).await? {
            return Ok(false);
        }
        if kind == WELLNESS {
            return self.ingest_wellness(date).await;
        }
        match self
            .source
            .daily(&self.session, kind, &date.to_string())
            .await
        {
            Ok(Value::Null) => Ok(true), // a 204: no data that day; counts as done
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

    /// Fetch and store the day's wellness download with the readings parsed from it.
    async fn ingest_wellness(&mut self, date: NaiveDate) -> Result<bool, RunError> {
        let bytes = match self
            .source
            .wellness_file(&self.session, &date.to_string())
            .await
        {
            Ok(b) => b,
            Err(ClientError::Http(404)) => return Ok(true), // nothing synced that day
            Err(e) => return Err(e.into()),
        };
        let readings = fit::parse_wellness(&bytes);
        let device = readings.iter().find_map(|s| s.device_id);
        let mut tx = self.pool.begin().await?;
        wellness::save_file(&mut tx, date, &bytes, &sha256_hex(&bytes), device).await?;
        wellness::insert_samples(&mut tx, &readings).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Give wellness rows stored before devices were linked their device, re-reading the
    /// stored downloads (no request to Garmin). Returns how many days were linked.
    pub async fn relink_wellness(&self) -> Result<u64, RunError> {
        let mut linked = 0;
        for date in wellness::unlinked_dates(&self.pool).await? {
            let Some(bytes) = wellness::file(&self.pool, date).await? else {
                continue;
            };
            let readings = fit::parse_wellness(&bytes);
            let Some(device) = readings.iter().find_map(|s| s.device_id) else {
                continue;
            };
            let mut tx = self.pool.begin().await?;
            wellness::set_file_device(&mut tx, date, device).await?;
            wellness::insert_samples(&mut tx, &readings).await?;
            tx.commit().await?;
            linked += 1;
        }
        Ok(linked)
    }

    /// Refresh the account's device registry: one request.
    async fn ingest_devices(&mut self) -> Result<(), RunError> {
        if !self.budget(self.budget).await? {
            return Ok(());
        }
        let v = self.source.devices(&self.session).await?;
        let mut conn = self.pool.acquire().await?;
        raw::upsert(&mut conn, "devices", "registration", &v).await?;
        for d in v.as_array().into_iter().flatten() {
            if let Some(device) = normalize::device(d) {
                devices::upsert(&mut conn, &device, d).await?;
            }
        }
        Ok(())
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
        // Daily metrics settle over hours, so the window is re-read every few hours, not
        // on every pass: 36 requests a pass would be ~3,500 a day at a 15-minute poll.
        if self
            .daily_polled
            .is_none_or(|at| at.elapsed() >= DAILY_POLL_EVERY)
        {
            self.ingest_devices().await?;
            let today = Utc::now().date_naive();
            for kind in DAILY_KINDS {
                for back in 0..POLL_TRAILING_DAYS {
                    if self
                        .ingest_daily(kind, today - TimeDelta::days(back), self.budget)
                        .await?
                    {
                        report.daily += 1;
                    }
                }
            }
            self.daily_polled = Some(Instant::now());
        }
        sync::record_ok(&self.pool, LOOP).await?;
        Ok(report)
    }

    /// One backfill step under the budget less [`POLL_RESERVE`]. Walks the activity list
    /// older and older (cursor = next page start), then the daily kinds back day by day to
    /// the floor, always the kind whose cursor is newest: the kinds move back together, so
    /// recent years are whole for every kind before any old ones are read.
    /// The caller paces [`BackfillStep::Progressed`] and waits on
    /// [`BackfillStep::BudgetSpent`].
    pub async fn backfill_step(&mut self) -> Result<BackfillStep, RunError> {
        if sync::all_backfill_done(&self.pool, &all_kinds()).await? {
            return Ok(BackfillStep::Done);
        }
        self.authenticate().await?;
        // Activities first.
        let (cur, done) = sync::cursor_get(&self.pool, "activities")
            .await?
            .unwrap_or_default();
        if !done {
            if !self.budget(self.backfill_ceiling()).await? {
                return Ok(BackfillStep::BudgetSpent);
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
            return Ok(BackfillStep::Progressed);
        }
        // Then the daily kinds, the one with the newest cursor first.
        let floor = self.floor().await?;
        let mut pick: Option<(&str, NaiveDate)> = None;
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
            if pick.is_none_or(|(_, newest)| next > newest) {
                pick = Some((kind, next));
            }
        }
        let Some((kind, next)) = pick else {
            return Ok(BackfillStep::Done);
        };
        if !self
            .ingest_daily(kind, next, self.backfill_ceiling())
            .await?
        {
            return Ok(BackfillStep::BudgetSpent);
        }
        sync::cursor_set(
            &self.pool,
            kind,
            &(next - TimeDelta::days(1)).to_string(),
            false,
        )
        .await?;
        Ok(BackfillStep::Progressed)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
