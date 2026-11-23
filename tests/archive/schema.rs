//! Schema tests: the migrations apply, the sample primary key rejects a duplicate
//! timestamp, a daily metric upsert overwrites, and the MCP role cannot read the session.

use garmin_core::db::{create_pool, ensure_schema};
use serde_json::json;

use crate::support::{self, db_test};

db_test!(migrations_apply_twice_cleanly, |f| {
    let again = ensure_schema(&f.pool).await.expect("re-apply is a no-op");
    assert!(again.is_empty(), "second apply ran: {again:?}");
});

db_test!(a_sample_pk_rejects_a_duplicate_ts, |f| {
    sqlx::query("INSERT INTO activity (id, start_ts, summary) VALUES (1, now(), '{}'::jsonb)")
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO activity_sample (activity_id, ts, heart_rate) VALUES (1, '2026-01-01T00:00:00Z', 100)")
        .execute(&f.pool)
        .await
        .unwrap();
    // Same (activity_id, ts) is a duplicate: a re-parse must not double-insert.
    let dup = sqlx::query("INSERT INTO activity_sample (activity_id, ts, heart_rate) VALUES (1, '2026-01-01T00:00:00Z', 101)")
        .execute(&f.pool)
        .await
        .unwrap_err();
    assert_eq!(
        dup.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505"),
        "{dup}"
    );
});

db_test!(a_daily_metric_upsert_overwrites, |f| {
    sqlx::query("INSERT INTO daily_metric (kind, date, raw) VALUES ('sleep', '2026-01-01', $1)")
        .bind(json!({"score": 70}))
        .execute(&f.pool)
        .await
        .unwrap();
    // Garmin recomputed the night: the same (kind, date) overwrites.
    sqlx::query(
        "INSERT INTO daily_metric (kind, date, raw) VALUES ('sleep', '2026-01-01', $1) \
         ON CONFLICT (kind, date) DO UPDATE SET raw = EXCLUDED.raw, fetched_at = now()",
    )
    .bind(json!({"score": 82}))
    .execute(&f.pool)
    .await
    .unwrap();
    let score: i64 = sqlx::query_scalar(
        "SELECT (raw->>'score')::bigint FROM daily_metric WHERE kind = 'sleep' AND date = '2026-01-01'",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(score, 82);
});

const GRANTS_DB: &str = "garmin_session_grants";

db_test!(the_mcp_role_cannot_read_the_session, |f| {
    let Some(dsn) = support::container_dsn().await else {
        return;
    };
    sqlx::query(
        "DO $$ BEGIN CREATE ROLE garmin_mcp; EXCEPTION WHEN duplicate_object THEN NULL; END $$",
    )
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("DROP DATABASE IF EXISTS garmin_session_grants WITH (FORCE)")
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("CREATE DATABASE garmin_session_grants")
        .execute(&f.pool)
        .await
        .unwrap();
    let (base, _) = dsn.rsplit_once('/').expect("the dsn names a database");
    let pool = create_pool(&format!("{base}/{GRANTS_DB}")).await.unwrap();
    sqlx::query("GRANT USAGE ON SCHEMA public TO garmin_mcp")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO garmin_mcp")
        .execute(&pool)
        .await
        .unwrap();
    ensure_schema(&pool).await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE garmin_mcp")
        .execute(&mut *tx)
        .await
        .unwrap();
    let activities: i64 = sqlx::query_scalar("SELECT count(*) FROM activity")
        .fetch_one(&mut *tx)
        .await
        .expect("the default grant reaches the archive");
    assert_eq!(activities, 0);
    let denied = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM ingest_session")
        .fetch_one(&mut *tx)
        .await
        .expect_err("the MCP role must not read the session");
    assert_eq!(
        denied.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501"),
        "{denied}"
    );
    drop(tx);
    pool.close().await;
    sqlx::query("DROP DATABASE IF EXISTS garmin_session_grants WITH (FORCE)")
        .execute(&f.pool)
        .await
        .unwrap();
});
