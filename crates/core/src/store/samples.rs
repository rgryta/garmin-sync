//! Per-second activity samples: batched insert and a downsampled read.

use chrono::{DateTime, Utc};
use sqlx::AssertSqlSafe;
use sqlx::{PgConnection, postgres::PgPool};

use crate::models::Sample;

/// Insert samples in chunks, idempotent on the (activity_id, ts) key. Returns how many
/// rows were newly inserted. A six-hour ride is tens of thousands of points, so they go
/// in bounded batches rather than one statement.
pub async fn insert_batch(
    conn: &mut PgConnection,
    activity_id: i64,
    samples: &[Sample],
) -> sqlx::Result<u64> {
    const CHUNK: usize = 1000;
    let mut inserted = 0u64;
    for chunk in samples.chunks(CHUNK) {
        // Build one multi-row INSERT per chunk with positional binds.
        let mut sql = String::from(
            "INSERT INTO activity_sample (activity_id, ts, elapsed_s, heart_rate, cadence, power, \
             speed, altitude, distance_m, lat, lon, temperature, extra) VALUES ",
        );
        let cols = 13;
        for i in 0..chunk.len() {
            if i > 0 {
                sql.push(',');
            }
            let b = i * cols;
            sql.push_str(&format!(
                "(${},${},${},${},${},${},${},${},${},${},${},${},${})",
                b + 1,
                b + 2,
                b + 3,
                b + 4,
                b + 5,
                b + 6,
                b + 7,
                b + 8,
                b + 9,
                b + 10,
                b + 11,
                b + 12,
                b + 13
            ));
        }
        sql.push_str(" ON CONFLICT (activity_id, ts) DO NOTHING");
        // The SQL is built only from the column count and chunk length — no caller input.
        let mut q = sqlx::query(AssertSqlSafe(sql));
        for s in chunk {
            q = q
                .bind(activity_id)
                .bind(s.ts)
                .bind(s.elapsed_s)
                .bind(s.heart_rate)
                .bind(s.cadence)
                .bind(s.power)
                .bind(s.speed)
                .bind(s.altitude)
                .bind(s.distance_m)
                .bind(s.lat)
                .bind(s.lon)
                .bind(s.temperature)
                .bind(&s.extra);
        }
        inserted += q.execute(&mut *conn).await?.rows_affected();
    }
    Ok(inserted)
}

/// A downsampled summary of an activity's samples, for the MCP (never the full dump).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SampleSummary {
    pub count: i64,
    pub first_ts: Option<DateTime<Utc>>,
    pub last_ts: Option<DateTime<Utc>>,
    pub avg_heart_rate: Option<f64>,
    pub max_heart_rate: Option<i32>,
    pub avg_power: Option<f64>,
    pub max_power: Option<i32>,
    pub avg_speed: Option<f64>,
    pub max_altitude: Option<f64>,
}

pub async fn summary(pool: &PgPool, activity_id: i64) -> sqlx::Result<SampleSummary> {
    sqlx::query_as(
        "SELECT count(*) AS count, min(ts) AS first_ts, max(ts) AS last_ts, \
            avg(heart_rate)::double precision AS avg_heart_rate, max(heart_rate) AS max_heart_rate, \
            avg(power)::double precision AS avg_power, max(power) AS max_power, \
            avg(speed)::double precision AS avg_speed, max(altitude) AS max_altitude \
         FROM activity_sample WHERE activity_id = $1",
    )
    .bind(activity_id)
    .fetch_one(pool)
    .await
}
