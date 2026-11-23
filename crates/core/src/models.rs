//! Domain models: the shapes the store reads and writes.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// One activity (a workout). The thin typed fields are for listing/filtering; `summary`
/// holds Garmin's whole summary payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub start_ts: DateTime<Utc>,
    pub type_: Option<String>,
    pub name: Option<String>,
    pub manual: Option<bool>,
    pub duration_s: Option<f64>,
    pub distance_m: Option<f64>,
    pub device: Option<String>,
    pub summary: Value,
}

/// The outcome of upserting an activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    New,
    Updated,
    Unchanged,
}

/// One per-record sample parsed from a FIT file. `extra` holds any field without a column.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub ts: DateTime<Utc>,
    pub elapsed_s: Option<i32>,
    pub heart_rate: Option<i32>,
    pub cadence: Option<f64>,
    pub power: Option<i32>,
    pub speed: Option<f64>,
    pub altitude: Option<f64>,
    pub distance_m: Option<f64>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub temperature: Option<f64>,
    pub extra: Option<Value>,
}

/// How a trend is bucketed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Day,
    Week,
    Month,
}

impl Bucket {
    pub fn trunc(self) -> &'static str {
        match self {
            Bucket::Day => "day",
            Bucket::Week => "week",
            Bucket::Month => "month",
        }
    }
}
