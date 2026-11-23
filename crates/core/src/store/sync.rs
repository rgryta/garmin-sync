//! Loop bookkeeping: last runs, the auth-error flag, the daily request budget, the
//! backfill floor, and per-kind backfill cursors.

use chrono::{DateTime, Utc};
use sqlx::{Row, postgres::PgPool};

pub async fn record_ok(pool: &PgPool, loop_: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO sync_state (loop, last_run_at, last_ok_at, last_error) \
         VALUES ($1, now(), now(), NULL) \
         ON CONFLICT (loop) DO UPDATE SET last_run_at = now(), last_ok_at = now(), last_error = NULL",
    )
    .bind(loop_)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_error(pool: &PgPool, loop_: &str, error: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO sync_state (loop, last_run_at, last_error) VALUES ($1, now(), $2) \
         ON CONFLICT (loop) DO UPDATE SET last_run_at = now(), last_error = EXCLUDED.last_error",
    )
    .bind(loop_)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_auth_error(pool: &PgPool, account: &str, error: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO account (id, last_auth_error, last_auth_error_at, updated_at) \
         VALUES ($1, $2, now(), now()) \
         ON CONFLICT (id) DO UPDATE SET last_auth_error = EXCLUDED.last_auth_error, \
           last_auth_error_at = now(), updated_at = now()",
    )
    .bind(account)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn clear_auth_error(pool: &PgPool, account: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE account SET last_auth_error = NULL, last_auth_error_at = NULL, updated_at = now() \
         WHERE id = $1 AND last_auth_error IS NOT NULL",
    )
    .bind(account)
    .execute(pool)
    .await?;
    Ok(())
}

/// Spend `n` requests against today's `ceiling` (UTC day, counted in the row so a restart
/// does not hand out a fresh allowance). False when the ceiling is reached.
pub async fn take_budget(pool: &PgPool, loop_: &str, n: i64, ceiling: i64) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "INSERT INTO sync_state (loop, budget_day, budget_spent) VALUES ($1, current_date, 0) \
         ON CONFLICT (loop) DO UPDATE SET \
           budget_day = CASE WHEN sync_state.budget_day = current_date THEN sync_state.budget_day ELSE current_date END, \
           budget_spent = CASE WHEN sync_state.budget_day = current_date THEN sync_state.budget_spent ELSE 0 END \
         RETURNING budget_spent",
    )
    .bind(loop_)
    .fetch_one(&mut *tx)
    .await?;
    let spent: i64 = row.get("budget_spent");
    if spent + n > ceiling {
        tx.commit().await?;
        return Ok(false);
    }
    sqlx::query("UPDATE sync_state SET budget_spent = budget_spent + $2 WHERE loop = $1")
        .bind(loop_)
        .bind(n)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn cursor_get(pool: &PgPool, kind: &str) -> sqlx::Result<Option<(String, bool)>> {
    let row: Option<(Option<String>, bool)> =
        sqlx::query_as("SELECT cursor, done FROM backfill_cursor WHERE kind = $1")
            .bind(kind)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(c, d)| (c.unwrap_or_default(), d)))
}

pub async fn cursor_set(pool: &PgPool, kind: &str, cursor: &str, done: bool) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO backfill_cursor (kind, cursor, done) VALUES ($1, $2, $3) \
         ON CONFLICT (kind) DO UPDATE SET cursor = EXCLUDED.cursor, done = EXCLUDED.done",
    )
    .bind(kind)
    .bind(cursor)
    .bind(done)
    .execute(pool)
    .await?;
    Ok(())
}

/// True when every registered backfill kind is done.
pub async fn all_backfill_done(pool: &PgPool, kinds: &[&str]) -> sqlx::Result<bool> {
    let names: Vec<String> = kinds.iter().map(|s| s.to_string()).collect();
    let done: i64 =
        sqlx::query_scalar("SELECT count(*) FROM backfill_cursor WHERE kind = ANY($1) AND done")
            .bind(&names)
            .fetch_one(pool)
            .await?;
    Ok(done as usize == kinds.len())
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct StatusRow {
    pub loop_name: String,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub budget_day: Option<chrono::NaiveDate>,
    pub budget_spent: i64,
}

pub async fn status(pool: &PgPool) -> sqlx::Result<Vec<StatusRow>> {
    sqlx::query_as(
        "SELECT loop AS loop_name, last_run_at, last_ok_at, last_error, budget_day, budget_spent \
         FROM sync_state ORDER BY loop",
    )
    .fetch_all(pool)
    .await
}
