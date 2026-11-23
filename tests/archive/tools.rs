//! Tool-logic tests: activities list, one activity with its sample summary, a day's
//! metrics, a metric series, and the `sql` tool's guards (no write, no session, timeout).

use std::time::Duration;

use chrono::{TimeZone, Utc};
use garmin_core::db::{create_pool, ensure_schema};
use garmin_core::models::{Activity, Sample};
use garmin_core::store::{activities, daily, samples, sql};
use garmin_mcp_server::tools;
use serde_json::json;

use crate::support::{self, db_test};

async fn seed_activity(pool: &sqlx::PgPool, id: i64) {
    let mut conn = pool.acquire().await.unwrap();
    activities::upsert(
        &mut conn,
        &Activity {
            id,
            parent_id: None,
            start_ts: Utc.with_ymd_and_hms(2026, 3, 1, 8, 0, 0).unwrap(),
            type_: Some("running".into()),
            name: Some("Run".into()),
            manual: Some(false),
            duration_s: Some(1800.0),
            distance_m: Some(5000.0),
            device: None,
            summary: json!({"activityId": id}),
        },
    )
    .await
    .unwrap();
    samples::insert_batch(
        &mut conn,
        id,
        &[Sample {
            ts: Utc.with_ymd_and_hms(2026, 3, 1, 8, 0, 0).unwrap(),
            elapsed_s: Some(0),
            heart_rate: Some(150),
            cadence: None,
            power: Some(200),
            speed: None,
            altitude: Some(100.0),
            distance_m: None,
            lat: None,
            lon: None,
            temperature: None,
            extra: None,
        }],
    )
    .await
    .unwrap();
}

db_test!(activities_list_and_filter, |f| {
    seed_activity(&f.pool, 1).await;
    let out = tools::activities_tool(&f.pool, None, None, None, None)
        .await
        .unwrap();
    assert!(out.contains("[1]"), "{out}");
    assert!(out.contains("running"), "{out}");
    // A non-matching type filter yields nothing.
    let none = tools::activities_tool(&f.pool, None, None, Some("cycling"), None)
        .await
        .unwrap();
    assert!(none.contains("No activities"), "{none}");
});

db_test!(activity_shows_a_sample_summary_not_the_dump, |f| {
    seed_activity(&f.pool, 2).await;
    let out = tools::activity_tool(&f.pool, 2).await.unwrap();
    assert!(out.contains("1 points"), "{out}");
    assert!(out.contains("max HR 150"), "{out}");
});

db_test!(day_and_metric, |f| {
    let mut conn = f.pool.acquire().await.unwrap();
    daily::upsert(
        &mut conn,
        "sleep",
        chrono::NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        &json!({"score": 80}),
    )
    .await
    .unwrap();
    let day = tools::day_tool(&f.pool, "2026-03-01").await.unwrap();
    assert!(day.contains("sleep"), "{day}");
    let metric = tools::metric_tool(&f.pool, "sleep", None, None)
        .await
        .unwrap();
    assert!(metric.contains("2026-03-01"), "{metric}");
});

db_test!(sql_refuses_a_write, |f| {
    let err = tools::sql_tool(
        &f.pool,
        "INSERT INTO activity (id, start_ts, summary) VALUES (9, now(), '{}')",
    )
    .await
    .unwrap_err();
    assert!(matches!(err, tools::ToolError::Usage(_)), "{err:?}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM activity WHERE id = 9")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
});

db_test!(sql_times_out, |f| {
    let err = sql::run_readonly(
        &f.pool,
        "SELECT pg_sleep(5)",
        500,
        Duration::from_millis(500),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("57014"),
        "{err}"
    );
});

const SESSION_DB: &str = "garmin_mcp_sql_session";

db_test!(sql_cannot_read_the_session, |f| {
    let Some(dsn) = support::container_dsn().await else {
        return;
    };
    sqlx::query("DO $$ BEGIN CREATE ROLE garmin_mcp LOGIN PASSWORD 'pw'; EXCEPTION WHEN duplicate_object THEN NULL; END $$")
        .execute(&f.pool).await.unwrap();
    sqlx::query("ALTER ROLE garmin_mcp LOGIN PASSWORD 'pw'")
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("DROP DATABASE IF EXISTS garmin_mcp_sql_session WITH (FORCE)")
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("CREATE DATABASE garmin_mcp_sql_session")
        .execute(&f.pool)
        .await
        .unwrap();
    let (base, _) = dsn.rsplit_once('/').unwrap();
    let admin = create_pool(&format!("{base}/{SESSION_DB}")).await.unwrap();
    sqlx::query("GRANT USAGE ON SCHEMA public TO garmin_mcp")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO garmin_mcp")
        .execute(&admin)
        .await
        .unwrap();
    ensure_schema(&admin).await.unwrap();
    let at = dsn.rsplit_once('@').unwrap().1;
    let host_db = at.rsplit_once('/').unwrap().0;
    let as_mcp = create_pool(&format!(
        "postgresql://garmin_mcp:pw@{host_db}/{SESSION_DB}"
    ))
    .await
    .unwrap();
    let denied = sql::run_readonly(
        &as_mcp,
        "SELECT count(*) FROM ingest_session",
        10,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert_eq!(
        denied.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501"),
        "{denied}"
    );
    as_mcp.close().await;
    admin.close().await;
    sqlx::query("DROP DATABASE IF EXISTS garmin_mcp_sql_session WITH (FORCE)")
        .execute(&f.pool)
        .await
        .unwrap();
});
