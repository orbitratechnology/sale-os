//! The draft cart (E09-T01, POS-001..003, BAR-005). Local only, mutable, never synced
//! (ADR-009). Every mutation bumps `revision`; the UI sends the revision back with
//! destructive commands and completion so a stale screen cannot act on a changed cart.

use crate::catalog::Unit;
use crate::pricing::{Discount, PricingLine, TaxRateSnapshot};
use crate::quantity::QuantityError;
use crate::{
    ArithmeticError, CartId, CartLineId, ErrorCode, HasErrorCode, IdGenerator, Money, ProductId, Quantity,
    ScanEventId, VariantId,
};
use serde::{Deserialize, Serialize};

/// Everything needed to price and show a line without another lookup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SellableVariant {
    pub variant_id: VariantId,
    pub product_id: ProductId,
    pub name: String,
    pub label: String,
    pub sku: Option<String>,
    pub unit: Unit,
    pub price: Money,
    pub taxes: Vec<TaxRateSnapshot>,
    pub track_stock: bool,
    /// Copied onto the sale line (POS-010).
    pub product_type: crate::catalog::ProductType,
    /// Cost at the time of sale, for margin reports later (REP-007).
    pub cost: Option<Money>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AddVia {
    Scan { event_id: ScanEventId },
    Search,
    QuickSelect,
    ManualCode,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CartStatus {
    Open,
    Parked,
    Voided,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CartLine {
    pub id: CartLineId,
    pub variant: SellableVariant,
    pub quantity: Quantity,
    pub price_override: Option<Money>,
    pub line_discount: Option<Discount>,
    pub added_via: AddVia,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cart {
    pub id: CartId,
    pub status: CartStatus,
    pub lines: Vec<CartLine>,
    pub order_discount: Option<Discount>,
    pub revision: u64,
    pub void_reason: Option<String>,
}

/// What a mutation did, for the UI to highlight and for audit decisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CartChange {
    LineAdded(CartLineId),
    QuantityIncreased { line: CartLineId, to: Quantity },
    QuantitySet(CartLineId),
    LineRemoved(CartLineId),
    DiscountChanged,
    PriceOverridden(CartLineId),
    Cleared { lines: usize },
    Voided,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CartError {
    #[error("the cart is not open")]
    NotOpen,
    #[error("cart line not found")]
    LineNotFound,
    #[error(transparent)]
    Quantity(#[from] QuantityError),
    #[error("the cart is empty")]
    Empty,
    #[error("the cart changed (revision {actual}, expected {expected}); refresh and try again")]
    RevisionMismatch { expected: u64, actual: u64 },
    #[error("a reason is required")]
    ReasonRequired,
    #[error("prices cannot be negative")]
    NegativePrice,
    #[error(transparent)]
    Arithmetic(#[from] ArithmeticError),
}

impl HasErrorCode for CartError {
    fn error_code(&self) -> ErrorCode {
        match self {
            CartError::Empty => ErrorCode::CartEmpty,
            CartError::RevisionMismatch { .. } => ErrorCode::CartRevisionMismatch,
            _ => ErrorCode::ValidationFailed,
        }
    }
}

impl Cart {
    pub fn new(ids: &dyn IdGenerator) -> Self {
        Self {
            id: CartId::generate(ids),
            status: CartStatus::Open,
            lines: Vec::new(),
            order_discount: None,
            revision: 0,
            void_reason: None,
        }
    }

    fn ensure_open(&self) -> Result<(), CartError> {
        if self.status == CartStatus::Open {
            Ok(())
        } else {
            Err(CartError::NotOpen)
        }
    }

    /// Guards destructive commands against a stale UI.
    pub fn check_revision(&self, expected: u64) -> Result<(), CartError> {
        if self.revision == expected {
            Ok(())
        } else {
            Err(CartError::RevisionMismatch {
                expected,
                actual: self.revision,
            })
        }
    }

    fn touch(&mut self) {
        self.revision += 1;
    }

    fn line_mut(&mut self, id: CartLineId) -> Result<&mut CartLine, CartError> {
        self.lines
            .iter_mut()
            .find(|l| l.id == id)
            .ok_or(CartError::LineNotFound)
    }

    /// Adds `qty` of a variant (POS-002). BAR-005: a repeated scan or add of the same variant
    /// merges into the **last** line for it, unless that line has a manual price or
    /// discount, in which case a new line keeps those adjustments isolated.
    pub fn add(
        &mut self,
        variant: SellableVariant,
        qty: Quantity,
        via: AddVia,
        ids: &dyn IdGenerator,
    ) -> Result<CartChange, CartError> {
        self.ensure_open()?;
        qty.validate_for_unit(variant.unit.allows_fraction())?;
        if let Some(last) = self
            .lines
            .iter_mut()
            .rev()
            .find(|l| l.variant.variant_id == variant.variant_id)
            && last.price_override.is_none()
            && last.line_discount.is_none()
        {
            last.quantity = last.quantity.checked_add(qty)?;
            // Keep the freshest catalogue snapshot (price edits made while the cart is open).
            last.variant = variant;
            let (line, to) = (last.id, last.quantity);
            self.touch();
            return Ok(CartChange::QuantityIncreased { line, to });
        }
        let id = CartLineId::generate(ids);
        self.lines.push(CartLine {
            id,
            variant,
            quantity: qty,
            price_override: None,
            line_discount: None,
            added_via: via,
        });
        self.touch();
        Ok(CartChange::LineAdded(id))
    }

    /// POS-003. Zero is not a quantity: use `remove_line`.
    pub fn set_quantity(&mut self, line: CartLineId, qty: Quantity) -> Result<CartChange, CartError> {
        self.ensure_open()?;
        let l = self.line_mut(line)?;
        qty.validate_for_unit(l.variant.unit.allows_fraction())?;
        l.quantity = qty;
        self.touch();
        Ok(CartChange::QuantitySet(line))
    }

    pub fn remove_line(&mut self, line: CartLineId) -> Result<CartChange, CartError> {
        self.ensure_open()?;
        let before = self.lines.len();
        self.lines.retain(|l| l.id != line);
        if self.lines.len() == before {
            return Err(CartError::LineNotFound);
        }
        if self.lines.is_empty() {
            self.order_discount = None;
        }
        self.touch();
        Ok(CartChange::LineRemoved(line))
    }

    /// Discount amounts are validated by the pricing engine; permissions and approvals
    /// by the application layer (E08-T02).
    pub fn set_line_discount(
        &mut self,
        line: CartLineId,
        discount: Option<Discount>,
    ) -> Result<CartChange, CartError> {
        self.ensure_open()?;
        self.line_mut(line)?.line_discount = discount;
        self.touch();
        Ok(CartChange::DiscountChanged)
    }

    pub fn set_order_discount(&mut self, discount: Option<Discount>) -> Result<CartChange, CartError> {
        self.ensure_open()?;
        self.order_discount = discount;
        self.touch();
        Ok(CartChange::DiscountChanged)
    }

    pub fn override_price(&mut self, line: CartLineId, price: Money) -> Result<CartChange, CartError> {
        self.ensure_open()?;
        if price.is_negative() {
            return Err(CartError::NegativePrice);
        }
        self.line_mut(line)?.price_override = Some(price);
        self.touch();
        Ok(CartChange::PriceOverridden(line))
    }

    /// POS-003: removes every line and the order discount; the cart stays open.
    pub fn clear(&mut self, expected_revision: u64) -> Result<CartChange, CartError> {
        self.ensure_open()?;
        self.check_revision(expected_revision)?;
        let lines = self.lines.len();
        self.lines.clear();
        self.order_discount = None;
        self.touch();
        Ok(CartChange::Cleared { lines })
    }

    /// RET-001: abandons the draft with a reason. Nothing is sold, no stock moves.
    pub fn void(&mut self, expected_revision: u64, reason: &str) -> Result<CartChange, CartError> {
        self.ensure_open()?;
        self.check_revision(expected_revision)?;
        let reason = reason.trim();
        if reason.is_empty() && !self.lines.is_empty() {
            return Err(CartError::ReasonRequired);
        }
        self.status = CartStatus::Voided;
        self.void_reason = (!reason.is_empty()).then(|| reason.to_owned());
        self.touch();
        Ok(CartChange::Voided)
    }

    /// Input for `pricing::price_cart`.
    pub fn pricing_lines(&self) -> Vec<PricingLine> {
        self.lines
            .iter()
            .map(|l| PricingLine {
                line_id: l.id,
                quantity: l.quantity,
                unit_price: l.price_override.unwrap_or(l.variant.price),
                line_discount: l.line_discount.clone(),
                taxes: l.variant.taxes.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{PricingPolicy, TaxMode, price_cart};
    use crate::{RatePpm, UuidV7Generator};

    fn ids() -> UuidV7Generator {
        UuidV7Generator
    }

    fn sellable(price: i64, unit: Unit) -> SellableVariant {
        SellableVariant {
            variant_id: VariantId::generate(&ids()),
            product_id: ProductId::generate(&ids()),
            name: "Cola".into(),
            label: String::new(),
            sku: None,
            unit,
            price: Money::from_minor(price),
            taxes: vec![],
            track_stock: true,
            product_type: crate::catalog::ProductType::Stock,
            cost: None,
        }
    }

    fn scan() -> AddVia {
        AddVia::Scan {
            event_id: ScanEventId::generate(&ids()),
        }
    }

    #[test]
    fn repeated_scans_merge_into_the_last_line() {
        let mut c = Cart::new(&ids());
        let cola = sellable(120, Unit::Ea);
        for _ in 0..10 {
            c.add(cola.clone(), Quantity::ONE, scan(), &ids()).unwrap();
        }
        assert_eq!(c.lines.len(), 1);
        assert_eq!(c.lines[0].quantity.milli(), 10_000);
        assert_eq!(c.revision, 10);
        // A pack scan of 12 then merges too: 22.
        c.add(cola, Quantity::from_whole(12).unwrap(), scan(), &ids())
            .unwrap();
        assert_eq!(c.lines[0].quantity.milli(), 22_000);
    }

    #[test]
    fn a_discounted_or_overridden_line_is_not_merged_into() {
        let mut c = Cart::new(&ids());
        let cola = sellable(120, Unit::Ea);
        c.add(cola.clone(), Quantity::ONE, scan(), &ids()).unwrap();
        let first = c.lines[0].id;
        c.set_line_discount(first, Some(Discount::percent(RatePpm::new(100_000).unwrap())))
            .unwrap();
        assert!(matches!(
            c.add(cola.clone(), Quantity::ONE, scan(), &ids()).unwrap(),
            CartChange::LineAdded(_)
        ));
        assert_eq!(c.lines.len(), 2);
        // The new undiscounted line is now the merge target.
        c.add(cola, Quantity::ONE, scan(), &ids()).unwrap();
        assert_eq!(
            c.lines.iter().map(|l| l.quantity.milli()).collect::<Vec<_>>(),
            vec![1000, 2000]
        );
    }

    #[test]
    fn quantities_follow_the_unit() {
        let mut c = Cart::new(&ids());
        let half = Quantity::parse_decimal("0.5").unwrap();
        assert!(matches!(
            c.add(sellable(120, Unit::Ea), half, AddVia::Search, &ids()),
            Err(CartError::Quantity(_))
        ));
        c.add(
            sellable(1249, Unit::Kg),
            Quantity::parse_decimal("0.333").unwrap(),
            AddVia::ManualCode,
            &ids(),
        )
        .unwrap();
        let line = c.lines[0].id;
        assert!(matches!(
            c.set_quantity(line, Quantity::ZERO),
            Err(CartError::Quantity(QuantityError::NotPositive))
        ));
        c.set_quantity(line, Quantity::parse_decimal("1.25").unwrap())
            .unwrap();
        assert_eq!(c.lines[0].quantity.milli(), 1250);
    }

    #[test]
    fn clear_and_void_check_the_revision() {
        let mut c = Cart::new(&ids());
        c.add(sellable(120, Unit::Ea), Quantity::ONE, scan(), &ids())
            .unwrap();
        let err = c.clear(0).unwrap_err();
        assert_eq!(
            err,
            CartError::RevisionMismatch {
                expected: 0,
                actual: 1
            }
        );
        assert_eq!(err.error_code(), ErrorCode::CartRevisionMismatch);
        assert_eq!(c.clear(1).unwrap(), CartChange::Cleared { lines: 1 });
        assert!(c.lines.is_empty() && c.status == CartStatus::Open);
        c.add(sellable(120, Unit::Ea), Quantity::ONE, scan(), &ids())
            .unwrap();
        assert_eq!(c.void(c.revision, "  "), Err(CartError::ReasonRequired));
        c.void(c.revision, "customer left").unwrap();
        assert_eq!(c.status, CartStatus::Voided);
        assert_eq!(
            c.add(sellable(1, Unit::Ea), Quantity::ONE, scan(), &ids()),
            Err(CartError::NotOpen)
        );
    }

    #[test]
    fn prices_through_the_pricing_engine_with_overrides() {
        let mut c = Cart::new(&ids());
        c.add(
            sellable(999, Unit::Ea),
            Quantity::from_whole(3).unwrap(),
            AddVia::Search,
            &ids(),
        )
        .unwrap();
        let line = c.lines[0].id;
        c.override_price(line, Money::from_minor(899)).unwrap();
        assert_eq!(
            c.override_price(line, Money::from_minor(-1)),
            Err(CartError::NegativePrice)
        );
        let priced = price_cart(
            &c.pricing_lines(),
            c.order_discount.as_ref(),
            &PricingPolicy::new(TaxMode::Inclusive),
        )
        .unwrap();
        assert_eq!(priced.total.minor(), 2697);
    }

    #[test]
    fn removing_the_last_line_drops_the_order_discount() {
        let mut c = Cart::new(&ids());
        c.add(sellable(100, Unit::Ea), Quantity::ONE, AddVia::Search, &ids())
            .unwrap();
        c.set_order_discount(Some(Discount::fixed(Money::from_minor(10))))
            .unwrap();
        let line = c.lines[0].id;
        c.remove_line(line).unwrap();
        assert!(c.order_discount.is_none());
        assert_eq!(c.remove_line(line), Err(CartError::LineNotFound));
    }
}
