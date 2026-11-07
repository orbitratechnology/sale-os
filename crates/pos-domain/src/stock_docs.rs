//! Inventory documents (E11-T02/T03, INV-002, INV-005, INV-006): opening stock, goods
//! received, adjustments and write-offs. A posted document is a fact: its movements are
//! appended to the ledger and never edited; a mistake is put right by a new document.

use crate::inventory::{MovementSource, MovementType, StockBucket, StockMovement};
use crate::{
    BusinessDate, ErrorCode, HasErrorCode, IdGenerator, InventoryDocumentId, LocationId, Money, MovementId,
    Quantity, Timestamp, UserId, VariantId,
};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum DocumentType {
    /// First stock count when starting to use the till: + sellable.
    Opening,
    /// Goods received from a supplier: + sellable.
    Receipt,
    /// A correction with a reason; see [`AdjustReason`] for which way stock moves.
    Adjustment,
    /// Damaged stock thrown away: − damaged.
    WriteOff,
}

impl DocumentType {
    pub const fn as_str(self) -> &'static str {
        match self {
            DocumentType::Opening => "opening",
            DocumentType::Receipt => "receipt",
            DocumentType::Adjustment => "adjustment",
            DocumentType::WriteOff => "write_off",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum AdjustReason {
    /// Sellable → damaged (kept, not sellable).
    Damage,
    /// − sellable.
    Loss,
    /// − sellable.
    Theft,
    /// ± sellable: the count differs from the record.
    CountCorrection,
    /// − sellable.
    Expired,
    /// ± sellable; a note is required.
    Other,
}

impl AdjustReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            AdjustReason::Damage => "damage",
            AdjustReason::Loss => "loss",
            AdjustReason::Theft => "theft",
            AdjustReason::CountCorrection => "count_correction",
            AdjustReason::Expired => "expired",
            AdjustReason::Other => "other",
        }
    }
}

/// One line as entered: for most documents a positive quantity; for a count correction or
/// "other" adjustment a signed one (− removes stock).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentLine {
    pub variant_id: VariantId,
    pub quantity: Quantity,
    /// What one unit cost (receipts and opening stock only).
    pub unit_cost: Option<Money>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentDraft {
    pub doc_type: DocumentType,
    pub reason: Option<AdjustReason>,
    pub supplier_name: Option<String>,
    pub reference: Option<String>,
    pub note: Option<String>,
    pub lines: Vec<DocumentLine>,
}

/// What the document needs to know about each variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StockItem {
    pub variant_id: VariantId,
    pub allows_fraction: bool,
    pub track_stock: bool,
    /// Current damaged on-hand (a write-off cannot take more than this).
    pub damaged: Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedMovement {
    pub variant_id: VariantId,
    pub bucket: StockBucket,
    pub movement_type: MovementType,
    pub delta: Quantity,
    pub unit_cost: Option<Money>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    #[error("a document needs at least one line")]
    NoLines,
    #[error("quantities must be more than zero")]
    QuantityNotPositive { variant: VariantId },
    #[error("a quantity of zero changes nothing")]
    ZeroQuantity { variant: VariantId },
    #[error("an adjustment needs a reason")]
    ReasonRequired,
    #[error("an adjustment for another reason needs a note")]
    NoteRequired,
    #[error("only adjustments and write-offs have a reason")]
    ReasonNotAllowed,
    #[error("a supplier and costs belong on goods received or opening stock")]
    SupplierNotAllowed,
    #[error("that item is not in the catalogue")]
    UnknownVariant { variant: VariantId },
    #[error("that item does not track stock")]
    NotStockTracked { variant: VariantId },
    #[error("only {available_milli} milli-units are in damaged stock")]
    WriteOffExceedsDamaged {
        variant: VariantId,
        available_milli: i64,
    },
    #[error("a unit cost cannot be negative")]
    NegativeCost,
    #[error("{field} is too long")]
    TooLong { field: &'static str },
    #[error(transparent)]
    Quantity(#[from] crate::quantity::QuantityError),
}

impl HasErrorCode for DocumentError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ValidationFailed
    }
}

fn text(v: &Option<String>, field: &'static str) -> Result<Option<String>, DocumentError> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.chars().count() > 200 => Err(DocumentError::TooLong { field }),
        Some(s) => Ok(Some(s.to_owned())),
    }
}

/// A validated document, ready to post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentPlan {
    pub doc_type: DocumentType,
    pub reason: Option<AdjustReason>,
    pub supplier_name: Option<String>,
    pub reference: Option<String>,
    pub note: Option<String>,
    pub movements: Vec<PlannedMovement>,
}

/// INV-002/005/006: checks a draft and works out its movements.
pub fn plan_document(draft: &DocumentDraft, items: &[StockItem]) -> Result<DocumentPlan, DocumentError> {
    use DocumentType::*;
    if draft.lines.is_empty() {
        return Err(DocumentError::NoLines);
    }
    let note = text(&draft.note, "note")?;
    let supplier_name = text(&draft.supplier_name, "supplier")?;
    let reference = text(&draft.reference, "reference")?;
    let reason = match (draft.doc_type, draft.reason) {
        (Adjustment, None) => return Err(DocumentError::ReasonRequired),
        (Adjustment, Some(AdjustReason::Other)) if note.is_none() => return Err(DocumentError::NoteRequired),
        (Opening | Receipt, Some(_)) => return Err(DocumentError::ReasonNotAllowed),
        (_, r) => r,
    };
    let costs_allowed = matches!(draft.doc_type, Opening | Receipt);
    if !costs_allowed && (supplier_name.is_some() || draft.lines.iter().any(|l| l.unit_cost.is_some())) {
        return Err(DocumentError::SupplierNotAllowed);
    }
    let signed = matches!(reason, Some(AdjustReason::CountCorrection | AdjustReason::Other));

    let mut movements = Vec::new();
    // Write-offs per variant so far in this document, against the damaged balance.
    let mut written_off: Vec<(VariantId, i64)> = Vec::new();
    for line in &draft.lines {
        let item =
            items
                .iter()
                .find(|i| i.variant_id == line.variant_id)
                .ok_or(DocumentError::UnknownVariant {
                    variant: line.variant_id,
                })?;
        if !item.track_stock {
            return Err(DocumentError::NotStockTracked {
                variant: line.variant_id,
            });
        }
        let q = line.quantity;
        if q.milli() == 0 {
            return Err(DocumentError::ZeroQuantity {
                variant: line.variant_id,
            });
        }
        if q.milli() < 0 && !signed {
            return Err(DocumentError::QuantityNotPositive {
                variant: line.variant_id,
            });
        }
        Quantity::from_milli(q.milli().abs()).validate_for_unit(item.allows_fraction)?;
        if line.unit_cost.is_some_and(|c| c.minor() < 0) {
            return Err(DocumentError::NegativeCost);
        }
        let mv = |bucket, movement_type, delta: i64| PlannedMovement {
            variant_id: line.variant_id,
            bucket,
            movement_type,
            delta: Quantity::from_milli(delta),
            unit_cost: line.unit_cost,
        };
        match (draft.doc_type, reason) {
            (Opening, _) => movements.push(mv(StockBucket::Sellable, MovementType::Opening, q.milli())),
            (Receipt, _) => movements.push(mv(StockBucket::Sellable, MovementType::Receipt, q.milli())),
            (WriteOff, _) => {
                let done = written_off.iter_mut().find(|(v, _)| *v == line.variant_id);
                let before = done.as_ref().map_or(0, |(_, n)| *n);
                let available = item.damaged.milli() - before;
                if q.milli() > available {
                    return Err(DocumentError::WriteOffExceedsDamaged {
                        variant: line.variant_id,
                        available_milli: available.max(0),
                    });
                }
                match done {
                    Some((_, n)) => *n += q.milli(),
                    None => written_off.push((line.variant_id, q.milli())),
                }
                movements.push(mv(StockBucket::Damaged, MovementType::WriteOff, -q.milli()));
            }
            (Adjustment, Some(AdjustReason::Damage)) => {
                movements.push(mv(StockBucket::Sellable, MovementType::Damage, -q.milli()));
                movements.push(mv(StockBucket::Damaged, MovementType::Damage, q.milli()));
            }
            (Adjustment, Some(AdjustReason::Loss | AdjustReason::Theft | AdjustReason::Expired)) => {
                movements.push(mv(StockBucket::Sellable, MovementType::Loss, -q.milli()))
            }
            (Adjustment, Some(AdjustReason::CountCorrection)) => {
                movements.push(mv(StockBucket::Sellable, MovementType::Correction, q.milli()))
            }
            (Adjustment, Some(AdjustReason::Other)) => {
                movements.push(mv(StockBucket::Sellable, MovementType::Adjustment, q.milli()))
            }
            (Adjustment, None) => unreachable!("checked above"),
        }
    }
    Ok(DocumentPlan {
        doc_type: draft.doc_type,
        reason,
        supplier_name,
        reference,
        note,
        movements,
    })
}

/// A posted document: the header and its movements. The number is given by the store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSnapshot {
    pub id: InventoryDocumentId,
    #[serde(flatten)]
    pub plan: DocumentPlan,
    pub actor: UserId,
    pub occurred_at: Timestamp,
    pub business_date: BusinessDate,
}

impl DocumentSnapshot {
    pub fn movements(&self, location: LocationId, ids: &dyn IdGenerator) -> Vec<StockMovement> {
        self.plan
            .movements
            .iter()
            .map(|m| StockMovement {
                id: MovementId::generate(ids),
                location_id: location,
                variant_id: m.variant_id,
                bucket: m.bucket,
                movement_type: m.movement_type,
                delta: m.delta,
                unit_cost: m.unit_cost,
                source: MovementSource::InventoryDocument { document_id: self.id },
                actor: self.actor,
                occurred_at: self.occurred_at,
                business_date: self.business_date,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UuidV7Generator;

    fn item(fraction: bool, damaged: i64) -> StockItem {
        StockItem {
            variant_id: VariantId::generate(&UuidV7Generator),
            allows_fraction: fraction,
            track_stock: true,
            damaged: Quantity::from_milli(damaged),
        }
    }

    fn line(i: &StockItem, milli: i64) -> DocumentLine {
        DocumentLine {
            variant_id: i.variant_id,
            quantity: Quantity::from_milli(milli),
            unit_cost: None,
        }
    }

    fn draft(
        doc_type: DocumentType,
        reason: Option<AdjustReason>,
        lines: Vec<DocumentLine>,
    ) -> DocumentDraft {
        DocumentDraft {
            doc_type,
            reason,
            supplier_name: None,
            reference: None,
            note: None,
            lines,
        }
    }

    fn moves(p: &DocumentPlan) -> Vec<(StockBucket, MovementType, i64)> {
        p.movements
            .iter()
            .map(|m| (m.bucket, m.movement_type, m.delta.milli()))
            .collect()
    }

    #[test]
    fn receipts_and_opening_add_sellable_stock_with_costs() {
        let (a, b) = (item(false, 0), item(true, 0));
        let mut d = draft(
            DocumentType::Receipt,
            None,
            vec![line(&a, 12_000), line(&b, 2_500)],
        );
        d.lines[0].unit_cost = Some(Money::from_minor(85));
        d.supplier_name = Some("  Wholesale Ltd ".into());
        let p = plan_document(&d, &[a.clone(), b.clone()]).unwrap();
        assert_eq!(
            moves(&p),
            [
                (StockBucket::Sellable, MovementType::Receipt, 12_000),
                (StockBucket::Sellable, MovementType::Receipt, 2_500)
            ]
        );
        assert_eq!(p.supplier_name.as_deref(), Some("Wholesale Ltd"));
        assert_eq!(p.movements[0].unit_cost, Some(Money::from_minor(85)));
        let o = plan_document(
            &draft(DocumentType::Opening, None, vec![line(&a, 3000)]),
            std::slice::from_ref(&a),
        )
        .unwrap();
        assert_eq!(moves(&o), [(StockBucket::Sellable, MovementType::Opening, 3000)]);
        // Refusals: ≤ 0, fractions of "each", reasons, unknown or untracked items.
        let e = |d: DocumentDraft, items: &[StockItem]| plan_document(&d, items).unwrap_err();
        assert_eq!(
            e(
                draft(DocumentType::Receipt, None, vec![line(&a, -1000)]),
                std::slice::from_ref(&a)
            ),
            DocumentError::QuantityNotPositive {
                variant: a.variant_id
            }
        );
        assert_eq!(
            e(
                draft(DocumentType::Receipt, None, vec![line(&a, 0)]),
                std::slice::from_ref(&a)
            ),
            DocumentError::ZeroQuantity {
                variant: a.variant_id
            }
        );
        assert!(matches!(
            e(
                draft(DocumentType::Receipt, None, vec![line(&a, 500)]),
                std::slice::from_ref(&a)
            ),
            DocumentError::Quantity(_)
        ));
        assert_eq!(
            e(
                draft(
                    DocumentType::Receipt,
                    Some(AdjustReason::Loss),
                    vec![line(&a, 1000)]
                ),
                std::slice::from_ref(&a)
            ),
            DocumentError::ReasonNotAllowed
        );
        assert_eq!(
            e(draft(DocumentType::Receipt, None, vec![]), &[]),
            DocumentError::NoLines
        );
        assert_eq!(
            e(draft(DocumentType::Receipt, None, vec![line(&a, 1000)]), &[]),
            DocumentError::UnknownVariant {
                variant: a.variant_id
            }
        );
        let mut service = a.clone();
        service.track_stock = false;
        assert_eq!(
            e(
                draft(DocumentType::Receipt, None, vec![line(&a, 1000)]),
                &[service]
            ),
            DocumentError::NotStockTracked {
                variant: a.variant_id
            }
        );
    }

    #[test]
    fn adjustments_move_stock_by_reason() {
        let a = item(false, 0);
        let p = |reason, milli| {
            plan_document(
                &draft(DocumentType::Adjustment, Some(reason), vec![line(&a, milli)]),
                std::slice::from_ref(&a),
            )
        };
        assert_eq!(
            moves(&p(AdjustReason::Damage, 2000).unwrap()),
            [
                (StockBucket::Sellable, MovementType::Damage, -2000),
                (StockBucket::Damaged, MovementType::Damage, 2000)
            ]
        );
        for r in [AdjustReason::Loss, AdjustReason::Theft, AdjustReason::Expired] {
            assert_eq!(
                moves(&p(r, 1000).unwrap()),
                [(StockBucket::Sellable, MovementType::Loss, -1000)]
            );
            assert_eq!(
                p(r, -1000).unwrap_err(),
                DocumentError::QuantityNotPositive {
                    variant: a.variant_id
                }
            );
        }
        // A count correction goes either way.
        assert_eq!(
            moves(&p(AdjustReason::CountCorrection, -3000).unwrap()),
            [(StockBucket::Sellable, MovementType::Correction, -3000)]
        );
        assert_eq!(
            moves(&p(AdjustReason::CountCorrection, 1000).unwrap()),
            [(StockBucket::Sellable, MovementType::Correction, 1000)]
        );
        // "Other" needs a note; no reason at all is refused; no supplier or cost on adjustments.
        assert_eq!(
            p(AdjustReason::Other, 1000).unwrap_err(),
            DocumentError::NoteRequired
        );
        let mut other = draft(
            DocumentType::Adjustment,
            Some(AdjustReason::Other),
            vec![line(&a, -1000)],
        );
        other.note = Some("used for display".into());
        assert_eq!(
            moves(&plan_document(&other, std::slice::from_ref(&a)).unwrap()),
            [(StockBucket::Sellable, MovementType::Adjustment, -1000)]
        );
        assert_eq!(
            plan_document(
                &draft(DocumentType::Adjustment, None, vec![line(&a, 1000)]),
                std::slice::from_ref(&a)
            )
            .unwrap_err(),
            DocumentError::ReasonRequired
        );
        let mut costed = draft(
            DocumentType::Adjustment,
            Some(AdjustReason::Loss),
            vec![line(&a, 1000)],
        );
        costed.lines[0].unit_cost = Some(Money::from_minor(1));
        assert_eq!(
            plan_document(&costed, std::slice::from_ref(&a)).unwrap_err(),
            DocumentError::SupplierNotAllowed
        );
    }

    #[test]
    fn write_offs_take_damaged_stock_and_no_more() {
        let a = item(false, 3000);
        let wo = |lines| plan_document(&draft(DocumentType::WriteOff, None, lines), std::slice::from_ref(&a));
        assert_eq!(
            moves(&wo(vec![line(&a, 2000)]).unwrap()),
            [(StockBucket::Damaged, MovementType::WriteOff, -2000)]
        );
        // Two lines of the same item count together.
        assert_eq!(
            wo(vec![line(&a, 2000), line(&a, 2000)]).unwrap_err(),
            DocumentError::WriteOffExceedsDamaged {
                variant: a.variant_id,
                available_milli: 1000
            }
        );
        assert!(wo(vec![line(&a, 2000), line(&a, 1000)]).is_ok());
    }
}
