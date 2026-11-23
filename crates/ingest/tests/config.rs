//! Config defaults. One test, its own process, so the env is touched from one place.

use garmin_ingest::config::IngestSettings;
use std::time::Duration;

#[test]
fn defaults_and_a_missing_dsn() {
    unsafe {
        std::env::set_var("GARMIN_DSN", "postgresql://u:p@h/garmin");
        std::env::set_var("GARMIN_ACCOUNT", "owner");
        std::env::remove_var("GARMIN_POLL_INTERVAL");
        std::env::remove_var("GARMIN_BACKFILL_BUDGET");
        std::env::remove_var("GARMIN_SESSION_FROM");
    }
    let s = IngestSettings::from_env().unwrap();
    assert_eq!(s.account, "owner");
    assert_eq!(s.poll_interval, Duration::from_secs(15 * 60));
    assert_eq!(s.backfill_budget, 2000);
    assert!(!format!("{:?}", s.dsn).contains("p@h"));
    unsafe { std::env::remove_var("GARMIN_DSN") };
    assert!(IngestSettings::from_env().is_err());
}
