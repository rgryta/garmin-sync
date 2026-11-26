//! Runner tests against a fake Source and the test database: the rolled refresh token is
//! saved before use, a valid token is left alone, a bad FIT keeps the activity and blob,
//! and a poll ingests only new activities.

use chrono::{TimeDelta, Utc};
use garmin_client::{ClientError, Session};
use garmin_ingest::runner::{BackfillStep, Runner};
use garmin_ingest::source::Source;
use serde_json::{Value, json};

use crate::support::db_test;

fn seed_session(expired: bool) -> Session {
    Session {
        refresh_token: "r0".into(),
        client_id: "cid".into(),
        access_token: "a0".into(),
        access_expires_at: Some(if expired {
            Utc::now() - TimeDelta::minutes(5)
        } else {
            Utc::now() + TimeDelta::hours(1)
        }),
        oauth1_token: None,
        oauth1_secret: None,
        garmin_user_id: Some("u1".into()),
    }
}

/// A valid minimal FIT with one record (so samples parse), or garbage when `bad`.
fn fit(bad: bool) -> Vec<u8> {
    if bad {
        return b"not a fit file".to_vec();
    }
    one_message(20, 3, None)
}

/// The fake watch's deviceId: in the registry and as its FIT files' serial.
const WATCH: u32 = 7;

/// A day's wellness download from the watch: one monitoring message with a heart rate.
fn wellness_fit() -> Vec<u8> {
    one_message(55, 27, Some(WATCH))
}

/// A FIT holding one message of type `global`: a timestamp and a uint8 at `field`, after
/// a file_id carrying `serial` when given.
fn one_message(global: u8, field: u8, serial: Option<u32>) -> Vec<u8> {
    fn crc(data: &[u8]) -> u16 {
        const T: [u16; 16] = [
            0x0000, 0xCC01, 0xD801, 0x1400, 0xF001, 0x3C00, 0x2800, 0xE401, 0xA001, 0x6C00, 0x7800,
            0xB401, 0x5000, 0x9C01, 0x8801, 0x4400,
        ];
        let mut c: u16 = 0;
        for &b in data {
            for n in [b & 0x0F, b >> 4] {
                let t = T[(c & 0x0F) as usize];
                c = (c >> 4) & 0x0FFF;
                c = c ^ t ^ T[n as usize];
            }
        }
        c
    }
    let mut d = Vec::new();
    if let Some(serial) = serial {
        // file_id (global 0) with serial_number (field 3, uint32z), as local type 1.
        d.extend_from_slice(&[0x41, 0x00, 0x00, 0, 0, 1, 3, 4, 0x8C, 0x01]);
        d.extend_from_slice(&serial.to_le_bytes());
    }
    d.extend_from_slice(&[0x40, 0x00, 0x00, global, 0, 2, 253, 4, 0x86, field, 1, 0x02]);
    d.push(0x00);
    d.extend_from_slice(&1_000_000_000u32.to_le_bytes());
    d.push(140);
    let mut f = vec![14, 0x20];
    f.extend_from_slice(&2191u16.to_le_bytes());
    f.extend_from_slice(&(d.len() as u32).to_le_bytes());
    f.extend_from_slice(b".FIT");
    let hc = crc(&f);
    f.extend_from_slice(&hc.to_le_bytes());
    f.extend_from_slice(&d);
    let fc = crc(&f);
    f.extend_from_slice(&fc.to_le_bytes());
    f
}

#[derive(Default)]
struct Fake {
    activities: Vec<Value>,
    bad_fit: bool,
    refreshed: std::sync::Mutex<bool>,
    daily_calls: std::sync::Mutex<Vec<(String, String)>>,
}

impl Source for Fake {
    async fn refresh(&self, s: &Session) -> Result<Session, ClientError> {
        *self.refreshed.lock().unwrap() = true;
        Ok(Session {
            access_token: "a1".into(),
            refresh_token: "r1".into(),
            ..s.clone()
        })
    }
    async fn user_id(&self, _s: &Session) -> Result<String, ClientError> {
        Ok("display-guid".into())
    }
    async fn activities(
        &self,
        _s: &Session,
        start: i64,
        _limit: i64,
    ) -> Result<Vec<Value>, ClientError> {
        Ok(if start == 0 {
            self.activities.clone()
        } else {
            vec![]
        })
    }
    async fn activity_details(&self, _s: &Session, _id: i64) -> Result<Value, ClientError> {
        Ok(json!({}))
    }
    async fn activity_file(&self, _s: &Session, _id: i64) -> Result<Vec<u8>, ClientError> {
        Ok(fit(self.bad_fit))
    }
    async fn daily(&self, _s: &Session, kind: &str, date: &str) -> Result<Value, ClientError> {
        self.daily_calls
            .lock()
            .unwrap()
            .push((kind.to_owned(), date.to_owned()));
        Ok(json!({"ok": true}))
    }
    async fn wellness_file(&self, _s: &Session, date: &str) -> Result<Vec<u8>, ClientError> {
        self.daily_calls
            .lock()
            .unwrap()
            .push(("wellness".to_owned(), date.to_owned()));
        Ok(wellness_fit())
    }
    async fn devices(&self, _s: &Session) -> Result<Value, ClientError> {
        Ok(json!([{"deviceId": WATCH, "productDisplayName": "test watch", "productSku": "010-1"}]))
    }
}

fn act(id: i64) -> Value {
    json!({"activityId": id, "startTimeGMT": "2026-03-01 08:00:00", "activityType": {"typeKey": "running"}})
}

/// An activity that started `days` days ago.
fn act_days_ago(id: i64, days: i64) -> Value {
    let start = (Utc::now() - TimeDelta::days(days)).format("%Y-%m-%d 08:00:00");
    json!({"activityId": id, "startTimeGMT": start.to_string(), "activityType": {"typeKey": "running"}})
}

/// Step backfill until it stops progressing; returns how it stopped.
async fn backfill_until_stopped(runner: &mut Runner<Fake>) -> BackfillStep {
    loop {
        match runner.backfill_step().await.unwrap() {
            BackfillStep::Progressed => continue,
            stopped => return stopped,
        }
    }
}

db_test!(
    backfill_moves_the_kinds_back_together_and_stops_at_the_first_activity,
    |f| {
        // History starts 5 days ago: the floor is the day before, so 4 days per kind.
        let fake = Fake {
            activities: vec![act_days_ago(1, 5)],
            ..Default::default()
        };
        let mut runner = Runner::new(
            f.pool.clone(),
            "owner".into(),
            2000,
            fake,
            seed_session(false),
        );
        assert_eq!(
            backfill_until_stopped(&mut runner).await,
            BackfillStep::Done
        );

        let calls = runner.source().daily_calls.lock().unwrap().clone();
        let kinds = garmin_ingest::runner::DAILY_KINDS.len();
        assert_eq!(calls.len(), kinds * 4, "nothing before the floor is asked");
        let today = Utc::now().date_naive();
        let floor = today - TimeDelta::days(6);
        assert!(
            calls
                .iter()
                .all(|(_, d)| d.parse::<chrono::NaiveDate>().unwrap() >= floor)
        );
        // Every kind reads a date before any kind moves to the day before.
        let first_day: Vec<_> = calls[..kinds].iter().map(|(_, d)| d.as_str()).collect();
        assert!(first_day.iter().all(|d| *d == first_day[0]));
    }
);

db_test!(
    backfill_leaves_the_poll_reserve_and_the_poll_spends_it,
    |f| {
        use garmin_ingest::runner::POLL_RESERVE;
        let fake = Fake {
            activities: vec![act_days_ago(1, 400)],
            ..Default::default()
        };
        let budget = POLL_RESERVE + 10;
        let mut runner = Runner::new(
            f.pool.clone(),
            "owner".into(),
            budget,
            fake,
            seed_session(false),
        );
        assert_eq!(
            backfill_until_stopped(&mut runner).await,
            BackfillStep::BudgetSpent
        );
        let spent: i64 =
            sqlx::query_scalar("SELECT budget_spent FROM sync_state WHERE loop = 'sync'")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(spent, 10, "backfill stops at the budget less the reserve");
        let report = runner.pass().await.unwrap();
        assert!(report.daily > 0, "the poll still reads the daily window");
    }
);

db_test!(the_daily_window_is_not_re_read_on_every_pass, |f| {
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        Fake::default(),
        seed_session(false),
    );
    assert!(runner.pass().await.unwrap().daily > 0);
    assert_eq!(runner.pass().await.unwrap().daily, 0);
});

db_test!(a_rolled_refresh_token_is_saved_before_use, |f| {
    let fake = Fake {
        activities: vec![act(1)],
        ..Default::default()
    };
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake,
        seed_session(true),
    );
    runner.pass().await.unwrap();
    let stored: Value =
        sqlx::query_scalar("SELECT session FROM ingest_session WHERE account_id = 'owner'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(
        stored.get("refresh_token").and_then(|v| v.as_str()),
        Some("r1")
    );
    assert_eq!(runner.session().access_token, "a1");
});

db_test!(the_display_name_is_resolved_and_persisted_once, |f| {
    let mut s = seed_session(false);
    s.garmin_user_id = None;
    let fake = Fake::default();
    let mut runner = Runner::new(f.pool.clone(), "owner".into(), 2000, fake, s);
    runner.pass().await.unwrap();
    assert_eq!(
        runner.session().garmin_user_id.as_deref(),
        Some("display-guid")
    );
    let stored: Value =
        sqlx::query_scalar("SELECT session FROM ingest_session WHERE account_id = 'owner'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(
        stored.get("garmin_user_id").and_then(|v| v.as_str()),
        Some("display-guid")
    );
});

db_test!(backfill_reports_budget_spent_when_the_budget_is_zero, |f| {
    let fake = Fake {
        activities: vec![act(1)],
        ..Default::default()
    };
    let mut runner = Runner::new(f.pool.clone(), "owner".into(), 0, fake, seed_session(false));
    assert_eq!(
        runner.backfill_step().await.unwrap(),
        garmin_ingest::runner::BackfillStep::BudgetSpent
    );
});

db_test!(backfill_progresses_while_there_is_work_and_budget, |f| {
    let fake = Fake {
        activities: vec![act(1)],
        ..Default::default()
    };
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake,
        seed_session(false),
    );
    assert_eq!(
        runner.backfill_step().await.unwrap(),
        garmin_ingest::runner::BackfillStep::Progressed
    );
});

db_test!(a_valid_token_is_not_refreshed, |f| {
    let fake = Fake {
        activities: vec![],
        ..Default::default()
    };
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake,
        seed_session(false),
    );
    runner.pass().await.unwrap();
    assert_eq!(runner.session().access_token, "a0");
});

db_test!(a_bad_fit_keeps_the_activity_and_blob, |f| {
    let fake = Fake {
        activities: vec![act(5)],
        bad_fit: true,
        ..Default::default()
    };
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake,
        seed_session(false),
    );
    runner.pass().await.unwrap();
    // Activity stored, file blob stored, but no samples.
    let has_act: bool = sqlx::query_scalar("SELECT exists(SELECT 1 FROM activity WHERE id = 5)")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(has_act);
    let has_file: bool =
        sqlx::query_scalar("SELECT exists(SELECT 1 FROM activity_file WHERE activity_id = 5)")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(has_file);
    let samples: i64 =
        sqlx::query_scalar("SELECT count(*) FROM activity_sample WHERE activity_id = 5")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(samples, 0);
});

db_test!(a_good_fit_produces_samples, |f| {
    let fake = Fake {
        activities: vec![act(6)],
        bad_fit: false,
        ..Default::default()
    };
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake,
        seed_session(false),
    );
    runner.pass().await.unwrap();
    let samples: i64 =
        sqlx::query_scalar("SELECT count(*) FROM activity_sample WHERE activity_id = 6")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(samples, 1);
});

db_test!(a_poll_skips_already_stored_activities, |f| {
    // First pass stores activity 7.
    let fake = Fake {
        activities: vec![act(7)],
        ..Default::default()
    };
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake,
        seed_session(false),
    );
    let r1 = runner.pass().await.unwrap();
    assert_eq!(r1.activities, 1);
    // A second pass sees the same activity (same start) and skips it.
    let fake2 = Fake {
        activities: vec![act(7)],
        ..Default::default()
    };
    let mut runner2 = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake2,
        seed_session(false),
    );
    let r2 = runner2.pass().await.unwrap();
    assert_eq!(r2.activities, 0);
});

db_test!(a_wellness_download_is_kept_with_its_readings, |f| {
    let fake = Fake {
        activities: vec![act_days_ago(1, 5)],
        ..Default::default()
    };
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake,
        seed_session(false),
    );
    assert_eq!(
        backfill_until_stopped(&mut runner).await,
        BackfillStep::Done
    );
    let files: i64 = sqlx::query_scalar("SELECT count(*) FROM wellness_file")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(files, 4, "one download per day back to the floor");
    let readings: Vec<(String, f64)> = sqlx::query_as("SELECT kind, value FROM wellness_sample")
        .fetch_all(&f.pool)
        .await
        .unwrap();
    // Every fake day holds the same reading, so it is stored once.
    assert_eq!(readings, vec![("heart_rate".to_owned(), 140.0)]);
});

db_test!(a_wellness_file_records_the_watch_that_wrote_it, |f| {
    let fake = Fake {
        activities: vec![act_days_ago(1, 2)],
        ..Default::default()
    };
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake,
        seed_session(false),
    );
    backfill_until_stopped(&mut runner).await;
    let devices: Vec<Option<i64>> =
        sqlx::query_scalar("SELECT DISTINCT device_id FROM wellness_file")
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert_eq!(devices, vec![Some(WATCH as i64)]);
    let sample: Option<i64> = sqlx::query_scalar("SELECT device_id FROM wellness_sample")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(sample, Some(WATCH as i64));
});

db_test!(a_poll_archives_the_registry_and_names_the_device, |f| {
    let mut on_watch = act(31);
    on_watch["deviceId"] = json!(WATCH);
    let fake = Fake {
        activities: vec![on_watch, act(32)],
        ..Default::default()
    };
    let mut runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        fake,
        seed_session(false),
    );
    runner.pass().await.unwrap();
    let names: Vec<(i64, Option<String>)> =
        garmin_core::store::activities::list(&f.pool, None, None, None, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|a| (a.id, a.device))
            .collect();
    assert!(
        names.contains(&(31, Some("test watch".to_owned()))),
        "{names:?}"
    );
    assert!(
        names.contains(&(32, None)),
        "no device stays unnamed: {names:?}"
    );
    let devices = garmin_core::store::devices::list(&f.pool).await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].product_sku.as_deref(), Some("010-1"));
});

db_test!(stored_wellness_days_are_linked_to_their_watch, |f| {
    // A day stored before devices were linked: no device on the file or its reading.
    sqlx::query("INSERT INTO wellness_file (date, bytes) VALUES ('2026-01-01', $1)")
        .bind(wellness_fit())
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO wellness_sample (kind, ts, value) \
         VALUES ('heart_rate', '2021-09-08T01:46:40Z', 140)",
    )
    .execute(&f.pool)
    .await
    .unwrap();
    let runner = Runner::new(
        f.pool.clone(),
        "owner".into(),
        2000,
        Fake::default(),
        seed_session(false),
    );
    assert_eq!(runner.relink_wellness().await.unwrap(), 1);
    assert_eq!(
        runner.relink_wellness().await.unwrap(),
        0,
        "nothing left to link"
    );
    let file: Option<i64> = sqlx::query_scalar("SELECT device_id FROM wellness_file")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    let sample: Vec<Option<i64>> = sqlx::query_scalar("SELECT device_id FROM wellness_sample")
        .fetch_all(&f.pool)
        .await
        .unwrap();
    assert_eq!(file, Some(WATCH as i64));
    assert_eq!(
        sample,
        vec![Some(WATCH as i64)],
        "the same reading, now linked"
    );
});
