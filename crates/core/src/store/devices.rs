//! The account's registered devices.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, postgres::PgPool};

use crate::models::Device;

pub async fn upsert(conn: &mut PgConnection, d: &Device, raw: &Value) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO device (id, display_name, product_sku, part_number, application_key, \
           registered_at, raw, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, now()) \
         ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name, \
           product_sku = EXCLUDED.product_sku, part_number = EXCLUDED.part_number, \
           application_key = EXCLUDED.application_key, registered_at = EXCLUDED.registered_at, \
           raw = EXCLUDED.raw, updated_at = now()",
    )
    .bind(d.id)
    .bind(&d.display_name)
    .bind(&d.product_sku)
    .bind(&d.part_number)
    .bind(&d.application_key)
    .bind(d.registered_at)
    .bind(raw)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct DeviceRow {
    pub id: i64,
    pub display_name: Option<String>,
    pub product_sku: Option<String>,
    pub registered_at: Option<DateTime<Utc>>,
}

/// Every registered device, oldest registration first, for `status`.
pub async fn list(pool: &PgPool) -> sqlx::Result<Vec<DeviceRow>> {
    sqlx::query_as(
        "SELECT id, display_name, product_sku, registered_at FROM device \
         ORDER BY registered_at NULLS LAST, id",
    )
    .fetch_all(pool)
    .await
}
