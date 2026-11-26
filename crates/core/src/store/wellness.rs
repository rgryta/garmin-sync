//! The day's wellness download and the intraday readings parsed from it.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgConnection;

use crate::models::WellnessSample;

/// Store the day's download; a re-fetch (today's file grows) replaces it.
pub async fn save_file(
    conn: &mut PgConnection,
    date: NaiveDate,
    bytes: &[u8],
    sha256: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO wellness_file (date, bytes, sha256, fetched_at) VALUES ($1, $2, $3, now()) \
         ON CONFLICT (date) DO UPDATE SET bytes = EXCLUDED.bytes, sha256 = EXCLUDED.sha256, \
           fetched_at = now()",
    )
    .bind(date)
    .bind(bytes)
    .bind(sha256)
    .execute(conn)
    .await?;
    Ok(())
}

/// Insert readings, idempotent on (kind, ts). Returns how many were new.
pub async fn insert_samples(
    conn: &mut PgConnection,
    samples: &[WellnessSample],
) -> sqlx::Result<u64> {
    let kinds: Vec<&str> = samples.iter().map(|s| s.kind).collect();
    let ts: Vec<DateTime<Utc>> = samples.iter().map(|s| s.ts).collect();
    let values: Vec<f64> = samples.iter().map(|s| s.value).collect();
    Ok(sqlx::query(
        "INSERT INTO wellness_sample (kind, ts, value) \
         SELECT * FROM UNNEST($1::text[], $2::timestamptz[], $3::float8[]) \
         ON CONFLICT (kind, ts) DO NOTHING",
    )
    .bind(&kinds)
    .bind(&ts)
    .bind(&values)
    .execute(conn)
    .await?
    .rows_affected())
}
