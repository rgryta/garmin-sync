//! Normalize tests over synthetic activity payloads.

use garmin_ingest::normalize;
use serde_json::json;

#[test]
fn an_activity_summary_maps() {
    let a = json!({
        "activityId": 123456, "startTimeGMT": "2026-11-20 07:59:43",
        "activityType": {"typeKey": "running"}, "activityName": "Morning Run",
        "manualActivity": false, "duration": 1800.5, "distance": 5000.0, "deviceId": 99
    });
    let act = normalize::activity(&a).unwrap();
    assert_eq!(act.id, 123456);
    assert_eq!(act.type_.as_deref(), Some("running"));
    assert_eq!(act.manual, Some(false));
    assert_eq!(act.duration_s, Some(1800.5));
    assert_eq!(act.device.as_deref(), Some("99"));
    assert_eq!(act.start_ts.to_rfc3339(), "2026-11-20T07:59:43+00:00");
}

#[test]
fn a_manual_activity_is_flagged() {
    let a = json!({"activityId": 1, "startTimeGMT": "2026-01-01 00:00:00", "manualActivity": true});
    assert_eq!(normalize::activity(&a).unwrap().manual, Some(true));
}

#[test]
fn an_unknown_type_is_kept_whole() {
    let a = json!({"activityId": 2, "startTimeGMT": "2026-01-01 00:00:00", "activityType": {"typeKey": "yoga_flow_x"}, "extra": 7});
    let act = normalize::activity(&a).unwrap();
    assert_eq!(act.type_.as_deref(), Some("yoga_flow_x"));
    assert_eq!(act.summary.get("extra").and_then(|v| v.as_i64()), Some(7));
}

#[test]
fn a_missing_id_is_a_shape_error() {
    let a = json!({"startTimeGMT": "2026-01-01 00:00:00"});
    assert!(
        normalize::activity(&a)
            .unwrap_err()
            .0
            .contains("activityId")
    );
}
