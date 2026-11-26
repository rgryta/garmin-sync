//! Turn Garmin payloads into archive rows. Pure functions over `serde_json::Value`.

use chrono::{DateTime, NaiveDateTime, Utc};
use garmin_core::models::{Activity, Device};
use serde_json::Value;

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("payload is missing or malformed: {0}")]
pub struct ShapeError(pub String);

fn require_i64(v: &Value, key: &str) -> Result<i64, ShapeError> {
    v.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| ShapeError(format!("missing i64 field {key}")))
}

/// Garmin stamps `startTimeGMT` as `YYYY-MM-DD HH:MM:SS` in UTC (no offset).
fn parse_gmt(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    let s = v.get(key).and_then(Value::as_str)?;
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|t| t.and_utc())
}

/// One activity summary from the activity list / detail.
pub fn activity(v: &Value) -> Result<Activity, ShapeError> {
    let id = require_i64(v, "activityId")?;
    let start_ts = parse_gmt(v, "startTimeGMT")
        .ok_or_else(|| ShapeError(format!("activity {id} has no startTimeGMT")))?;
    let type_ = v
        .get("activityType")
        .and_then(|t| t.get("typeKey"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(Activity {
        id,
        parent_id: v.get("parentId").and_then(Value::as_i64),
        start_ts,
        type_,
        name: v
            .get("activityName")
            .and_then(Value::as_str)
            .map(str::to_owned),
        manual: v.get("manualActivity").and_then(Value::as_bool),
        duration_s: v.get("duration").and_then(Value::as_f64),
        distance_m: v.get("distance").and_then(Value::as_f64),
        device: v
            .get("deviceId")
            .and_then(Value::as_i64)
            .map(|n| n.to_string())
            .or_else(|| {
                v.get("deviceName")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
        summary: v.clone(),
    })
}

/// One device from the registry (`device-service/deviceregistration/devices`); `None`
/// without a deviceId.
pub fn device(v: &Value) -> Option<Device> {
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let id = v.get("deviceId")?;
    Some(Device {
        id: id.as_i64().or_else(|| id.as_str()?.parse().ok())?,
        display_name: text("productDisplayName").or_else(|| text("displayName")),
        product_sku: text("productSku"),
        part_number: text("partNumber"),
        application_key: text("applicationKey"),
        registered_at: v
            .get("registeredDate")
            .and_then(Value::as_i64)
            .and_then(DateTime::from_timestamp_millis),
    })
}
