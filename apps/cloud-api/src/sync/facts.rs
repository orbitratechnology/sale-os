//! Fact appliers (E17-T05, SYN-009, PRD §11.1/§11.5, 07 §4): sales, returns, stock documents,
//! cash sessions and audit entries. Facts already happened, so they never conflict: a valid
//! fact is stored as sent, its stock movements update the balances (which may go negative, and
//! are then flagged), live sales and returns feed the daily rollup, and other registers see it
//! in the change feed. A fact that names another register or location, does not add up
//! (`SNAPSHOT_MISMATCH`) or is malformed is rejected; a return whose sale has not arrived yet
//! is transient (`ORIGINAL_SALE_PENDING`).

use super::{Appliers, ApplyError, ApplyOutcome, EventApplier};
use crate::devices::DevicePrincipal;
use async_trait::async_trait;
use pos_cloud_db::TenantTx;
use pos_cloud_db::facts::{self as db, RollupDelta, WentNegative};
use pos_cloud_db::sync::{NewChange, append_change};
use pos_sync::events::{SaleMode, StockMovementV1};
use pos_sync::{EventEnvelope, Payload, codes, verify};
use serde_json::json;
use std::ops::RangeInclusive;
use std::sync::Arc;
use uuid::Uuid;

/// Adds every fact applier to `appliers`.
pub fn register(appliers: Appliers) -> Appliers {
    appliers
        .with(Arc::new(Fact("sale.completed")))
        .with(Arc::new(Fact("return.completed")))
        .with(Arc::new(Fact("inventory.document_posted")))
        .with(Arc::new(Fact("cash_session.opened")))
        .with(Arc::new(Fact("cash_session.closed")))
        .with(Arc::new(Fact("audit.recorded")))
}

fn invalid(detail: impl Into<String>) -> ApplyError {
    ApplyError::Rejected {
        code: codes::EVENT_INVALID,
        detail: detail.into(),
    }
}

fn refused(r: verify::Refusal) -> ApplyError {
    ApplyError::Rejected {
        code: r.code,
        detail: r.detail,
    }
}

/// Database errors: a key that is already taken or a reference to nothing is the event's
/// fault (rejected); anything else may pass (transient).
fn db_err(e: sqlx::Error) -> ApplyError {
    if let sqlx::Error::Database(d) = &e {
        match d.code().as_deref() {
            Some("23505") => return invalid("a fact with this id was already recorded"),
            Some("23503") => return invalid("the fact refers to something this business does not have"),
            _ => {}
        }
    }
    ApplyError::from(e)
}

/// The fact is this device's register's, at that register's location, and the envelope
/// describes it.
async fn check_place(
    tx: &mut TenantTx<'_>,
    me: &DevicePrincipal,
    env: &EventEnvelope,
    register_id: Uuid,
    location_id: Option<Uuid>,
    fact_id: Uuid,
) -> Result<(), ApplyError> {
    if register_id != me.register_id {
        return Err(invalid("the fact is for another register"));
    }
    let at = db::register_location(tx, register_id).await.map_err(db_err)?;
    let location = location_id.unwrap_or(env.location_id);
    if at != Some(location) || env.location_id != location {
        return Err(invalid("the fact is for another location"));
    }
    if env.aggregate_id != fact_id.to_string() {
        return Err(invalid("the envelope's aggregateId is not the fact's id"));
    }
    Ok(())
}

fn check_times(times: &[i64], dates: &[&str]) -> Result<(), ApplyError> {
    if times.iter().any(|t| db::time(*t).is_none()) {
        return Err(invalid("a time is out of range"));
    }
    if dates.iter().any(|d| db::date(d).is_none()) {
        return Err(invalid("a business date is not YYYY-MM-DD"));
    }
    Ok(())
}

fn movement_times(ms: &[StockMovementV1]) -> (Vec<i64>, Vec<&str>) {
    (
        ms.iter().map(|m| m.occurred_at).collect(),
        ms.iter().map(|m| m.business_date.as_str()).collect(),
    )
}

/// Moves stock and flags balances that went negative.
async fn move_stock(
    tx: &mut TenantTx<'_>,
    ms: &[StockMovementV1],
    register_id: Uuid,
    event_id: Uuid,
) -> Result<(), ApplyError> {
    let negative = db::apply_movements(tx, ms, register_id).await.map_err(db_err)?;
    for WentNegative {
        location_id,
        variant_id,
        on_hand_milli,
    } in negative
    {
        db::insert_flag(
            tx,
            "negative_stock",
            variant_id,
            &json!({"locationId": location_id, "variantId": variant_id, "onHandMilli": on_hand_milli,
                    "registerId": register_id}),
            event_id,
        )
        .await
        .map_err(db_err)?;
    }
    Ok(())
}

/// The change-feed row other registers pull.
async fn feed(
    tx: &mut TenantTx<'_>,
    me: &DevicePrincipal,
    env: &EventEnvelope,
    aggregate_type: &str,
) -> Result<i64, ApplyError> {
    append_change(
        tx,
        &NewChange {
            event_type: &env.event_type,
            schema_version: i32::try_from(env.schema_version).unwrap_or(1),
            aggregate_type,
            aggregate_id: &env.aggregate_id,
            version: None,
            kind: "fact",
            origin_register: Some(me.register_id),
            origin_event_id: Some(env.event_id),
            snapshot: &env.payload,
        },
    )
    .await
    .map_err(db_err)
}

fn applied(server_seq: Option<i64>) -> Result<ApplyOutcome, ApplyError> {
    Ok(ApplyOutcome::Applied {
        server_seq,
        new_version: None,
    })
}

/// One applier per fact type; the payload decides what it does.
struct Fact(&'static str);

#[async_trait]
impl EventApplier for Fact {
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
        match payload {
            Payload::SaleCompletedV1(s) => {
                check_place(tx, me, env, s.register_id, Some(s.location_id), s.id).await?;
                let (mut times, mut dates) = movement_times(&s.stock_movements);
                times.push(s.occurred_at);
                dates.push(&s.business_date);
                check_times(&times, &dates)?;
                verify::sale(s).map_err(refused)?;
                db::insert_sale(tx, s, env.event_id, &env.payload)
                    .await
                    .map_err(db_err)?;
                move_stock(tx, &s.stock_movements, s.register_id, env.event_id).await?;
                if s.mode == SaleMode::Sandbox {
                    // Practice sales are kept, but count nowhere and go to no other register.
                    return applied(None);
                }
                let delta = RollupDelta {
                    sales_count: 1,
                    sales_total: s.total,
                    tax: s.tax_total,
                    discount: s.line_discount_total + s.order_discount_total,
                    ..RollupDelta::default()
                };
                db::add_to_rollup(tx, &s.business_date, s.register_id, delta)
                    .await
                    .map_err(db_err)?;
                applied(Some(feed(tx, me, env, "sale").await?))
            }
            Payload::ReturnCompletedV1(r) => {
                check_place(tx, me, env, r.register_id, Some(r.location_id), r.id).await?;
                let (mut times, mut dates) = movement_times(&r.stock_movements);
                times.push(r.occurred_at);
                dates.push(&r.business_date);
                check_times(&times, &dates)?;
                verify::return_(r).map_err(refused)?;
                if !db::sale_exists(tx, r.original_sale).await.map_err(db_err)? {
                    // Sold on another register that has not synced yet: try again later.
                    return Err(ApplyError::Transient {
                        code: codes::ORIGINAL_SALE_PENDING,
                        detail: format!("sale {} has not arrived yet", r.original_receipt_number),
                    });
                }
                let lines: Vec<Uuid> = r.lines.iter().map(|l| l.sale_line_id).collect();
                if db::foreign_sale_lines(tx, r.original_sale, &lines)
                    .await
                    .map_err(db_err)?
                    > 0
                {
                    return Err(invalid("a return line names a line of another sale"));
                }
                db::insert_return(tx, r, env.event_id, &env.payload)
                    .await
                    .map_err(db_err)?;
                move_stock(tx, &r.stock_movements, r.register_id, env.event_id).await?;
                // Returned on two registers offline, more than was sold: it happened; flag it.
                for (line, sold, returned) in db::over_returned(tx, r.original_sale).await.map_err(db_err)? {
                    db::insert_flag(
                        tx,
                        "over_return",
                        line,
                        &json!({"saleId": r.original_sale, "saleLineId": line, "soldMilli": sold,
                                "returnedMilli": returned, "returnId": r.id}),
                        env.event_id,
                    )
                    .await
                    .map_err(db_err)?;
                }
                let delta = RollupDelta {
                    returns_count: 1,
                    refunds_total: r.total,
                    ..RollupDelta::default()
                };
                db::add_to_rollup(tx, &r.business_date, r.register_id, delta)
                    .await
                    .map_err(db_err)?;
                applied(Some(feed(tx, me, env, "return").await?))
            }
            Payload::DocumentPostedV1(d) => {
                check_place(tx, me, env, d.register_id, Some(d.location_id), d.id).await?;
                let (mut times, mut dates) = movement_times(&d.stock_movements);
                times.push(d.occurred_at);
                dates.push(&d.business_date);
                check_times(&times, &dates)?;
                verify::document(d).map_err(refused)?;
                db::insert_document(tx, d, env.event_id, &env.payload)
                    .await
                    .map_err(db_err)?;
                move_stock(tx, &d.stock_movements, d.register_id, env.event_id).await?;
                applied(Some(feed(tx, me, env, "inventory_document").await?))
            }
            Payload::CashSessionOpenedV1(c) => {
                check_place(tx, me, env, c.register_id, Some(c.location_id), c.session.id).await?;
                check_times(&[c.session.opened_at], &[&c.session.business_date])?;
                if c.session.opening_float < 0 {
                    return Err(invalid("the opening float is negative"));
                }
                db::insert_cash_opened(tx, c, env.event_id)
                    .await
                    .map_err(db_err)?;
                applied(Some(feed(tx, me, env, "cash_session").await?))
            }
            Payload::CashSessionClosedV1(c) => {
                check_place(tx, me, env, c.register_id, None, c.session.id).await?;
                check_times(&[c.session.closed_at], &[])?;
                if c.session.counted - c.session.expected != c.session.variance {
                    return Err(ApplyError::Rejected {
                        code: codes::SNAPSHOT_MISMATCH,
                        detail: "variance is not counted less expected".into(),
                    });
                }
                match db::cash_session_register(tx, c.session.id)
                    .await
                    .map_err(db_err)?
                {
                    Some(r) if r == c.register_id => {}
                    Some(_) => return Err(invalid("the session was opened on another register")),
                    None => return Err(invalid("closing a session that was never opened")),
                }
                db::insert_cash_closed(tx, c, env.event_id)
                    .await
                    .map_err(db_err)?;
                applied(Some(feed(tx, me, env, "cash_session").await?))
            }
            Payload::AuditRecordedV1(a) => {
                if env.aggregate_id != a.audit_id.to_string() {
                    return Err(invalid("the envelope's aggregateId is not the audit id"));
                }
                let at = chrono::DateTime::parse_from_rfc3339(&env.occurred_at)
                    .map_err(|_| invalid("occurredAt is not an RFC 3339 time"))?
                    .to_utc();
                db::insert_audit(
                    tx,
                    a.audit_id,
                    me.register_id,
                    &a.action,
                    &a.entity_type,
                    &a.entity_id,
                    &a.metadata,
                    at,
                    env.event_id,
                )
                .await
                .map_err(db_err)?;
                // The local log of one register: kept for the owner, not sent to the others.
                applied(None)
            }
            _ => Err(invalid(format!("{} is not a fact", env.event_type))),
        }
    }
}
