//! Server-side checks on fact payloads (07 §4 "Server-side validation for facts", E17-T05).
//! They repeat, on the wire types, the invariants the register enforced when it wrote the
//! fact, so a payload that was corrupted or tampered with on the way is refused instead of
//! skewing the books. Totals that do not add up are `SNAPSHOT_MISMATCH`; anything else
//! malformed is `EVENT_INVALID`.

use crate::codes;
use crate::events::{
    DocumentPostedV1, MovementSource, ReturnCompletedV1, SaleCompletedV1, StockMovementV1, TenderV1,
};
use uuid::Uuid;

/// Why a fact was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub detail: String,
}

fn mismatch(detail: impl Into<String>) -> Refusal {
    Refusal {
        code: codes::SNAPSHOT_MISMATCH,
        detail: detail.into(),
    }
}

fn invalid(detail: impl Into<String>) -> Refusal {
    Refusal {
        code: codes::EVENT_INVALID,
        detail: detail.into(),
    }
}

fn sum(xs: impl IntoIterator<Item = i64>) -> Result<i64, Refusal> {
    xs.into_iter()
        .try_fold(0i64, i64::checked_add)
        .ok_or_else(|| invalid("an amount overflows"))
}

fn check(ok: bool, r: impl FnOnce() -> Refusal) -> Result<(), Refusal> {
    if ok { Ok(()) } else { Err(r()) }
}

fn tenders(ts: &[TenderV1], total: i64, what: &str) -> Result<(), Refusal> {
    for t in ts {
        check(t.amount > 0, || {
            invalid(format!("a {what} amount is not positive"))
        })?;
        check(t.change >= 0, || invalid(format!("a {what} has negative change")))?;
    }
    let paid = sum(ts.iter().map(|t| t.amount))?;
    check(paid == total, || {
        mismatch(format!("{what}s add up to {paid}, the total is {total}"))
    })
}

fn movements(
    ms: &[StockMovementV1],
    location: Uuid,
    source_ok: impl Fn(&MovementSource) -> bool,
) -> Result<(), Refusal> {
    for m in ms {
        check(m.location_id == location, || {
            invalid("a stock movement is for another location")
        })?;
        check(m.delta != 0, || invalid("a stock movement moves nothing"))?;
        check(source_ok(&m.source), || {
            invalid("a stock movement names another source")
        })?;
    }
    Ok(())
}

/// A completed sale: every total is the sum of its parts (PRD §8.3, 04 §0006).
pub fn sale(s: &SaleCompletedV1) -> Result<(), Refusal> {
    check(!s.lines.is_empty(), || invalid("a sale has no lines"))?;
    for l in &s.lines {
        check(l.quantity > 0, || {
            invalid(format!("line {} has a quantity that is not positive", l.line_no))
        })?;
        check(l.unit_price >= 0 && l.gross >= 0, || {
            invalid(format!("line {} has a negative price", l.line_no))
        })?;
        check(
            l.line_discount >= 0 && l.order_discount_alloc >= 0 && l.tax >= 0,
            || invalid(format!("line {} has a negative discount or tax", l.line_no)),
        )?;
        let net = l.gross - l.line_discount - l.order_discount_alloc;
        let expected = if s.prices_include_tax { net } else { net + l.tax };
        check(l.total == expected, || {
            mismatch(format!(
                "line {} total {} should be {expected}",
                l.line_no, l.total
            ))
        })?;
        let line_tax = sum(l.tax_lines.iter().map(|t| t.tax))?;
        check(line_tax == l.tax, || {
            mismatch(format!(
                "line {} tax lines add up to {line_tax}, not {}",
                l.line_no, l.tax
            ))
        })?;
    }
    let parts = [
        ("subtotal", s.subtotal, sum(s.lines.iter().map(|l| l.gross))?),
        (
            "line discounts",
            s.line_discount_total,
            sum(s.lines.iter().map(|l| l.line_discount))?,
        ),
        (
            "order discount",
            s.order_discount_total,
            sum(s.lines.iter().map(|l| l.order_discount_alloc))?,
        ),
        ("tax", s.tax_total, sum(s.lines.iter().map(|l| l.tax))?),
        (
            "tax breakdown",
            s.tax_total,
            sum(s.tax_breakdown.iter().map(|t| t.tax))?,
        ),
        (
            "total",
            s.total,
            sum(s.lines.iter().map(|l| l.total))? + s.rounding,
        ),
        ("tendered less change", s.total, s.tendered_total - s.change),
    ];
    for (what, stated, computed) in parts {
        check(stated == computed, || {
            mismatch(format!("{what}: {stated} stated, {computed} from the parts"))
        })?;
    }
    tenders(&s.tenders, s.total, "tender")?;
    let line_ids: Vec<Uuid> = s.lines.iter().map(|l| l.id).collect();
    movements(
        &s.stock_movements,
        s.location_id,
        |src| matches!(src, MovementSource::Sale { sale_id, line_id } if *sale_id == s.id && line_ids.contains(line_id)),
    )?;
    for m in &s.stock_movements {
        check(m.delta < 0, || invalid("a sale movement adds stock"))?;
    }
    Ok(())
}

/// A completed return: refunds add up to the lines, and are paid out exactly.
pub fn return_(r: &ReturnCompletedV1) -> Result<(), Refusal> {
    check(!r.lines.is_empty(), || invalid("a return has no lines"))?;
    for l in &r.lines {
        check(l.quantity > 0, || {
            invalid("a return line has a quantity that is not positive")
        })?;
        check(l.refund >= 0 && l.tax >= 0 && l.tax <= l.refund, || {
            invalid("a return line has a negative refund or tax")
        })?;
    }
    let total = sum(r.lines.iter().map(|l| l.refund))?;
    let tax = sum(r.lines.iter().map(|l| l.tax))?;
    check(r.total == total, || {
        mismatch(format!("total: {} stated, {total} from the lines", r.total))
    })?;
    check(r.tax == tax, || {
        mismatch(format!("tax: {} stated, {tax} from the lines", r.tax))
    })?;
    check(r.subtotal == total - tax, || {
        mismatch("subtotal is not total less tax")
    })?;
    tenders(&r.refunds, r.total, "refund")?;
    let line_ids: Vec<Uuid> = r.lines.iter().map(|l| l.id).collect();
    movements(
        &r.stock_movements,
        r.location_id,
        |src| matches!(src, MovementSource::Return { return_id, line_id } if *return_id == r.id && line_ids.contains(line_id)),
    )?;
    for m in &r.stock_movements {
        check(m.delta > 0, || invalid("a return movement removes stock"))?;
    }
    Ok(())
}

/// A posted inventory document: its stock movements are its lines.
pub fn document(d: &DocumentPostedV1) -> Result<(), Refusal> {
    check(!d.movements.is_empty(), || invalid("a document has no lines"))?;
    check(d.movements.len() == d.stock_movements.len(), || {
        mismatch("the document's lines and its stock movements differ")
    })?;
    for (line, m) in d.movements.iter().zip(&d.stock_movements) {
        check(
            line.variant_id == m.variant_id && line.delta == m.delta && line.bucket == m.bucket,
            || mismatch("a document line differs from its stock movement"),
        )?;
    }
    movements(
        &d.stock_movements,
        d.location_id,
        |src| matches!(src, MovementSource::InventoryDocument { document_id } if *document_id == d.id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::*;
    use serde_json::json;

    fn sale_json() -> serde_json::Value {
        let (sale, line, loc) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let tax =
            json!({"tax_rate_id": Uuid::now_v7(), "name": "VAT", "rate": 200000, "taxable": 200, "tax": 40});
        let line_json = json!({"id": line, "lineNo": 1, "productId": Uuid::now_v7(), "variantId": Uuid::now_v7(),
            "name": "Cola", "variantLabel": "", "sku": null, "productType": "stock", "unit": "ea", "trackStock": true,
            "quantity": 2000, "unitPrice": 120, "unitCost": null, "priceOverridden": false, "gross": 240,
            "discount": null, "lineDiscount": 0, "orderDiscountAlloc": 0, "tax": 40, "taxLines": [tax.clone()], "total": 240});
        let tender = json!({"id": Uuid::now_v7(), "seq": 1, "kind": "cash", "amount": 240, "received": 500, "change": 260,
            "external_reference": null, "status": "confirmed", "confirmation": "operator"});
        let source = json!({"type": "sale", "sale_id": sale, "line_id": line});
        let movement = json!({"id": Uuid::now_v7(), "variantId": Uuid::now_v7(), "locationId": loc, "bucket": "sellable",
            "movementType": "sale", "delta": -2000, "unitCost": null, "source": source, "actor": Uuid::now_v7(),
            "occurredAt": 1, "businessDate": "2026-11-05"});
        let mut v = json!({
            "id": sale, "cartId": Uuid::now_v7(), "registerId": Uuid::now_v7(), "locationId": loc,
            "cashier": Uuid::now_v7(), "approver": null, "mode": "live", "receiptSeq": 1, "receiptNumber": "R01-000001",
            "occurredAt": 1, "businessDate": "2026-11-05", "timezone": "Europe/London",
            "currency": {"code": "GBP", "exponent": 2}, "pricesIncludeTax": true,
            "orderDiscount": null, "subtotal": 240, "lineDiscountTotal": 0, "orderDiscountTotal": 0, "taxTotal": 40,
            "rounding": 0, "total": 240, "tenderedTotal": 500, "change": 260
        });
        v["lines"] = json!([line_json]);
        v["taxBreakdown"] = json!([tax]);
        v["tenders"] = json!([tender]);
        v["stockMovements"] = json!([movement]);
        v
    }

    fn parse(v: serde_json::Value) -> SaleCompletedV1 {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_consistent_sale_passes() {
        assert_eq!(sale(&parse(sale_json())), Ok(()));
    }

    #[test]
    fn tampered_totals_are_a_snapshot_mismatch() {
        for (path, value) in [
            ("/total", 230),
            ("/subtotal", 999),
            ("/taxTotal", 41),
            ("/lines/0/total", 200),
            ("/tenders/0/amount", 200),
            ("/change", 250),
        ] {
            let mut v = sale_json();
            *v.pointer_mut(path).unwrap() = json!(value);
            let r = sale(&parse(v)).unwrap_err();
            assert_eq!(r.code, codes::SNAPSHOT_MISMATCH, "{path}: {}", r.detail);
        }
    }

    #[test]
    fn malformed_sales_are_invalid() {
        let mut v = sale_json();
        *v.pointer_mut("/lines/0/quantity").unwrap() = json!(0);
        assert_eq!(sale(&parse(v)).unwrap_err().code, codes::EVENT_INVALID);
        let mut v = sale_json();
        *v.pointer_mut("/stockMovements/0/delta").unwrap() = json!(2000);
        assert_eq!(sale(&parse(v)).unwrap_err().code, codes::EVENT_INVALID);
        let mut v = sale_json();
        *v.pointer_mut("/stockMovements/0/source/sale_id").unwrap() = json!(Uuid::now_v7());
        assert_eq!(sale(&parse(v)).unwrap_err().code, codes::EVENT_INVALID);
        let mut v = sale_json();
        *v.pointer_mut("/stockMovements/0/locationId").unwrap() = json!(Uuid::now_v7());
        assert_eq!(sale(&parse(v)).unwrap_err().code, codes::EVENT_INVALID);
    }

    #[test]
    fn tax_on_top_is_added_to_the_line_total() {
        let mut v = sale_json();
        v["pricesIncludeTax"] = json!(false);
        assert_eq!(
            sale(&parse(v.clone())).unwrap_err().code,
            codes::SNAPSHOT_MISMATCH
        );
        for p in ["/lines/0/total", "/total", "/tenders/0/amount"] {
            *v.pointer_mut(p).unwrap() = json!(280);
        }
        v["change"] = json!(220);
        v["tenders"][0]["change"] = json!(220);
        assert_eq!(sale(&parse(v)), Ok(()));
    }
}
