//! Master-data appliers (E17-T06, SYN-010, ADR-011): business, location, register, settings,
//! staff, tax rates and categories, categories and products. An edit carries the version it
//! was based on; it applies only when the cloud is still at that version, which then moves
//! to the next one. Otherwise it is a `conflict` carrying the cloud's version and snapshot,
//! and nothing changes (the register settles it, E18). A barcode or SKU that another active
//! product already holds is a conflict too (`BARCODE_TAKEN`, `SKU_TAKEN`), with that product.
//! The same rules serve edits made in the cloud (`POST /v1/catalog/products`).

use super::{Appliers, ApplyError, ApplyOutcome, EventApplier};
use crate::devices::DevicePrincipal;
use async_trait::async_trait;
use pos_cloud_db::TenantTx;
use pos_cloud_db::master::{self as db, Kind};
use pos_cloud_db::sync::{NewChange, append_change};
use pos_sync::events::ProductUpsertedV1;
use pos_sync::{EventEnvelope, Payload, codes};
use serde_json::Value;
use std::ops::RangeInclusive;
use std::sync::Arc;
use uuid::Uuid;

/// Adds every master-data applier to `appliers`.
pub fn register(appliers: Appliers) -> Appliers {
    pos_sync::CATALOGUE
        .iter()
        .filter(|(t, _)| pos_sync::events::is_master(t))
        .fold(appliers, |a, (t, _)| a.with(Arc::new(Master(t))))
}

fn invalid(detail: impl Into<String>) -> ApplyError {
    ApplyError::Rejected {
        code: codes::EVENT_INVALID,
        detail: detail.into(),
    }
}

fn db_err(e: sqlx::Error) -> ApplyError {
    if let sqlx::Error::Database(d) = &e
        && matches!(d.code().as_deref(), Some("23505" | "23503" | "23514"))
    {
        return invalid(format!(
            "the change breaks a rule of the cloud data: {}",
            d.message()
        ));
    }
    ApplyError::from(e)
}

/// Who made an edit: a register (with the event that carried it), or the cloud.
#[derive(Copy, Clone, Debug)]
pub struct Origin {
    pub register: Option<Uuid>,
    pub event: Option<Uuid>,
}

/// What a payload is: its aggregate, id, the version it moves to, whether it archives, and
/// the business it names (if it names one).
fn describe(p: &Payload) -> Option<(Kind, String, u64, bool, Option<Uuid>)> {
    Some(match p {
        Payload::BusinessUpdatedV1(x) => (
            Kind::Business,
            x.business_id.to_string(),
            x.version,
            false,
            Some(x.business_id),
        ),
        Payload::LocationUpsertedV1(x) => (
            Kind::Location,
            x.location_id.to_string(),
            x.version,
            x.archived,
            Some(x.business_id),
        ),
        Payload::RegisterUpsertedV1(x) => (
            Kind::Register,
            x.register_id.to_string(),
            x.version,
            x.retired,
            Some(x.business_id),
        ),
        Payload::SettingUpdatedV1(x) => (
            Kind::Setting,
            x.key.clone(),
            x.version,
            false,
            Some(x.business_id),
        ),
        Payload::StaffUpsertedV1(x) => (
            Kind::Staff,
            x.user_id.to_string(),
            x.version,
            !x.active,
            Some(x.business_id),
        ),
        Payload::TaxRateUpsertedV1(x) => (
            Kind::TaxRate,
            x.tax_rate_id.to_string(),
            x.version,
            x.archived,
            Some(x.business_id),
        ),
        Payload::TaxCategoryUpsertedV1(x) => (
            Kind::TaxCategory,
            x.tax_category_id.to_string(),
            x.version,
            x.archived,
            Some(x.business_id),
        ),
        Payload::CategoryUpsertedV1(x) => (
            Kind::Category,
            x.category_id.to_string(),
            x.version,
            x.archived,
            Some(x.business_id),
        ),
        Payload::ProductUpsertedV1(x) => (Kind::Product, x.id.to_string(), x.version, x.archived, None),
        _ => return None,
    })
}

/// A conflict with `owner`'s current state (another product holding a barcode or SKU).
async fn taken(tx: &mut TenantTx<'_>, owner: Uuid, reason: &'static str) -> Result<ApplyOutcome, ApplyError> {
    let (version, snapshot) = db::current(tx, Kind::Product, &owner.to_string())
        .await
        .map_err(db_err)?
        .unwrap_or((0, Value::Null));
    Ok(ApplyOutcome::Conflict {
        server_version: u64::try_from(version).unwrap_or(0),
        server_snapshot: snapshot,
        reason,
    })
}

/// The first barcode or SKU of `p` that another active product holds.
async fn uniqueness(
    tx: &mut TenantTx<'_>,
    p: &ProductUpsertedV1,
) -> Result<Option<ApplyOutcome>, ApplyError> {
    if p.archived {
        return Ok(None);
    }
    for v in p.variants.iter().filter(|v| !v.archived) {
        for c in &v.barcodes {
            if let Some(owner) = db::barcode_owner(tx, &c.value, p.id).await.map_err(db_err)? {
                return taken(tx, owner, codes::BARCODE_TAKEN).await.map(Some);
            }
        }
        if let Some(sku) = &v.sku
            && let Some(owner) = db::sku_owner(tx, sku, p.id).await.map_err(db_err)?
        {
            return taken(tx, owner, codes::SKU_TAKEN).await.map(Some);
        }
    }
    Ok(None)
}

/// Applies one master-data edit based on `base`: the version check, the write and the
/// change-feed row. The caller holds nothing; this takes the business's master-data lock.
pub async fn upsert(
    tx: &mut TenantTx<'_>,
    origin: Origin,
    aggregate_type: &str,
    aggregate_id: &str,
    base: u64,
    payload: &Payload,
    snapshot: &Value,
) -> Result<ApplyOutcome, ApplyError> {
    let Some((kind, id, new_version, archived, names_business)) = describe(payload) else {
        return Err(invalid("not a master-data event"));
    };
    if id != aggregate_id {
        return Err(invalid("the envelope's aggregateId is not the aggregate's id"));
    }
    if names_business.is_some_and(|b| b != *tx.business_id().as_uuid()) {
        return Err(invalid("the change is for another business"));
    }
    if new_version != base + 1 {
        return Err(invalid(format!(
            "version {new_version} does not follow base version {base}"
        )));
    }
    db::lock(tx).await.map_err(db_err)?;
    let (server_version, server_snapshot) = db::current(tx, kind, &id)
        .await
        .map_err(db_err)?
        .unwrap_or((0, Value::Null));
    let server_version = u64::try_from(server_version).unwrap_or(0);
    if server_version != base {
        return Ok(ApplyOutcome::Conflict {
            server_version,
            server_snapshot,
            reason: codes::VERSION,
        });
    }
    match payload {
        Payload::BusinessUpdatedV1(p) => db::update_business(tx, p, snapshot).await,
        Payload::LocationUpsertedV1(p) => db::upsert_location(tx, p, snapshot).await,
        Payload::RegisterUpsertedV1(p) => {
            if origin.register.is_some_and(|r| r != p.register_id) {
                return Err(invalid("a register may only describe itself"));
            }
            if server_version == 0 && db::current(tx, kind, &id).await.map_err(db_err)?.is_none() {
                return Err(invalid("registers are created by linking a device"));
            }
            db::update_register(tx, p, snapshot).await
        }
        Payload::SettingUpdatedV1(p) => db::upsert_setting(tx, p, snapshot).await,
        Payload::StaffUpsertedV1(p) => db::upsert_staff(tx, p, snapshot).await,
        Payload::TaxRateUpsertedV1(p) => db::upsert_tax_rate(tx, p, snapshot).await,
        Payload::TaxCategoryUpsertedV1(p) => db::upsert_tax_category(tx, p, snapshot).await,
        Payload::CategoryUpsertedV1(p) => db::upsert_category(tx, p, snapshot).await,
        Payload::ProductUpsertedV1(p) => {
            if let Some(conflict) = uniqueness(tx, p).await? {
                return Ok(conflict);
            }
            db::upsert_product(tx, p, snapshot).await
        }
        _ => return Err(invalid("not a master-data event")),
    }
    .map_err(db_err)?;
    let server_seq = append_change(
        tx,
        &NewChange {
            aggregate_type,
            aggregate_id,
            version: Some(i64::try_from(new_version).unwrap_or(i64::MAX)),
            kind: if archived { "archive" } else { "upsert" },
            origin_register: origin.register,
            origin_event_id: origin.event,
            snapshot,
        },
    )
    .await
    .map_err(db_err)?;
    Ok(ApplyOutcome::Applied {
        server_seq: Some(server_seq),
        new_version: Some(new_version),
    })
}

struct Master(&'static str);

#[async_trait]
impl EventApplier for Master {
    fn event_type(&self) -> &'static str {
        self.0
    }

    fn supported_versions(&self) -> RangeInclusive<u32> {
        1..=1
    }

    async fn apply(
        &self,
        tx: &mut TenantTx<'_>,
        me: &DevicePrincipal,
        env: &EventEnvelope,
        payload: &Payload,
    ) -> Result<ApplyOutcome, ApplyError> {
        let base = env
            .base_version
            .ok_or_else(|| invalid("a master-data event needs baseVersion"))?;
        upsert(
            tx,
            Origin {
                register: Some(me.register_id),
                event: Some(env.event_id),
            },
            &env.aggregate_type,
            &env.aggregate_id,
            base,
            payload,
            &env.payload,
        )
        .await
    }
}
