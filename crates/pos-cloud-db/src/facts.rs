//! Fact storage (E17-T05, migration 0006): sales, returns, stock documents, cash sessions and
//! audit entries pushed by registers, the stock ledger and its balances, the daily rollup and
//! the flags raised for an owner. Insert-only, except the two projections. Every call runs in
//! a [`TenantTx`]; the applier has already checked the payload (`pos_sync::verify`).

use crate::tenant::TenantTx;
use chrono::{DateTime, NaiveDate, Utc};
use pos_sync::events::{
    CashSessionClosedV1, CashSessionOpenedV1, DocumentPostedV1, MovementSource, ReturnCompletedV1,
    SaleCompletedV1, StockMovementV1,
};
use serde::Serialize;
use uuid::Uuid;

/// Epoch milliseconds as a time, if valid.
pub fn time(ms: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms)
}

/// A business date (`YYYY-MM-DD`), if valid.
pub fn date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn bad(what: &str) -> sqlx::Error {
    sqlx::Error::Protocol(format!("unchecked {what} reached the store"))
}

fn ts(ms: i64) -> Result<DateTime<Utc>, sqlx::Error> {
    time(ms).ok_or_else(|| bad("time"))
}

fn day(s: &str) -> Result<NaiveDate, sqlx::Error> {
    date(s).ok_or_else(|| bad("date"))
}

/// The wire name of an enum value (`"card_external"`).
fn tag<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

fn biz(tx: &TenantTx<'_>) -> Uuid {
    *tx.business_id().as_uuid()
}

/// The location of a register of this business, if it exists.
pub async fn register_location(
    tx: &mut TenantTx<'_>,
    register_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let b = biz(tx);
    sqlx::query_scalar("SELECT location_id FROM registers WHERE business_id = $1 AND register_id = $2")
        .bind(b)
        .bind(register_id)
        .fetch_optional(tx.conn())
        .await
}

// ---------- sales ----------

pub async fn insert_sale(
    tx: &mut TenantTx<'_>,
    s: &SaleCompletedV1,
    event_id: Uuid,
    snapshot: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO sales (business_id, sale_id, register_id, location_id, cashier, approver, mode, receipt_seq,
                            receipt_number, occurred_at, business_date, currency_code, subtotal_minor, discount_minor,
                            tax_minor, rounding_minor, total_minor, origin_event_id, snapshot)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
    )
    .bind(b)
    .bind(s.id)
    .bind(s.register_id)
    .bind(s.location_id)
    .bind(s.cashier)
    .bind(s.approver)
    .bind(tag(&s.mode))
    .bind(i64::try_from(s.receipt_seq).map_err(|_| bad("receipt sequence"))?)
    .bind(&s.receipt_number)
    .bind(ts(s.occurred_at)?)
    .bind(day(&s.business_date)?)
    .bind(&s.currency.code)
    .bind(s.subtotal)
    .bind(s.line_discount_total + s.order_discount_total)
    .bind(s.tax_total)
    .bind(s.rounding)
    .bind(s.total)
    .bind(event_id)
    .bind(snapshot)
    .execute(tx.conn())
    .await?;
    for l in &s.lines {
        sqlx::query(
            "INSERT INTO sale_lines (business_id, line_id, sale_id, line_no, product_id, variant_id, name, quantity_milli,
                                     unit_price_minor, gross_minor, discount_minor, tax_minor, total_minor)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(b)
        .bind(l.id)
        .bind(s.id)
        .bind(i32::try_from(l.line_no).map_err(|_| bad("line number"))?)
        .bind(l.product_id)
        .bind(l.variant_id)
        .bind(&l.name)
        .bind(l.quantity)
        .bind(l.unit_price)
        .bind(l.gross)
        .bind(l.line_discount + l.order_discount_alloc)
        .bind(l.tax)
        .bind(l.total)
        .execute(tx.conn())
        .await?;
    }
    for t in &s.tenders {
        sqlx::query(
            "INSERT INTO tenders (business_id, tender_id, sale_id, seq, kind, amount_minor, status)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(b)
        .bind(t.id)
        .bind(s.id)
        .bind(i32::try_from(t.seq).map_err(|_| bad("tender sequence"))?)
        .bind(tag(&t.kind))
        .bind(t.amount)
        .bind(tag(&t.status))
        .execute(tx.conn())
        .await?;
    }
    Ok(())
}

/// Whether a sale has arrived.
pub async fn sale_exists(tx: &mut TenantTx<'_>, sale_id: Uuid) -> Result<bool, sqlx::Error> {
    let b = biz(tx);
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sales WHERE business_id = $1 AND sale_id = $2)")
        .bind(b)
        .bind(sale_id)
        .fetch_one(tx.conn())
        .await
}

// ---------- returns ----------

pub async fn insert_return(
    tx: &mut TenantTx<'_>,
    r: &ReturnCompletedV1,
    event_id: Uuid,
    snapshot: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO returns (business_id, return_id, original_sale_id, register_id, location_id, actor, receipt_number,
                              reason, occurred_at, business_date, subtotal_minor, tax_minor, total_minor,
                              origin_event_id, snapshot)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
    )
    .bind(b)
    .bind(r.id)
    .bind(r.original_sale)
    .bind(r.register_id)
    .bind(r.location_id)
    .bind(r.actor)
    .bind(&r.receipt_number)
    .bind(tag(&r.reason))
    .bind(ts(r.occurred_at)?)
    .bind(day(&r.business_date)?)
    .bind(r.subtotal)
    .bind(r.tax)
    .bind(r.total)
    .bind(event_id)
    .bind(snapshot)
    .execute(tx.conn())
    .await?;
    for l in &r.lines {
        sqlx::query(
            "INSERT INTO return_lines (business_id, return_line_id, return_id, sale_line_id, variant_id, quantity_milli,
                                       disposition, refund_minor, tax_minor)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(b)
        .bind(l.id)
        .bind(r.id)
        .bind(l.sale_line_id)
        .bind(l.variant_id)
        .bind(l.quantity)
        .bind(tag(&l.disposition))
        .bind(l.refund)
        .bind(l.tax)
        .execute(tx.conn())
        .await?;
    }
    for t in &r.refunds {
        sqlx::query(
            "INSERT INTO refunds (business_id, refund_id, return_id, seq, kind, amount_minor, status)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(b)
        .bind(t.id)
        .bind(r.id)
        .bind(i32::try_from(t.seq).map_err(|_| bad("refund sequence"))?)
        .bind(tag(&t.kind))
        .bind(t.amount)
        .bind(tag(&t.status))
        .execute(tx.conn())
        .await?;
    }
    Ok(())
}

/// The sale lines named by `line_ids` that do not belong to `sale_id` (or do not exist).
pub async fn foreign_sale_lines(
    tx: &mut TenantTx<'_>,
    sale_id: Uuid,
    line_ids: &[Uuid],
) -> Result<i64, sqlx::Error> {
    let b = biz(tx);
    sqlx::query_scalar(
        "SELECT count(*) FROM unnest($3::uuid[]) AS id
          WHERE NOT EXISTS (SELECT 1 FROM sale_lines l WHERE l.business_id = $1 AND l.sale_id = $2 AND l.line_id = id)",
    )
    .bind(b)
    .bind(sale_id)
    .bind(line_ids)
    .fetch_one(tx.conn())
    .await
}

/// Lines of a sale returned more than they were sold, across every register:
/// `(sale line, sold, returned)` in milli-units.
pub async fn over_returned(
    tx: &mut TenantTx<'_>,
    sale_id: Uuid,
) -> Result<Vec<(Uuid, i64, i64)>, sqlx::Error> {
    let b = biz(tx);
    sqlx::query_as(
        "SELECT l.line_id, l.quantity_milli, sum(r.quantity_milli)::bigint
           FROM sale_lines l JOIN return_lines r ON r.business_id = l.business_id AND r.sale_line_id = l.line_id
          WHERE l.business_id = $1 AND l.sale_id = $2
          GROUP BY l.line_id, l.quantity_milli
         HAVING sum(r.quantity_milli) > l.quantity_milli",
    )
    .bind(b)
    .bind(sale_id)
    .fetch_all(tx.conn())
    .await
}

// ---------- inventory documents ----------

pub async fn insert_document(
    tx: &mut TenantTx<'_>,
    d: &DocumentPostedV1,
    event_id: Uuid,
    snapshot: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO inventory_documents (business_id, document_id, doc_type, document_no, register_id, location_id,
                                          actor, reason, occurred_at, business_date, origin_event_id, snapshot)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(b)
    .bind(d.id)
    .bind(tag(&d.doc_type))
    .bind(&d.document_no)
    .bind(d.register_id)
    .bind(d.location_id)
    .bind(d.actor)
    .bind(d.reason.as_ref().map(tag))
    .bind(ts(d.occurred_at)?)
    .bind(day(&d.business_date)?)
    .bind(event_id)
    .bind(snapshot)
    .execute(tx.conn())
    .await
    .map(|_| ())
}

// ---------- stock ----------

/// A sellable balance that this movement took below zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WentNegative {
    pub location_id: Uuid,
    pub variant_id: Uuid,
    pub on_hand_milli: i64,
}

/// Records movements and moves their balances; returns the sellable balances that went from
/// zero or more to below zero.
pub async fn apply_movements(
    tx: &mut TenantTx<'_>,
    movements: &[StockMovementV1],
    register_id: Uuid,
) -> Result<Vec<WentNegative>, sqlx::Error> {
    let b = biz(tx);
    let mut negative = Vec::new();
    for m in movements {
        let (source_type, source_id) = match &m.source {
            MovementSource::Sale { sale_id, .. } => ("sale", *sale_id),
            MovementSource::Return { return_id, .. } => ("return", *return_id),
            MovementSource::InventoryDocument { document_id } => ("inventory_document", *document_id),
        };
        let bucket = tag(&m.bucket);
        sqlx::query(
            "INSERT INTO stock_movements (business_id, movement_id, location_id, variant_id, bucket, movement_type,
                                          delta_milli, unit_cost_minor, source_type, source_id, register_id,
                                          occurred_at, business_date)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(b)
        .bind(m.id)
        .bind(m.location_id)
        .bind(m.variant_id)
        .bind(&bucket)
        .bind(tag(&m.movement_type))
        .bind(m.delta)
        .bind(m.unit_cost)
        .bind(source_type)
        .bind(source_id)
        .bind(register_id)
        .bind(ts(m.occurred_at)?)
        .bind(day(&m.business_date)?)
        .execute(tx.conn())
        .await?;
        let on_hand: i64 = sqlx::query_scalar(
            "INSERT INTO stock_balances (business_id, location_id, variant_id, bucket, on_hand_milli)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (business_id, location_id, variant_id, bucket)
             DO UPDATE SET on_hand_milli = stock_balances.on_hand_milli + EXCLUDED.on_hand_milli, updated_at = now()
             RETURNING on_hand_milli",
        )
        .bind(b)
        .bind(m.location_id)
        .bind(m.variant_id)
        .bind(&bucket)
        .bind(m.delta)
        .fetch_one(tx.conn())
        .await?;
        if bucket == "sellable" && on_hand < 0 && on_hand - m.delta >= 0 {
            negative.push(WentNegative {
                location_id: m.location_id,
                variant_id: m.variant_id,
                on_hand_milli: on_hand,
            });
        }
    }
    Ok(negative)
}

/// A balance, in milli-units (0 when never moved).
pub async fn balance(
    tx: &mut TenantTx<'_>,
    location_id: Uuid,
    variant_id: Uuid,
    bucket: &str,
) -> Result<i64, sqlx::Error> {
    let b = biz(tx);
    let v: Option<i64> = sqlx::query_scalar(
        "SELECT on_hand_milli FROM stock_balances
          WHERE business_id = $1 AND location_id = $2 AND variant_id = $3 AND bucket = $4",
    )
    .bind(b)
    .bind(location_id)
    .bind(variant_id)
    .bind(bucket)
    .fetch_optional(tx.conn())
    .await?;
    Ok(v.unwrap_or(0))
}

// ---------- flags and rollup ----------

pub async fn insert_flag(
    tx: &mut TenantTx<'_>,
    kind: &str,
    subject_id: Uuid,
    detail: &serde_json::Value,
    event_id: Uuid,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO sync_flags (business_id, kind, subject_id, detail, origin_event_id) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(b)
    .bind(kind)
    .bind(subject_id)
    .bind(detail)
    .bind(event_id)
    .execute(tx.conn())
    .await
    .map(|_| ())
}

/// What one fact adds to a day's rollup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RollupDelta {
    pub sales_count: i32,
    pub sales_total: i64,
    pub tax: i64,
    pub discount: i64,
    pub returns_count: i32,
    pub refunds_total: i64,
}

pub async fn add_to_rollup(
    tx: &mut TenantTx<'_>,
    business_date: &str,
    register_id: Uuid,
    d: RollupDelta,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO daily_sales_rollup (business_id, business_date, register_id, sales_count, sales_total_minor,
                                         tax_minor, discount_minor, returns_count, refunds_total_minor)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
         ON CONFLICT (business_id, business_date, register_id) DO UPDATE SET
           sales_count = daily_sales_rollup.sales_count + EXCLUDED.sales_count,
           sales_total_minor = daily_sales_rollup.sales_total_minor + EXCLUDED.sales_total_minor,
           tax_minor = daily_sales_rollup.tax_minor + EXCLUDED.tax_minor,
           discount_minor = daily_sales_rollup.discount_minor + EXCLUDED.discount_minor,
           returns_count = daily_sales_rollup.returns_count + EXCLUDED.returns_count,
           refunds_total_minor = daily_sales_rollup.refunds_total_minor + EXCLUDED.refunds_total_minor",
    )
    .bind(b)
    .bind(day(business_date)?)
    .bind(register_id)
    .bind(d.sales_count)
    .bind(d.sales_total)
    .bind(d.tax)
    .bind(d.discount)
    .bind(d.returns_count)
    .bind(d.refunds_total)
    .execute(tx.conn())
    .await
    .map(|_| ())
}

// ---------- cash sessions and audit ----------

pub async fn insert_cash_opened(
    tx: &mut TenantTx<'_>,
    c: &CashSessionOpenedV1,
    event_id: Uuid,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO cash_sessions (business_id, cash_session_id, register_id, location_id, opened_by, opened_at,
                                    business_date, opening_float_minor, origin_event_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(b)
    .bind(c.session.id)
    .bind(c.register_id)
    .bind(c.location_id)
    .bind(c.session.opened_by)
    .bind(ts(c.session.opened_at)?)
    .bind(day(&c.session.business_date)?)
    .bind(c.session.opening_float)
    .bind(event_id)
    .execute(tx.conn())
    .await
    .map(|_| ())
}

/// The register a cash session was opened on, if it has arrived.
pub async fn cash_session_register(
    tx: &mut TenantTx<'_>,
    session_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let b = biz(tx);
    sqlx::query_scalar(
        "SELECT register_id FROM cash_sessions WHERE business_id = $1 AND cash_session_id = $2",
    )
    .bind(b)
    .bind(session_id)
    .fetch_optional(tx.conn())
    .await
}

pub async fn insert_cash_closed(
    tx: &mut TenantTx<'_>,
    c: &CashSessionClosedV1,
    event_id: Uuid,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    let totals = serde_json::to_value(&c.session.totals).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query(
        "INSERT INTO cash_session_closings (business_id, cash_session_id, register_id, session_number, closed_by,
                                            closed_at, expected_minor, counted_minor, variance_minor, totals, note,
                                            origin_event_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(b)
    .bind(c.session.id)
    .bind(c.register_id)
    .bind(&c.session_number)
    .bind(c.session.closed_by)
    .bind(ts(c.session.closed_at)?)
    .bind(c.session.expected)
    .bind(c.session.counted)
    .bind(c.session.variance)
    .bind(totals)
    .bind(&c.session.note)
    .bind(event_id)
    .execute(tx.conn())
    .await
    .map(|_| ())
}

/// An audit entry, timed by its event.
#[allow(clippy::too_many_arguments)]
pub async fn insert_audit(
    tx: &mut TenantTx<'_>,
    audit_id: Uuid,
    register_id: Uuid,
    action: &str,
    entity_type: &str,
    entity_id: &str,
    metadata: &serde_json::Value,
    occurred_at: DateTime<Utc>,
    event_id: Uuid,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO audit_events (business_id, audit_id, register_id, action, entity_type, entity_id, metadata,
                                   occurred_at, origin_event_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(b)
    .bind(audit_id)
    .bind(register_id)
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .bind(metadata)
    .bind(occurred_at)
    .bind(event_id)
    .execute(tx.conn())
    .await
    .map(|_| ())
}
