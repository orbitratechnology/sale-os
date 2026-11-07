//! A completed sale as an immutable snapshot (E09-T08, POS-009/010).
//!
//! Every line carries what the receipt and reports need (name, label, SKU, unit price, tax
//! lines), so renaming a product or changing its price or tax later never changes a past
//! sale. The snapshot is also the `sale.completed` v1 payload: self-contained, so the cloud
//! never looks up current prices (PRD §8.3).

use crate::cart::Cart;
use crate::catalog::{ProductType, Unit};
use crate::inventory::{MovementSource, MovementType, StockBucket, StockMovement};
use crate::pricing::{Discount, PricedCart, TaxLine};
use crate::settlement::{SettledTender, Settlement};
use crate::{
    BusinessDate, CartId, Currency, ErrorCode, HasErrorCode, IdGenerator, LocationId, Money, MovementId,
    ProductId, Quantity, SaleId, SaleLineId, TenderId, Timestamp, UserId, VariantId,
};
use serde::Serialize;

/// Sandbox sales are practice: excluded from reports (PRD §7.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SaleMode {
    Sandbox,
    Live,
}

impl SaleMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            SaleMode::Sandbox => "sandbox",
            SaleMode::Live => "live",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleLine {
    pub id: SaleLineId,
    pub line_no: u32,
    pub product_id: ProductId,
    pub variant_id: VariantId,
    pub name: String,
    pub variant_label: String,
    pub sku: Option<String>,
    pub unit: Unit,
    pub product_type: ProductType,
    pub track_stock: bool,
    pub quantity: Quantity,
    pub unit_price: Money,
    pub price_overridden: bool,
    pub gross: Money,
    pub line_discount: Money,
    pub order_discount_alloc: Money,
    pub tax: Money,
    pub total: Money,
    pub discount: Option<Discount>,
    pub tax_lines: Vec<TaxLine>,
    pub unit_cost: Option<Money>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleTender {
    pub id: TenderId,
    pub seq: u32,
    #[serde(flatten)]
    pub tender: SettledTender,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleSnapshot {
    pub id: SaleId,
    pub cart_id: CartId,
    pub mode: SaleMode,
    pub currency: Currency,
    pub prices_include_tax: bool,
    pub subtotal: Money,
    pub line_discount_total: Money,
    pub order_discount_total: Money,
    pub tax_total: Money,
    /// Cash rounding (ADR-004).
    pub rounding: Money,
    /// What the customer paid: the priced total plus rounding.
    pub total: Money,
    pub tendered_total: Money,
    pub change: Money,
    pub order_discount: Option<Discount>,
    pub tax_breakdown: Vec<TaxLine>,
    pub lines: Vec<SaleLine>,
    pub tenders: Vec<SaleTender>,
    pub cashier: UserId,
    /// Who approved a discount, override or other step in this sale, if anyone.
    pub approver: Option<UserId>,
    pub occurred_at: Timestamp,
    pub business_date: BusinessDate,
    pub timezone: String,
}

/// The parts of a sale that come from the till rather than the cart.
pub struct SaleHeader {
    pub mode: SaleMode,
    pub currency: Currency,
    pub prices_include_tax: bool,
    pub cashier: UserId,
    pub occurred_at: Timestamp,
    pub business_date: BusinessDate,
    pub timezone: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SaleError {
    #[error("the cart is empty")]
    Empty,
    #[error("the priced cart does not match the cart")]
    PricingMismatch,
    #[error("the sale does not add up: {0}")]
    Inconsistent(&'static str),
}

impl HasErrorCode for SaleError {
    fn error_code(&self) -> ErrorCode {
        match self {
            SaleError::Empty => ErrorCode::CartEmpty,
            _ => ErrorCode::Internal,
        }
    }
}

impl SaleSnapshot {
    /// Builds the snapshot from the cart, its pricing and the settlement, and checks the
    /// invariants before anything is written.
    pub fn build(
        cart: &Cart,
        priced: &PricedCart,
        settlement: Settlement,
        header: SaleHeader,
        ids: &dyn IdGenerator,
    ) -> Result<SaleSnapshot, SaleError> {
        if cart.lines.is_empty() {
            return Err(SaleError::Empty);
        }
        if cart.lines.len() != priced.lines.len() {
            return Err(SaleError::PricingMismatch);
        }
        let mut lines = Vec::with_capacity(cart.lines.len());
        for (i, (c, p)) in cart.lines.iter().zip(&priced.lines).enumerate() {
            if c.id != p.line_id {
                return Err(SaleError::PricingMismatch);
            }
            let v = &c.variant;
            lines.push(SaleLine {
                id: SaleLineId::generate(ids),
                line_no: u32::try_from(i + 1).map_err(|_| SaleError::Inconsistent("too many lines"))?,
                product_id: v.product_id,
                variant_id: v.variant_id,
                name: v.name.clone(),
                variant_label: v.label.clone(),
                sku: v.sku.clone(),
                unit: v.unit,
                product_type: v.product_type,
                track_stock: v.track_stock,
                quantity: c.quantity,
                unit_price: c.price_override.unwrap_or(v.price),
                price_overridden: c.price_override.is_some(),
                gross: p.gross,
                line_discount: p.line_discount,
                order_discount_alloc: p.order_discount_alloc,
                tax: p.tax,
                total: p.total,
                discount: c.line_discount.clone(),
                tax_lines: p.tax_lines.clone(),
                unit_cost: v.cost,
            });
        }
        let approver = cart
            .lines
            .iter()
            .filter_map(|l| l.line_discount.as_ref().and_then(|d| d.authorized_by))
            .chain(cart.order_discount.as_ref().and_then(|d| d.authorized_by))
            .next();
        let tenders = settlement
            .tenders
            .into_iter()
            .enumerate()
            .map(|(i, tender)| SaleTender {
                id: TenderId::generate(ids),
                seq: u32::try_from(i + 1).unwrap_or(u32::MAX),
                tender,
            })
            .collect();
        let sale = SaleSnapshot {
            id: SaleId::generate(ids),
            cart_id: cart.id,
            mode: header.mode,
            currency: header.currency,
            prices_include_tax: header.prices_include_tax,
            subtotal: priced.subtotal,
            line_discount_total: priced.line_discount_total,
            order_discount_total: priced.order_discount_total,
            tax_total: priced.tax_total,
            rounding: settlement.rounding,
            total: settlement.total_due,
            tendered_total: settlement.tendered_total,
            change: settlement.change,
            order_discount: cart.order_discount.clone(),
            tax_breakdown: priced.tax_breakdown.clone(),
            lines,
            tenders,
            cashier: header.cashier,
            approver,
            occurred_at: header.occurred_at,
            business_date: header.business_date,
            timezone: header.timezone,
        };
        sale.verify()?;
        Ok(sale)
    }

    /// The invariants of 04-local-database §0006, also checked again by the cloud.
    pub fn verify(&self) -> Result<(), SaleError> {
        fn sum(xs: impl Iterator<Item = Money>) -> Result<Money, SaleError> {
            xs.into_iter()
                .try_fold(Money::ZERO, |a, x| a.checked_add(x))
                .map_err(|_| SaleError::Inconsistent("overflow"))
        }
        let lines = sum(self.lines.iter().map(|l| l.total))?;
        if lines.checked_add(self.rounding).ok() != Some(self.total) {
            return Err(SaleError::Inconsistent(
                "line totals plus rounding differ from the total",
            ));
        }
        if sum(self.tenders.iter().map(|t| t.tender.amount))? != self.total {
            return Err(SaleError::Inconsistent("tenders differ from the total"));
        }
        if self.tendered_total.checked_sub(self.change).ok() != Some(self.total) {
            return Err(SaleError::Inconsistent(
                "tendered minus change differs from the total",
            ));
        }
        Ok(())
    }

    /// Stock movements for the stock-tracked lines (INV-002): one per line, negative.
    pub fn movements(&self, location: LocationId, ids: &dyn IdGenerator) -> Vec<StockMovement> {
        self.lines
            .iter()
            .filter(|l| l.track_stock)
            .map(|l| StockMovement {
                id: MovementId::generate(ids),
                location_id: location,
                variant_id: l.variant_id,
                bucket: StockBucket::Sellable,
                movement_type: MovementType::Sale,
                delta: Quantity::from_milli(-l.quantity.milli()),
                unit_cost: l.unit_cost,
                source: MovementSource::Sale {
                    sale_id: self.id,
                    line_id: l.id,
                },
                actor: self.cashier,
                occurred_at: self.occurred_at,
                business_date: self.business_date,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::{AddVia, SellableVariant};
    use crate::pricing::{PricingPolicy, TaxMode, price_cart};
    use crate::settlement::{TenderInput, TenderKind, settle};
    use crate::{CurrencyCode, UuidV7Generator};

    fn variant(price: i64, track: bool) -> SellableVariant {
        let ids: &dyn IdGenerator = &UuidV7Generator;
        SellableVariant {
            variant_id: VariantId::generate(ids),
            product_id: ProductId::generate(ids),
            name: "Tea".into(),
            label: String::new(),
            sku: Some("TEA".into()),
            unit: Unit::Ea,
            price: Money::from_minor(price),
            taxes: vec![],
            track_stock: track,
            product_type: if track {
                ProductType::Stock
            } else {
                ProductType::Service
            },
            cost: Some(Money::from_minor(40)),
        }
    }

    fn header() -> SaleHeader {
        SaleHeader {
            mode: SaleMode::Sandbox,
            currency: Currency::new(CurrencyCode::new("GBP").unwrap(), 2).unwrap(),
            prices_include_tax: true,
            cashier: UserId::generate(&UuidV7Generator),
            occurred_at: Timestamp::from_millis(1),
            business_date: "2026-11-07".parse().unwrap(),
            timezone: "Europe/London".into(),
        }
    }

    #[test]
    fn a_sale_snapshot_adds_up_and_moves_only_tracked_stock() {
        let ids = UuidV7Generator;
        let mut cart = Cart::new(&ids);
        cart.add(
            variant(250, true),
            Quantity::from_whole(2).unwrap(),
            AddVia::Search,
            &ids,
        )
        .unwrap();
        cart.add(variant(500, false), Quantity::ONE, AddVia::Search, &ids)
            .unwrap();
        let policy = PricingPolicy::new(TaxMode::Inclusive);
        let priced = price_cart(&cart.pricing_lines(), None, &policy).unwrap();
        let tenders = [TenderInput {
            kind: TenderKind::Cash,
            amount: Money::from_minor(2000),
            external_reference: None,
        }];
        let settlement = settle(priced.total, &tenders, &policy).unwrap();
        let sale = SaleSnapshot::build(&cart, &priced, settlement, header(), &ids).unwrap();
        assert_eq!(
            (sale.total.minor(), sale.change.minor(), sale.lines.len()),
            (1000, 1000, 2)
        );
        assert_eq!(sale.lines[0].line_no, 1);
        let moves = sale.movements(LocationId::generate(&ids), &ids);
        assert_eq!(moves.len(), 1, "the service line moves no stock");
        assert_eq!(moves[0].delta.milli(), -2000);

        let mut broken = sale.clone();
        broken.change = Money::from_minor(999);
        assert!(matches!(broken.verify(), Err(SaleError::Inconsistent(_))));
        assert_eq!(
            SaleSnapshot::build(
                &Cart::new(&ids),
                &priced,
                settle(Money::from_minor(1), &tenders, &policy).unwrap(),
                header(),
                &ids
            ),
            Err(SaleError::Empty)
        );
    }
}
