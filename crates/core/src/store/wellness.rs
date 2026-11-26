//! The day's wellness download and the intraday readings parsed from it.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, postgres::PgPool};

use crate::models::WellnessSample;

/// Store the day's download; a re-fetch (today's file grows) replaces it.
pub async fn save_file(
    conn: &mut PgConnection,
    date: NaiveDate,
    bytes: &[u8],
    sha256: &str,
    device_id: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO wellness_file (date, bytes, sha256, device_id, fetched_at) \
         VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (date) DO UPDATE SET bytes = EXCLUDED.bytes, sha256 = EXCLUDED.sha256, \
           device_id = EXCLUDED.device_id, fetched_at = now()",
    )
    .bind(date)
    .bind(bytes)
    .bind(sha256)
    .bind(device_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Insert readings, idempotent on (kind, ts); a stored reading with no device gets this
/// one's. `samples` must not repeat a (kind, ts). Returns how many rows were written.
pub async fn insert_samples(
    conn: &mut PgConnection,
    samples: &[WellnessSample],
) -> sqlx::Result<u64> {
    let kinds: Vec<&str> = samples.iter().map(|s| s.kind).collect();
    let ts: Vec<DateTime<Utc>> = samples.iter().map(|s| s.ts).collect();
    let values: Vec<f64> = samples.iter().map(|s| s.value).collect();
    let devices: Vec<Option<i64>> = samples.iter().map(|s| s.device_id).collect();
    Ok(sqlx::query(
        "INSERT INTO wellness_sample (kind, ts, value, device_id) \
         SELECT * FROM UNNEST($1::text[], $2::timestamptz[], $3::float8[], $4::bigint[]) \
         ON CONFLICT (kind, ts) DO UPDATE SET device_id = EXCLUDED.device_id \
           WHERE wellness_sample.device_id IS NULL AND EXCLUDED.device_id IS NOT NULL",
    )
    .bind(&kinds)
    .bind(&ts)
    .bind(&values)
    .bind(&devices)
    .execute(conn)
    .await?
    .rows_affected())
}

/// Days whose stored download has no device yet (stored before devices were linked).
pub async fn unlinked_dates(pool: &PgPool) -> sqlx::Result<Vec<NaiveDate>> {
    sqlx::query_scalar("SELECT date FROM wellness_file WHERE device_id IS NULL ORDER BY date")
        .fetch_all(pool)
        .await
}

/// One day's stored download.
pub async fn file(pool: &PgPool, date: NaiveDate) -> sqlx::Result<Option<Vec<u8>>> {
    sqlx::query_scalar("SELECT bytes FROM wellness_file WHERE date = $1")
        .bind(date)
        .fetch_optional(pool)
        .await
}

pub async fn set_file_device(
    conn: &mut PgConnection,
    date: NaiveDate,
    device_id: i64,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE wellness_file SET device_id = $2 WHERE date = $1")
        .bind(date)
        .bind(device_id)
        .execute(conn)
        .await?;
    Ok(())
}
