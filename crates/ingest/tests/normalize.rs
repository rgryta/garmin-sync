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

#[test]
fn a_registered_device_maps() {
    let d = json!({
        "deviceId": 1234567890u64, "productDisplayName": "quatix 7 Sapphire",
        "displayName": "my watch", "productSku": "010-02582-61", "partNumber": "006-B3943-00",
        "applicationKey": "quatix7OLED", "registeredDate": 1721812308000i64
    });
    let dev = normalize::device(&d).unwrap();
    assert_eq!(dev.id, 1234567890);
    assert_eq!(dev.display_name.as_deref(), Some("quatix 7 Sapphire"));
    assert_eq!(dev.product_sku.as_deref(), Some("010-02582-61"));
    assert_eq!(
        dev.registered_at.unwrap().to_rfc3339(),
        "2024-07-24T09:11:48+00:00"
    );
    assert!(normalize::device(&json!({"displayName": "no id"})).is_none());
}
