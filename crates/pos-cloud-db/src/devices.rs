//! Businesses an owner belongs to, registers, devices and refresh-token rotation (E16-T04).

use crate::tenant::{AccountTx, TenantTx};
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct BusinessFull {
    pub business_id: Uuid,
    pub name: String,
    pub country_code: String,
    pub timezone: String,
    pub currency_code: String,
    pub currency_exponent: i16,
    pub prices_include_tax: bool,
    pub version: i64,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct DeviceRow {
    pub business_id: Uuid,
    pub device_id: Uuid,
    pub register_id: Uuid,
    pub app_version: String,
    pub os_info: Option<String>,
    pub status: String,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

const BUSINESS_COLUMNS: &str = "business_id, name, country_code::text AS country_code, timezone, currency_code::text AS currency_code, \
     currency_exponent, prices_include_tax, version, status";
const DEVICE_COLUMNS: &str = "business_id, device_id, register_id, app_version, os_info, status, registered_at, last_seen_at, revoked_at";

// ---------- across an owner's businesses (read only) ----------

pub async fn member_businesses(tx: &mut AccountTx<'_>) -> Result<Vec<BusinessFull>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {BUSINESS_COLUMNS} FROM businesses ORDER BY name"
    ))
    .fetch_all(tx.conn())
    .await
}

pub async fn member_devices(tx: &mut AccountTx<'_>) -> Result<Vec<DeviceRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {DEVICE_COLUMNS} FROM devices ORDER BY registered_at"
    ))
    .fetch_all(tx.conn())
    .await
}

/// The business of a device the owner can see; `None` for anyone else's device.
pub async fn device_business(tx: &mut AccountTx<'_>, device_id: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT business_id FROM devices WHERE device_id = $1")
        .bind(device_id)
        .fetch_optional(tx.conn())
        .await
}

// ---------- inside one business ----------

pub async fn business_full(tx: &mut TenantTx<'_>) -> Result<Option<BusinessFull>, sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    sqlx::query_as(&format!(
        "SELECT {BUSINESS_COLUMNS} FROM businesses WHERE business_id = $1"
    ))
    .bind(id)
    .fetch_optional(tx.conn())
    .await
}

pub async fn is_member(tx: &mut TenantTx<'_>, account_id: Uuid) -> Result<bool, sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM business_members WHERE business_id = $1 AND account_id = $2)",
    )
    .bind(id)
    .bind(account_id)
    .fetch_one(tx.conn())
    .await
}

pub async fn insert_owner(tx: &mut TenantTx<'_>, account_id: Uuid) -> Result<(), sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    sqlx::query("INSERT INTO business_members (business_id, account_id, role) VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING")
        .bind(id)
        .bind(account_id)
        .execute(tx.conn())
        .await?;
    Ok(())
}

/// Creates the location and the register if they do not exist yet (the desktop's ids).
pub async fn ensure_register(
    tx: &mut TenantTx<'_>,
    location_id: Uuid,
    location_name: &str,
    register_id: Uuid,
    register_name: &str,
    register_code: &str,
) -> Result<(), sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    sqlx::query(
        "INSERT INTO locations (business_id, location_id, name, status, version, updated_at)
         VALUES ($1, $2, $3, 'active', 1, now()) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(location_id)
    .bind(location_name)
    .execute(tx.conn())
    .await?;
    sqlx::query(
        "INSERT INTO registers (business_id, register_id, location_id, name, code, status, version, updated_at)
         VALUES ($1, $2, $3, $4, $5, 'active', 1, now()) ON CONFLICT (business_id, register_id) DO NOTHING",
    )
    .bind(id)
    .bind(register_id)
    .bind(location_id)
    .bind(register_name)
    .bind(register_code)
    .execute(tx.conn())
    .await?;
    Ok(())
}

pub async fn active_device(tx: &mut TenantTx<'_>, register_id: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT device_id FROM devices WHERE register_id = $1 AND status = 'active'")
        .bind(register_id)
        .fetch_optional(tx.conn())
        .await
}

/// Revokes a device and spends all its refresh tokens.
pub async fn revoke_device(tx: &mut TenantTx<'_>, device_id: Uuid) -> Result<bool, sqlx::Error> {
    let n = sqlx::query("UPDATE devices SET status = 'revoked', revoked_at = now() WHERE device_id = $1 AND status = 'active'")
        .bind(device_id)
        .execute(tx.conn())
        .await?
        .rows_affected();
    sqlx::query("UPDATE device_refresh_tokens SET used_at = coalesce(used_at, now()) WHERE device_id = $1")
        .bind(device_id)
        .execute(tx.conn())
        .await?;
    Ok(n > 0)
}

pub struct NewDevice<'a> {
    pub device_id: Uuid,
    pub register_id: Uuid,
    pub installation_id: Uuid,
    pub app_version: &'a str,
    pub os_info: Option<&'a str>,
    pub registered_by: Uuid,
}

pub async fn insert_device(tx: &mut TenantTx<'_>, d: &NewDevice<'_>) -> Result<(), sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    sqlx::query(
        "INSERT INTO devices (business_id, device_id, register_id, installation_id, app_version, os_info, registered_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(d.device_id)
    .bind(d.register_id)
    .bind(d.installation_id)
    .bind(d.app_version)
    .bind(d.os_info)
    .bind(d.registered_by)
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// Stores a refresh token's hash (never the token), valid for `days`.
pub async fn insert_refresh_token(
    tx: &mut TenantTx<'_>,
    hash: &[u8],
    device_id: Uuid,
    family_id: Uuid,
    days: i32,
) -> Result<(), sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    sqlx::query(
        "INSERT INTO device_refresh_tokens (token_hash, business_id, device_id, family_id, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(days => $5))",
    )
    .bind(hash)
    .bind(id)
    .bind(device_id)
    .bind(family_id)
    .bind(days)
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// The device's status (`active`/`revoked`), marking it seen. `None`: no such device here.
pub async fn device_seen(tx: &mut TenantTx<'_>, device_id: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("UPDATE devices SET last_seen_at = now() WHERE device_id = $1 RETURNING status")
        .bind(device_id)
        .fetch_optional(tx.conn())
        .await
}

// ---------- refresh-token rotation ----------

/// What became of a refresh request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rotation {
    /// A new token was stored for the device; the old one is spent.
    Rotated {
        business_id: Uuid,
        register_id: Uuid,
        device_id: Uuid,
    },
    /// The token was already used: the device is now revoked (ADR-013).
    Reused,
    /// The device was revoked earlier.
    Revoked,
    Expired,
    Unknown,
}

/// Spends the token whose hash is `hash` and stores `new_hash` in its family, in one
/// transaction (the row is locked, so two refreshes with one token cannot both succeed).
pub async fn rotate(
    pool: &sqlx::PgPool,
    hash: &[u8],
    new_hash: &[u8],
    days: i32,
) -> Result<Rotation, sqlx::Error> {
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL ROLE pos_app").execute(&mut *tx).await?;
    sqlx::query("SELECT set_config('app.refresh_token_hash', $1, true)")
        .bind(&hex)
        .execute(&mut *tx)
        .await?;
    let Some(business): Option<Uuid> =
        sqlx::query_scalar("SELECT business_id FROM device_refresh_tokens WHERE token_hash = $1")
            .bind(hash)
            .fetch_optional(&mut *tx)
            .await?
    else {
        return Ok(Rotation::Unknown);
    };
    // From here on, inside that business only.
    sqlx::query(
        "SELECT set_config('app.refresh_token_hash', '', true), set_config('app.business_id', $1, true)",
    )
    .bind(business.to_string())
    .execute(&mut *tx)
    .await?;
    let (device, family, used, expired): (Uuid, Uuid, bool, bool) = sqlx::query_as(
        "SELECT device_id, family_id, used_at IS NOT NULL, expires_at <= now()
           FROM device_refresh_tokens WHERE token_hash = $1 FOR UPDATE",
    )
    .bind(hash)
    .fetch_one(&mut *tx)
    .await?;
    let (status, register): (String, Uuid) =
        sqlx::query_as("SELECT status, register_id FROM devices WHERE device_id = $1 FOR UPDATE")
            .bind(device)
            .fetch_one(&mut *tx)
            .await?;
    let outcome = if status != "active" {
        Rotation::Revoked
    } else if used {
        // A spent token came back: someone has a copy. Cut the device off.
        sqlx::query("UPDATE devices SET status = 'revoked', revoked_at = now() WHERE device_id = $1")
            .bind(device)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE device_refresh_tokens SET used_at = coalesce(used_at, now()) WHERE family_id = $1",
        )
        .bind(family)
        .execute(&mut *tx)
        .await?;
        Rotation::Reused
    } else if expired {
        Rotation::Expired
    } else {
        sqlx::query("UPDATE device_refresh_tokens SET used_at = now() WHERE token_hash = $1")
            .bind(hash)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO device_refresh_tokens (token_hash, business_id, device_id, family_id, expires_at)
             VALUES ($1, $2, $3, $4, now() + make_interval(days => $5))",
        )
        .bind(new_hash)
        .bind(business)
        .bind(device)
        .bind(family)
        .bind(days)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE devices SET last_seen_at = now() WHERE device_id = $1")
            .bind(device)
            .execute(&mut *tx)
            .await?;
        Rotation::Rotated {
            business_id: business,
            register_id: register,
            device_id: device,
        }
    };
    tx.commit().await?;
    Ok(outcome)
}
