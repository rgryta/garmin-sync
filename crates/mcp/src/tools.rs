//! The tool layer: resolves arguments, calls the store, hands rows to the renderer. No
//! SQL of its own — every query lives in `garmin_core::store`.

use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use garmin_core::store::{activities, daily, samples, sql, sync};
use sqlx::postgres::PgPool;

use crate::render;

#[derive(Debug)]
pub enum ToolError {
    Usage(String),
    Sql(sqlx::Error),
}

impl From<sqlx::Error> for ToolError {
    fn from(e: sqlx::Error) -> Self {
        ToolError::Sql(e)
    }
}

type R = Result<String, ToolError>;

fn parse_when(value: Option<&str>) -> Result<Option<DateTime<Utc>>, ToolError> {
    let Some(raw) = value.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(dt.with_timezone(&Utc)));
    }
    if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(Some(d.and_hms_opt(0, 0, 0).unwrap().and_utc()));
    }
    Err(ToolError::Usage(format!(
        "not a time I can read (ISO 8601 or YYYY-MM-DD): {raw:?}"
    )))
}

fn parse_date(value: &str) -> Result<NaiveDate, ToolError> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| ToolError::Usage(format!("not a date (YYYY-MM-DD): {value:?}")))
}

fn clamp(limit: Option<i64>, default: i64, ceiling: i64) -> i64 {
    limit.unwrap_or(default).clamp(1, ceiling)
}

pub async fn status(pool: &PgPool) -> R {
    let loops = sync::status(pool).await?;
    let kinds = daily::kinds(pool).await?;
    Ok(render::status(&loops, &kinds))
}

pub async fn activities_tool(
    pool: &PgPool,
    after: Option<&str>,
    before: Option<&str>,
    type_: Option<&str>,
    limit: Option<i64>,
) -> R {
    let rows = activities::list(
        pool,
        parse_when(after)?,
        parse_when(before)?,
        type_,
        clamp(limit, 20, 200),
    )
    .await?;
    Ok(render::activities(&rows))
}

pub async fn activity_tool(pool: &PgPool, id: i64) -> R {
    let Some(summary) = activities::get(pool, id).await? else {
        return Ok(format!("No activity {id} archived."));
    };
    let samples = samples::summary(pool, id).await?;
    Ok(render::activity(id, &summary, &samples))
}

pub async fn day_tool(pool: &PgPool, date: &str) -> R {
    let rows = daily::day(pool, parse_date(date)?).await?;
    Ok(render::day(date, &rows))
}

pub async fn metric_tool(
    pool: &PgPool,
    kind: &str,
    after: Option<&str>,
    before: Option<&str>,
) -> R {
    let after = after.map(parse_date).transpose()?;
    let before = before.map(parse_date).transpose()?;
    let rows = daily::series(pool, kind, after, before, 400).await?;
    Ok(render::metric(kind, &rows))
}

pub async fn sql_tool(pool: &PgPool, query: &str) -> R {
    let q = query.trim();
    if q.is_empty() {
        return Err(ToolError::Usage("the query is empty".to_owned()));
    }
    match sql::run_readonly(pool, q, 500, Duration::from_secs(10)).await {
        Ok(result) => Ok(render::sql(&result)),
        Err(e) => Err(ToolError::Usage(format!("query failed: {e}"))),
    }
}
