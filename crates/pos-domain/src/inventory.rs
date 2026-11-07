//! The stock ledger (E11-T01..T04, INV-001..008, ADR-020). Movements are append-only facts;
//! balances are a projection of them.

use crate::{
    BusinessDate, ErrorCode, HasErrorCode, LocationId, Money, MovementId, Quantity, Timestamp, UserId,
    VariantId,
};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StockBucket {
    Sellable,
    Damaged,
}

impl StockBucket {
    pub const fn as_str(self) -> &'static str {
        match self {
            StockBucket::Sellable => "sellable",
            StockBucket::Damaged => "damaged",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MovementType {
    Opening,
    Receipt,
    Sale,
    ReturnRestock,
    ReturnDamaged,
    Damage,
    Loss,
    Adjustment,
    WriteOff,
    Correction,
}

impl MovementType {
    pub const fn as_str(self) -> &'static str {
        match self {
            MovementType::Opening => "opening",
            MovementType::Receipt => "receipt",
            MovementType::Sale => "sale",
            MovementType::ReturnRestock => "return_restock",
            MovementType::ReturnDamaged => "return_damaged",
            MovementType::Damage => "damage",
            MovementType::Loss => "loss",
            MovementType::Adjustment => "adjustment",
            MovementType::WriteOff => "write_off",
            MovementType::Correction => "correction",
        }
    }
}

/// What caused a movement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MovementSource {
    Sale {
        sale_id: crate::SaleId,
        line_id: crate::SaleLineId,
    },
    InventoryDocument {
        document_id: crate::InventoryDocumentId,
    },
}

impl MovementSource {
    /// `(source_type, source_id, source_line_id)` as stored.
    pub fn columns(&self) -> (&'static str, String, Option<String>) {
        match self {
            MovementSource::Sale { sale_id, line_id } => {
                ("sale", sale_id.to_string(), Some(line_id.to_string()))
            }
            MovementSource::InventoryDocument { document_id } => {
                ("inventory_document", document_id.to_string(), None)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StockMovement {
    pub id: MovementId,
    pub location_id: LocationId,
    pub variant_id: VariantId,
    pub bucket: StockBucket,
    pub movement_type: MovementType,
    /// Signed: a sale is negative.
    pub delta: Quantity,
    pub unit_cost: Option<Money>,
    pub source: MovementSource,
    pub actor: UserId,
    pub occurred_at: Timestamp,
    pub business_date: BusinessDate,
}

/// `inventory.allow_negative` (INV-008): sell anyway and flag it, or refuse the sale.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NegativeStockPolicy {
    AllowAndFlag,
    Block,
}

/// A variant a set of movements would take below zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NegativeStockFlag {
    pub variant_id: VariantId,
    pub resulting: Quantity,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InventoryError {
    #[error("not enough stock for {} item(s), and the policy blocks negative stock", .0.len())]
    NegativeBlocked(Vec<NegativeStockFlag>),
    #[error("quantity out of range")]
    Overflow,
}

impl HasErrorCode for InventoryError {
    fn error_code(&self) -> ErrorCode {
        match self {
            InventoryError::NegativeBlocked(_) => ErrorCode::StockNegativeBlocked,
            InventoryError::Overflow => ErrorCode::ValidationFailed,
        }
    }
}

/// The variants `movements` would take below zero in the sellable bucket, given `on_hand`
/// (missing = 0). `Block` refuses them; `AllowAndFlag` returns them for the audit trail.
pub fn check_negative(
    policy: NegativeStockPolicy,
    on_hand: &BTreeMap<VariantId, Quantity>,
    movements: &[StockMovement],
) -> Result<Vec<NegativeStockFlag>, InventoryError> {
    let mut after: BTreeMap<VariantId, Quantity> = BTreeMap::new();
    for m in movements.iter().filter(|m| m.bucket == StockBucket::Sellable) {
        let current = after
            .get(&m.variant_id)
            .or_else(|| on_hand.get(&m.variant_id))
            .copied()
            .unwrap_or(Quantity::ZERO);
        let next = current
            .checked_add(m.delta)
            .map_err(|_| InventoryError::Overflow)?;
        after.insert(m.variant_id, next);
    }
    let flags: Vec<NegativeStockFlag> = after
        .into_iter()
        .filter(|(_, q)| q.milli() < 0)
        .map(|(variant_id, resulting)| NegativeStockFlag {
            variant_id,
            resulting,
        })
        .collect();
    match policy {
        NegativeStockPolicy::Block if !flags.is_empty() => Err(InventoryError::NegativeBlocked(flags)),
        _ => Ok(flags),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IdGenerator, UuidV7Generator};

    fn sale(variant: VariantId, milli: i64) -> StockMovement {
        let ids: &dyn IdGenerator = &UuidV7Generator;
        StockMovement {
            id: MovementId::generate(ids),
            location_id: LocationId::generate(ids),
            variant_id: variant,
            bucket: StockBucket::Sellable,
            movement_type: MovementType::Sale,
            delta: Quantity::from_milli(-milli),
            unit_cost: None,
            source: MovementSource::Sale {
                sale_id: crate::SaleId::generate(ids),
                line_id: crate::SaleLineId::generate(ids),
            },
            actor: UserId::generate(ids),
            occurred_at: Timestamp::from_millis(0),
            business_date: "2026-11-07".parse().unwrap(),
        }
    }

    #[test]
    fn flags_or_blocks_what_would_go_negative() {
        let ids: &dyn IdGenerator = &UuidV7Generator;
        let (a, b) = (VariantId::generate(ids), VariantId::generate(ids));
        let on_hand = BTreeMap::from([(a, Quantity::from_milli(2000)), (b, Quantity::from_milli(5000))]);
        // Two lines of the same variant add up: 2 on hand, 1 + 2 sold.
        let ms = [sale(a, 1000), sale(a, 2000), sale(b, 1000)];
        let flags = check_negative(NegativeStockPolicy::AllowAndFlag, &on_hand, &ms).unwrap();
        assert_eq!(
            flags,
            vec![NegativeStockFlag {
                variant_id: a,
                resulting: Quantity::from_milli(-1000)
            }]
        );
        let err = check_negative(NegativeStockPolicy::Block, &on_hand, &ms).unwrap_err();
        assert_eq!(err.error_code(), ErrorCode::StockNegativeBlocked);
        // Unknown balance counts as zero.
        let c = VariantId::generate(ids);
        assert_eq!(
            check_negative(NegativeStockPolicy::AllowAndFlag, &on_hand, &[sale(c, 1)])
                .unwrap()
                .len(),
            1
        );
        assert!(
            check_negative(NegativeStockPolicy::Block, &on_hand, &[sale(b, 5000)])
                .unwrap()
                .is_empty()
        );
    }
}
