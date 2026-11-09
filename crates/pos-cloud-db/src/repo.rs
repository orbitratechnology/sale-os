//! Tenant repositories (E16-T02). Each takes a [`TenantTx`]; row-level security keeps it to
//! the transaction's business, so a lookup by another business's id finds nothing.

use crate::tenant::TenantTx;
use pos_domain::{BusinessId, LocationId, RegisterId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewBusiness {
    pub business_id: BusinessId,
    pub name: String,
    pub country_code: String,
    pub timezone: String,
    pub currency_code: String,
    pub currency_exponent: i16,
    pub prices_include_tax: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct BusinessRow {
    pub business_id: uuid::Uuid,
    pub name: String,
    pub currency_code: String,
    pub version: i64,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct RegisterRow {
    pub register_id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub name: String,
    pub code: String,
    pub status: String,
    pub version: i64,
}

/// The transaction's own business.
pub async fn business(tx: &mut TenantTx<'_>) -> Result<Option<BusinessRow>, sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    sqlx::query_as(
        "SELECT business_id, name, currency_code, version, status FROM businesses WHERE business_id = $1",
    )
    .bind(id)
    .fetch_optional(tx.conn())
    .await
}

/// Creates the transaction's business (its id must be the transaction's).
pub async fn insert_business(tx: &mut TenantTx<'_>, b: &NewBusiness) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO businesses (business_id, name, country_code, timezone, currency_code, currency_exponent,
           prices_include_tax) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(*b.business_id.as_uuid())
    .bind(&b.name)
    .bind(&b.country_code)
    .bind(&b.timezone)
    .bind(&b.currency_code)
    .bind(b.currency_exponent)
    .bind(b.prices_include_tax)
    .execute(tx.conn())
    .await?;
    Ok(())
}

pub async fn insert_location(tx: &mut TenantTx<'_>, id: LocationId, name: &str) -> Result<(), sqlx::Error> {
    let business = *tx.business_id().as_uuid();
    sqlx::query(
        "INSERT INTO locations (business_id, location_id, name, status, version, updated_at)
         VALUES ($1, $2, $3, 'active', 1, now())",
    )
    .bind(business)
    .bind(*id.as_uuid())
    .bind(name)
    .execute(tx.conn())
    .await?;
    Ok(())
}

pub async fn insert_register(
    tx: &mut TenantTx<'_>,
    id: RegisterId,
    location: LocationId,
    name: &str,
    code: &str,
) -> Result<(), sqlx::Error> {
    let business = *tx.business_id().as_uuid();
    sqlx::query(
        "INSERT INTO registers (business_id, register_id, location_id, name, code, status, version, updated_at)
         VALUES ($1, $2, $3, $4, $5, 'active', 1, now())",
    )
    .bind(business)
    .bind(*id.as_uuid())
    .bind(*location.as_uuid())
    .bind(name)
    .bind(code)
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// A register of the transaction's business; another business's id finds nothing.
pub async fn register(tx: &mut TenantTx<'_>, id: RegisterId) -> Result<Option<RegisterRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT register_id, location_id, name, code, status, version FROM registers WHERE register_id = $1",
    )
    .bind(*id.as_uuid())
    .fetch_optional(tx.conn())
    .await
}

pub async fn registers(tx: &mut TenantTx<'_>) -> Result<Vec<RegisterRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT register_id, location_id, name, code, status, version FROM registers ORDER BY code",
    )
    .fetch_all(tx.conn())
    .await
}
