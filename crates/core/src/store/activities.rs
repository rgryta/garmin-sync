//! Activities, their original files, and the read side.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, postgres::PgPool};

use crate::models::{Activity, Change};

/// Upsert an activity. A changed summary counts as Updated; a reappearance clears
/// `deleted_at`.
pub async fn upsert(conn: &mut PgConnection, a: &Activity) -> sqlx::Result<Change> {
    let before: Option<sqlx::types::Json<serde_json::Value>> =
        sqlx::query_scalar("SELECT summary FROM activity WHERE id = $1")
            .bind(a.id)
            .fetch_optional(&mut *conn)
            .await?;
    sqlx::query(
        "INSERT INTO activity \
           (id, parent_id, start_ts, type, name, manual, duration_s, distance_m, device, deleted_at, summary, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NULL, $10, now()) \
         ON CONFLICT (id) DO UPDATE SET parent_id = EXCLUDED.parent_id, start_ts = EXCLUDED.start_ts, \
           type = EXCLUDED.type, name = EXCLUDED.name, manual = EXCLUDED.manual, \
           duration_s = EXCLUDED.duration_s, distance_m = EXCLUDED.distance_m, device = EXCLUDED.device, \
           deleted_at = NULL, summary = EXCLUDED.summary, updated_at = now()",
    )
    .bind(a.id)
    .bind(a.parent_id)
    .bind(a.start_ts)
    .bind(&a.type_)
    .bind(&a.name)
    .bind(a.manual)
    .bind(a.duration_s)
    .bind(a.distance_m)
    .bind(&a.device)
    .bind(&a.summary)
    .execute(&mut *conn)
    .await?;
    Ok(match before {
        None => Change::New,
        Some(prev) if prev.0 != a.summary => Change::Updated,
        Some(_) => Change::Unchanged,
    })
}

/// Store the original file for an activity. Idempotent on the activity_id pk.
pub async fn save_file(
    conn: &mut PgConnection,
    activity_id: i64,
    format: &str,
    bytes: &[u8],
    sha256: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO activity_file (activity_id, format, bytes, sha256, fetched_at) \
         VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (activity_id) DO UPDATE SET format = EXCLUDED.format, bytes = EXCLUDED.bytes, \
           sha256 = EXCLUDED.sha256, fetched_at = now()",
    )
    .bind(activity_id)
    .bind(format)
    .bind(bytes)
    .bind(sha256)
    .execute(conn)
    .await?;
    Ok(())
}

/// Whether an activity already has its original file stored (so backfill can skip it).
pub async fn has_file(pool: &PgPool, activity_id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT exists(SELECT 1 FROM activity_file WHERE activity_id = $1)")
        .bind(activity_id)
        .fetch_one(pool)
        .await
}

pub async fn mark_deleted(conn: &mut PgConnection, id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE activity SET deleted_at = COALESCE(deleted_at, now()), updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}

/// The newest activity's start, for the poll to fetch only what is newer.
pub async fn newest_start(pool: &PgPool) -> sqlx::Result<Option<DateTime<Utc>>> {
    sqlx::query_scalar("SELECT max(start_ts) FROM activity WHERE deleted_at IS NULL")
        .fetch_one(pool)
        .await
}

/// The oldest activity's start: where the account's history begins.
pub async fn oldest_start(pool: &PgPool) -> sqlx::Result<Option<DateTime<Utc>>> {
    sqlx::query_scalar("SELECT min(start_ts) FROM activity")
        .fetch_one(pool)
        .await
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ActivityRow {
    pub id: i64,
    pub start_ts: DateTime<Utc>,
    pub type_: Option<String>,
    pub name: Option<String>,
    pub manual: Option<bool>,
    pub duration_s: Option<f64>,
    pub distance_m: Option<f64>,
}

pub async fn list(
    pool: &PgPool,
    after: Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
    type_: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<ActivityRow>> {
    sqlx::query_as(
        "SELECT id, start_ts, type AS type_, name, manual, duration_s, distance_m FROM activity \
         WHERE deleted_at IS NULL \
           AND ($1::timestamptz IS NULL OR start_ts >= $1) \
           AND ($2::timestamptz IS NULL OR start_ts <= $2) \
           AND ($3::text IS NULL OR type = $3) \
         ORDER BY start_ts DESC LIMIT $4",
    )
    .bind(after)
    .bind(before)
    .bind(type_)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// An activity's summary JSON, for the `activity` tool.
pub async fn get(pool: &PgPool, id: i64) -> sqlx::Result<Option<serde_json::Value>> {
    sqlx::query_scalar("SELECT summary FROM activity WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}
