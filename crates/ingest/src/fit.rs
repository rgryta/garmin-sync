//! Parse a FIT activity file into per-second samples. Lives in the ingester so it builds
//! `garmin_core::models::Sample` directly. A parse error returns `FitError` — the caller
//! keeps the activity and the FIT blob and only skips the samples.

use chrono::{DateTime, Utc};
use fitparser::{FitDataField, Value as FitValue};
use garmin_core::models::Sample;
use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
#[error("FIT parse error: {0}")]
pub struct FitError(pub String);

/// Semicircles -> degrees (FIT stores GPS as i32 semicircles).
fn semicircles_to_deg(v: f64) -> f64 {
    v * (180.0 / 2147483648.0)
}

fn num(v: &FitValue) -> Option<f64> {
    match v {
        FitValue::SInt8(n) => Some(*n as f64),
        FitValue::UInt8(n) | FitValue::UInt8z(n) => Some(*n as f64),
        FitValue::SInt16(n) => Some(*n as f64),
        FitValue::UInt16(n) | FitValue::UInt16z(n) => Some(*n as f64),
        FitValue::SInt32(n) => Some(*n as f64),
        FitValue::UInt32(n) | FitValue::UInt32z(n) => Some(*n as f64),
        FitValue::SInt64(n) => Some(*n as f64),
        FitValue::UInt64(n) | FitValue::UInt64z(n) => Some(*n as f64),
        FitValue::Float32(n) => Some(*n as f64),
        FitValue::Float64(n) => Some(*n),
        _ => None,
    }
}

/// Garmin's download endpoint wraps the original file in a zip. Return the format to store
/// and the bytes to store: the inner `.fit` when the download is a zip containing one
/// (parseable into samples), the raw zip (format `zip`) when it holds no FIT, or the input
/// unchanged when it is already a bare FIT.
pub fn unwrap_download(bytes: Vec<u8>) -> (String, Vec<u8>) {
    if !bytes.starts_with(b"PK\x03\x04") {
        return ("fit".into(), bytes);
    }
    if let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(&bytes)) {
        for i in 0..zip.len() {
            let Ok(mut entry) = zip.by_index(i) else {
                continue;
            };
            if entry
                .name()
                .is_ok_and(|name| name.to_lowercase().ends_with(".fit"))
            {
                let mut out = Vec::new();
                if std::io::copy(&mut entry, &mut out).is_ok() {
                    return ("fit".into(), out);
                }
            }
        }
    }
    ("zip".into(), bytes)
}

/// Parse the `record` messages into samples. Known fields get columns; everything else is
/// kept in `extra` so nothing the device recorded is lost.
pub fn parse_samples(bytes: &[u8]) -> Result<Vec<Sample>, FitError> {
    let records = fitparser::from_bytes(bytes).map_err(|e| FitError(e.to_string()))?;
    let mut out = Vec::new();
    for rec in records {
        if rec.kind() != fitparser::profile::MesgNum::Record {
            continue;
        }
        let fields = rec.fields();
        let Some(ts) = timestamp(fields) else {
            continue;
        };
        let mut s = Sample {
            ts,
            elapsed_s: None,
            heart_rate: None,
            cadence: None,
            power: None,
            speed: None,
            altitude: None,
            distance_m: None,
            lat: None,
            lon: None,
            temperature: None,
            extra: None,
        };
        let mut extra = Map::new();
        for f in fields {
            match f.name() {
                "timestamp" => {}
                "heart_rate" => s.heart_rate = num(f.value()).map(|n| n as i32),
                "cadence" => s.cadence = num(f.value()),
                "power" => s.power = num(f.value()).map(|n| n as i32),
                "enhanced_speed" | "speed" => {
                    if s.speed.is_none() {
                        s.speed = num(f.value())
                    }
                }
                "enhanced_altitude" | "altitude" => {
                    if s.altitude.is_none() {
                        s.altitude = num(f.value())
                    }
                }
                "distance" => s.distance_m = num(f.value()),
                "position_lat" => s.lat = num(f.value()).map(semicircles_to_deg),
                "position_long" => s.lon = num(f.value()).map(semicircles_to_deg),
                "temperature" => s.temperature = num(f.value()),
                other => {
                    if let Some(n) = num(f.value()) {
                        extra.insert(other.to_owned(), Value::from(n));
                    }
                }
            }
        }
        if !extra.is_empty() {
            s.extra = Some(Value::Object(extra));
        }
        out.push(s);
    }
    Ok(out)
}

fn timestamp(fields: &[FitDataField]) -> Option<DateTime<Utc>> {
    fields
        .iter()
        .find(|f| f.name() == "timestamp")
        .and_then(|f| {
            if let FitValue::Timestamp(t) = f.value() {
                Some(t.with_timezone(&Utc))
            } else {
                None
            }
        })
}
