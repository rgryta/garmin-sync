//! The raw-first daily-metric store, keyed by (kind, date), plus the read side.

use chrono::NaiveDate;
use serde_json::Value;
use sqlx::{PgConnection, postgres::PgPool};

/// Upsert a daily metric; Garmin recomputes some, so the same (kind, date) overwrites.
pub async fn upsert(
    conn: &mut PgConnection,
    kind: &str,
    date: NaiveDate,
    raw: &Value,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO daily_metric (kind, date, raw, fetched_at) VALUES ($1, $2, $3, now()) \
         ON CONFLICT (kind, date) DO UPDATE SET raw = EXCLUDED.raw, fetched_at = now()",
    )
    .bind(kind)
    .bind(date)
    .bind(raw)
    .execute(conn)
    .await?;
    Ok(())
}

/// The oldest and newest date held for a kind, so backfill knows where it has reached.
pub async fn span(
    pool: &PgPool,
    kind: &str,
) -> sqlx::Result<(Option<NaiveDate>, Option<NaiveDate>)> {
    let row: (Option<NaiveDate>, Option<NaiveDate>) =
        sqlx::query_as("SELECT min(date), max(date) FROM daily_metric WHERE kind = $1")
            .bind(kind)
            .fetch_one(pool)
            .await?;
    Ok(row)
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct DailyRow {
    pub kind: String,
    pub date: NaiveDate,
    pub raw: Value,
}

/// One kind over a date range, oldest first.
pub async fn series(
    pool: &PgPool,
    kind: &str,
    after: Option<NaiveDate>,
    before: Option<NaiveDate>,
    limit: i64,
) -> sqlx::Result<Vec<DailyRow>> {
    sqlx::query_as(
        "SELECT kind, date, raw FROM daily_metric \
         WHERE kind = $1 AND ($2::date IS NULL OR date >= $2) AND ($3::date IS NULL OR date <= $3) \
         ORDER BY date LIMIT $4",
    )
    .bind(kind)
    .bind(after)
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Every metric for one date (the `day` tool).
pub async fn day(pool: &PgPool, date: NaiveDate) -> sqlx::Result<Vec<DailyRow>> {
    sqlx::query_as("SELECT kind, date, raw FROM daily_metric WHERE date = $1 ORDER BY kind")
        .bind(date)
        .fetch_all(pool)
        .await
}

/// The distinct kinds held, for `status`.
pub async fn kinds(pool: &PgPool) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as("SELECT kind, count(*) FROM daily_metric GROUP BY kind ORDER BY kind")
        .fetch_all(pool)
        .await
}
