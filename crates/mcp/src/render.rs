//! Rendering: takes rows, does not fetch them.

use garmin_core::store::activities::ActivityRow;
use garmin_core::store::daily::DailyRow;
use garmin_core::store::devices::DeviceRow;
use garmin_core::store::samples::SampleSummary;
use garmin_core::store::sql::SqlResult;
use garmin_core::store::sync::StatusRow;

pub fn status(loops: &[StatusRow], kinds: &[(String, i64)], devices: &[DeviceRow]) -> String {
    let mut out = String::from("Garmin archive status\n");
    for l in loops {
        out.push_str(&format!(
            "  loop {}: {}{}{}\n",
            l.loop_name,
            l.last_ok_at
                .map(|t| format!("last ok {}", t.to_rfc3339()))
                .unwrap_or_else(|| "no run yet".into()),
            l.budget_day
                .map(|d| format!(", budget {} on {}", l.budget_spent, d))
                .unwrap_or_default(),
            l.last_error
                .as_ref()
                .map(|e| format!(", last error: {e}"))
                .unwrap_or_default(),
        ));
    }
    out.push_str("Daily metrics held:\n");
    for (k, n) in kinds {
        out.push_str(&format!("  {k}: {n} days\n"));
    }
    out.push_str("Registered devices:\n");
    for d in devices {
        out.push_str(&format!(
            "  {} (id {}{}{})\n",
            d.display_name.as_deref().unwrap_or("unnamed"),
            d.id,
            d.product_sku
                .as_deref()
                .map(|s| format!(", SKU {s}"))
                .unwrap_or_default(),
            d.registered_at
                .map(|t| format!(", registered {}", t.date_naive()))
                .unwrap_or_default(),
        ));
    }
    out
}

pub fn activities(rows: &[ActivityRow]) -> String {
    if rows.is_empty() {
        return "No activities for that query.".to_owned();
    }
    let mut out = String::from("Activities (newest first)\n");
    for a in rows {
        out.push_str(&format!(
            "  [{}] {} — {}{}{}{}{}\n",
            a.id,
            a.start_ts.to_rfc3339(),
            a.type_.as_deref().unwrap_or("activity"),
            a.name
                .as_deref()
                .map(|n| format!(" \"{n}\""))
                .unwrap_or_default(),
            a.distance_m
                .map(|d| format!(", {:.2} km", d / 1000.0))
                .unwrap_or_default(),
            if a.manual == Some(true) {
                " (manual)"
            } else {
                ""
            },
            a.device
                .as_deref()
                .map(|d| format!(" · {d}"))
                .unwrap_or_default(),
        ));
    }
    out
}

pub fn activity(id: i64, summary: &serde_json::Value, s: &SampleSummary) -> String {
    let mut out = format!("Activity {id}\n");
    out.push_str(&format!("  summary: {}\n", summary));
    out.push_str(&format!(
        "  samples: {} points{}{}{}\n",
        s.count,
        s.avg_heart_rate
            .map(|h| format!(", avg HR {h:.0}"))
            .unwrap_or_default(),
        s.max_heart_rate
            .map(|h| format!(", max HR {h}"))
            .unwrap_or_default(),
        s.max_altitude
            .map(|a| format!(", max alt {a:.0} m"))
            .unwrap_or_default(),
    ));
    out
}

pub fn day(date: &str, rows: &[DailyRow]) -> String {
    if rows.is_empty() {
        return format!("No metrics archived for {date}.");
    }
    let mut out = format!("Metrics for {date}\n");
    for r in rows {
        out.push_str(&format!("  {}: {}\n", r.kind, r.raw));
    }
    out
}

pub fn metric(kind: &str, rows: &[DailyRow]) -> String {
    if rows.is_empty() {
        return format!("No {kind} data in that range.");
    }
    let mut out = format!("{kind} by day\n");
    for r in rows {
        out.push_str(&format!("  {}: {}\n", r.date, r.raw));
    }
    out
}

pub fn sql(result: &SqlResult) -> String {
    if result.rows.is_empty() {
        return "0 rows.".to_owned();
    }
    let mut out = String::new();
    for row in &result.rows {
        out.push_str(&serde_json::to_string(row).unwrap_or_default());
        out.push('\n');
        if out.len() > 100_000 {
            out.push_str("… output truncated at 100 KB\n");
            return out;
        }
    }
    if result.truncated {
        out.push_str("… more rows; narrow the query or add LIMIT\n");
    }
    out
}
