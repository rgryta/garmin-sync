//! Runner tests against a fake Source and the test database: the rolled refresh token is
//! saved before use, a valid token is left alone, a bad FIT keeps the activity and blob,
//! and a poll ingests only new activities.

use chrono::{TimeDelta, Utc};
use garmin_client::{ClientError, Session};
use garmin_ingest::runner::Runner;
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
    let mut d = vec![0x40, 0x00, 0x00, 20, 0, 2, 253, 4, 0x86, 3, 1, 0x02];
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
    async fn daily(&self, _s: &Session, _kind: &str, _date: &str) -> Result<Value, ClientError> {
        Ok(json!({"ok": true}))
    }
}

fn act(id: i64) -> Value {
    json!({"activityId": id, "startTimeGMT": "2026-03-01 08:00:00", "activityType": {"typeKey": "running"}})
}

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
