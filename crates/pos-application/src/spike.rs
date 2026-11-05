//! STAGE 0 SPIKE (E00-T06): throwaway records and write-set builders for the vertical
//! slice "add product -> scan -> complete cash sale -> print". It exercises the real write
//! path (WriteSet, apply, outbox, audit, receipt numbering) against `spike_*` tables.
//!
//! **Delete this module, its `WriteOp::Spike*` variants and the `spike_*` tables when the
//! real catalogue, cart and sale tasks land (E06-T02, E09-T01, E09-T08).**

use crate::audit::{self, AuditEntry};
use crate::records::AggregateRef;
use crate::write::{Counter, NumberSlot, OutboxDraft, WriteContext, WriteSet, WriteSetError};
use pos_domain::{
    ArithmeticError, CartLineId, ErrorCode, HasErrorCode, IdGenerator, Money, MovementId, ProductId,
    Quantity, SaleId, SaleLineId, TenderId,
};
use serde::Serialize;

pub const RECEIPT_SLOT: NumberSlot = NumberSlot("receipt");

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeProduct {
    pub product_id: ProductId,
    pub name: String,
    pub barcode: String,
    pub price: Money,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpikeCartAdd {
    pub line_id: CartLineId,
    pub product_id: ProductId,
    pub quantity: Quantity,
}

/// A cart line as read back for checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpikeCartLine {
    pub product: SpikeProduct,
    pub quantity: Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeSaleLine {
    pub line_id: SaleLineId,
    pub movement_id: MovementId,
    pub product_id: ProductId,
    /// Snapshots (POS-010): later catalogue edits must not change the sale.
    pub name: String,
    pub barcode: String,
    pub quantity: Quantity,
    pub unit_price: Money,
    pub total: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeTender {
    pub tender_id: TenderId,
    pub amount: Money,
    pub received: Money,
    pub change: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeSale {
    pub sale_id: SaleId,
    pub lines: Vec<SpikeSaleLine>,
    pub total: Money,
    pub tender: SpikeTender,
    #[serde(skip)]
    pub receipt_slot: NumberSlot,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpikeError {
    #[error("the cart is empty")]
    EmptyCart,
    #[error("cash received {received} is less than the total {total} (minor units)")]
    Insufficient { total: i64, received: i64 },
    #[error("only whole quantities are supported in the Stage 0 spike")]
    FractionalQuantity,
    #[error(transparent)]
    Arithmetic(#[from] ArithmeticError),
    #[error(transparent)]
    WriteSet(#[from] WriteSetError),
}

impl HasErrorCode for SpikeError {
    fn error_code(&self) -> ErrorCode {
        match self {
            SpikeError::EmptyCart => ErrorCode::CartEmpty,
            SpikeError::Insufficient { .. } => ErrorCode::TenderInsufficient,
            SpikeError::FractionalQuantity | SpikeError::Arithmetic(_) => ErrorCode::ValidationFailed,
            SpikeError::WriteSet(e) => e.error_code(),
        }
    }
}

pub fn create_product(
    ctx: &WriteContext,
    ids: &dyn IdGenerator,
    product: SpikeProduct,
) -> Result<WriteSet, SpikeError> {
    let aggregate = AggregateRef {
        aggregate_type: "spike_product",
        aggregate_id: product.product_id.to_string(),
    };
    let event = OutboxDraft::new(ids, "spike.product.created", 1, aggregate, Some(0), &product)?;
    let audit = AuditEntry::new(
        ids,
        audit::CATALOG_PRODUCT_CREATED,
        "spike_product",
        product.product_id,
    )
    .linked_to(event.event_id);
    Ok(WriteSet::builder(ctx.clone())
        .spike_put_product(product)
        .outbox(event)
        .audit(audit)
        .build()?)
}

/// Cart edits are local only (ADR-009): no outbox event and, for a plain add, no audit row.
pub fn add_to_cart(
    ctx: &WriteContext,
    ids: &dyn IdGenerator,
    product: ProductId,
    quantity: Quantity,
) -> Result<WriteSet, SpikeError> {
    let add = SpikeCartAdd {
        line_id: CartLineId::generate(ids),
        product_id: product,
        quantity,
    };
    Ok(WriteSet::builder(ctx.clone()).spike_cart_add(add).build()?)
}

/// One atomic write set: receipt number, sale, lines, tender, stock movements, cart clear,
/// audit and outbox (PRD §11.2).
pub fn complete_cash_sale(
    ctx: &WriteContext,
    ids: &dyn IdGenerator,
    cart: &[SpikeCartLine],
    cash_received: Money,
) -> Result<(WriteSet, SpikeSale), SpikeError> {
    if cart.is_empty() {
        return Err(SpikeError::EmptyCart);
    }
    let mut lines = Vec::with_capacity(cart.len());
    for l in cart {
        if !l.quantity.is_whole() {
            return Err(SpikeError::FractionalQuantity);
        }
        let units = l.quantity.milli() / 1000;
        let total = Money::from_minor(
            l.product
                .price
                .minor()
                .checked_mul(units)
                .ok_or(ArithmeticError::Overflow)?,
        );
        lines.push(SpikeSaleLine {
            line_id: SaleLineId::generate(ids),
            movement_id: MovementId::generate(ids),
            product_id: l.product.product_id,
            name: l.product.name.clone(),
            barcode: l.product.barcode.clone(),
            quantity: l.quantity,
            unit_price: l.product.price,
            total,
        });
    }
    let total = Money::checked_sum(lines.iter().map(|l| l.total))?;
    if cash_received < total {
        return Err(SpikeError::Insufficient {
            total: total.minor(),
            received: cash_received.minor(),
        });
    }
    let change = cash_received.checked_sub(total)?;
    let sale = SpikeSale {
        sale_id: SaleId::generate(ids),
        lines,
        total,
        tender: SpikeTender {
            tender_id: TenderId::generate(ids),
            amount: total,
            received: cash_received,
            change,
        },
        receipt_slot: RECEIPT_SLOT,
    };
    let aggregate = AggregateRef {
        aggregate_type: "sale",
        aggregate_id: sale.sale_id.to_string(),
    };
    let event = OutboxDraft::new(ids, "sale.completed", 1, aggregate, None, &sale)?;
    let audit = AuditEntry::new(ids, audit::SALE_COMPLETED, "sale", sale.sale_id)
        .with_metadata(
            serde_json::json!({ "totalMinor": total.minor().to_string(), "lines": sale.lines.len() }),
        )
        .linked_to(event.event_id);
    let set = WriteSet::builder(ctx.clone())
        .allocate_number(Counter::Receipt, RECEIPT_SLOT)
        .spike_complete_sale(sale.clone())
        .outbox(event)
        .audit(audit)
        .build()?;
    Ok((set, sale))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_domain::{BusinessId, LocationId, RegisterId, Timestamp};
    use pos_testkit::SeqIdGenerator;

    fn ctx(ids: &SeqIdGenerator) -> WriteContext {
        WriteContext {
            business_id: BusinessId::generate(ids),
            location_id: LocationId::generate(ids),
            register_id: RegisterId::generate(ids),
            actor: None,
            occurred_at: Timestamp::from_millis(0),
        }
    }

    fn line(ids: &SeqIdGenerator, price: i64, units: i64) -> SpikeCartLine {
        SpikeCartLine {
            product: SpikeProduct {
                product_id: ProductId::generate(ids),
                name: "Cola".into(),
                barcode: "5012345678900".into(),
                price: Money::from_minor(price),
            },
            quantity: Quantity::from_whole(units).unwrap(),
        }
    }

    #[test]
    fn sale_totals_change_and_shape() {
        let ids = SeqIdGenerator::default();
        let c = ctx(&ids);
        let (set, sale) = complete_cash_sale(
            &c,
            &ids,
            &[line(&ids, 120, 2), line(&ids, 99, 1)],
            Money::from_minor(500),
        )
        .unwrap();
        assert_eq!(sale.total, Money::from_minor(339));
        assert_eq!(sale.tender.change, Money::from_minor(161));
        assert_eq!(set.outbox().len(), 1);
        assert_eq!(set.outbox()[0].event_type, "sale.completed");
        assert_eq!(set.audit().len(), 1);
    }

    #[test]
    fn insufficient_cash_and_empty_cart_are_rejected() {
        let ids = SeqIdGenerator::default();
        let c = ctx(&ids);
        let err = complete_cash_sale(&c, &ids, &[line(&ids, 120, 1)], Money::from_minor(100)).unwrap_err();
        assert_eq!(err.error_code(), ErrorCode::TenderInsufficient);
        assert_eq!(
            complete_cash_sale(&c, &ids, &[], Money::from_minor(100)).unwrap_err(),
            SpikeError::EmptyCart
        );
    }

    #[test]
    fn cart_adds_need_no_audit_or_outbox() {
        let ids = SeqIdGenerator::default();
        let c = ctx(&ids);
        let set = add_to_cart(&c, &ids, ProductId::generate(&ids), Quantity::ONE).unwrap();
        assert!(set.audit().is_empty() && set.outbox().is_empty());
    }
}
