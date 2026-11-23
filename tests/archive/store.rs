//! Store tests: activity upsert, batched sample insert (idempotent), the budget's
//! UTC-day reset, and that a deleted activity keeps its file and samples.

use chrono::{TimeDelta, TimeZone, Utc};
use garmin_core::models::{Activity, Change, Sample};
use garmin_core::store::{activities, daily, samples, sync};
use serde_json::json;

use crate::support::db_test;

fn activity(id: i64, summary: serde_json::Value) -> Activity {
    Activity {
        id,
        parent_id: None,
        start_ts: Utc.with_ymd_and_hms(2026, 3, 1, 8, 0, 0).unwrap(),
        type_: Some("running".into()),
        name: Some("Morning Run".into()),
        manual: Some(false),
        duration_s: Some(1800.0),
        distance_m: Some(5000.0),
        device: Some("fenix".into()),
        summary,
    }
}

db_test!(a_new_activity_is_new_then_unchanged, |f| {
    let mut conn = f.pool.acquire().await.unwrap();
    let a = activity(100, json!({"v": 1}));
    assert_eq!(
        activities::upsert(&mut conn, &a).await.unwrap(),
        Change::New
    );
    assert_eq!(
        activities::upsert(&mut conn, &a).await.unwrap(),
        Change::Unchanged
    );
    let changed = activity(100, json!({"v": 2}));
    assert_eq!(
        activities::upsert(&mut conn, &changed).await.unwrap(),
        Change::Updated
    );
});

db_test!(samples_insert_in_batches_and_are_idempotent, |f| {
    let mut conn = f.pool.acquire().await.unwrap();
    activities::upsert(&mut conn, &activity(200, json!({})))
        .await
        .unwrap();
    // 25k points across the batch boundary.
    let base = Utc.with_ymd_and_hms(2026, 3, 1, 8, 0, 0).unwrap();
    let batch: Vec<Sample> = (0..25_000)
        .map(|i| Sample {
            ts: base + TimeDelta::seconds(i),
            elapsed_s: Some(i as i32),
            heart_rate: Some(100 + (i % 60) as i32),
            cadence: None,
            power: Some(200),
            speed: Some(3.0),
            altitude: Some(10.0),
            distance_m: Some(i as f64 * 3.0),
            lat: Some(52.0),
            lon: Some(21.0),
            temperature: Some(15.0),
            extra: None,
        })
        .collect();
    let n = samples::insert_batch(&mut conn, 200, &batch).await.unwrap();
    assert_eq!(n, 25_000);
    // A re-parse inserts nothing new (idempotent on the pk).
    let again = samples::insert_batch(&mut conn, 200, &batch).await.unwrap();
    assert_eq!(again, 0);
    let s = samples::summary(&f.pool, 200).await.unwrap();
    assert_eq!(s.count, 25_000);
    assert_eq!(s.max_heart_rate, Some(159));
});

db_test!(the_budget_resets_on_a_new_utc_day, |f| {
    assert!(sync::take_budget(&f.pool, "backfill", 3, 5).await.unwrap());
    assert!(!sync::take_budget(&f.pool, "backfill", 3, 5).await.unwrap());
    sqlx::query("UPDATE sync_state SET budget_day = current_date - 1 WHERE loop = 'backfill'")
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(sync::take_budget(&f.pool, "backfill", 3, 5).await.unwrap());
});

db_test!(a_deleted_activity_keeps_its_file_and_samples, |f| {
    let mut conn = f.pool.acquire().await.unwrap();
    activities::upsert(&mut conn, &activity(300, json!({})))
        .await
        .unwrap();
    activities::save_file(&mut conn, 300, "fit", b"FITDATA", "sha")
        .await
        .unwrap();
    let base = Utc.with_ymd_and_hms(2026, 3, 1, 8, 0, 0).unwrap();
    samples::insert_batch(
        &mut conn,
        300,
        &[Sample {
            ts: base,
            elapsed_s: Some(0),
            heart_rate: Some(120),
            cadence: None,
            power: None,
            speed: None,
            altitude: None,
            distance_m: None,
            lat: None,
            lon: None,
            temperature: None,
            extra: None,
        }],
    )
    .await
    .unwrap();
    activities::mark_deleted(&mut conn, 300).await.unwrap();
    // The activity is flagged deleted but file + samples remain.
    let deleted: bool =
        sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM activity WHERE id = 300")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(deleted);
    assert!(activities::has_file(&f.pool, 300).await.unwrap());
    assert_eq!(samples::summary(&f.pool, 300).await.unwrap().count, 1);
});

db_test!(a_daily_kind_spans_and_reads_back, |f| {
    let mut conn = f.pool.acquire().await.unwrap();
    for d in 1..=3 {
        daily::upsert(
            &mut conn,
            "steps",
            chrono::NaiveDate::from_ymd_opt(2026, 3, d).unwrap(),
            &json!({"steps": d * 1000}),
        )
        .await
        .unwrap();
    }
    let (min, max) = daily::span(&f.pool, "steps").await.unwrap();
    assert_eq!(min, chrono::NaiveDate::from_ymd_opt(2026, 3, 1));
    assert_eq!(max, chrono::NaiveDate::from_ymd_opt(2026, 3, 3));
    let series = daily::series(&f.pool, "steps", None, None, 10)
        .await
        .unwrap();
    assert_eq!(series.len(), 3);
});
