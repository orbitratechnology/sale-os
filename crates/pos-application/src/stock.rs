//! Posting inventory documents (E11-T02/T03): opening stock, goods received, adjustments and
//! write-offs. One transaction holds the document, its movements (with the balances), the
//! audit row and the `inventory.document_posted` event. There is no command to edit or
//! delete a document: a mistake is corrected by a new one.

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, StockQueries};
use crate::write::{Counter, NumberSlot, inventory_document_aggregate};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::stock_docs::{DocumentDraft, DocumentSnapshot, DocumentType, StockItem, plan_document};
use pos_domain::{InventoryDocumentId, Permission, Quantity, VariantId};
use serde::Serialize;
use std::sync::Arc;

const DOCUMENT_NUMBER: NumberSlot = NumberSlot("inventory_document");

pub struct StockService {
    pub store: Arc<dyn LocalStore>,
    /// Variants (unit, stock tracking) and the business time zone.
    pub carts: Arc<dyn CartQueries>,
    pub stock: Arc<dyn StockQueries>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostedDocument {
    pub doc: DocumentSnapshot,
    /// `R01-IN-000001`.
    pub document_no: String,
}

/// The `inventory.document_posted` v1 payload (`documentNo`/`documentSeq` filled in the
/// transaction).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentPostedV1<'a> {
    #[serde(flatten)]
    doc: &'a DocumentSnapshot,
    location_id: pos_domain::LocationId,
    register_id: pos_domain::RegisterId,
    stock_movements: &'a [pos_domain::inventory::StockMovement],
}

/// Who may post which document (07 §1.2).
pub fn permission_for(doc_type: DocumentType) -> Permission {
    match doc_type {
        DocumentType::Opening | DocumentType::Receipt => Permission::InventoryReceive,
        DocumentType::Adjustment | DocumentType::WriteOff => Permission::InventoryAdjust,
    }
}

impl StockService {
    pub async fn post(&self, ctx: &CommandContext, draft: DocumentDraft) -> Result<PostedDocument, AppError> {
        ctx.authorize(permission_for(draft.doc_type))?;
        let actor = ctx.actor()?.clone();
        let settings = self.carts.pricing_settings().await?;
        let tz = pos_domain::time::parse_time_zone(&settings.timezone)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let today =
            pos_domain::time::business_date(ctx.now(), tz).map_err(|e| AppError::Internal(e.to_string()))?;

        let mut variants: Vec<VariantId> = draft.lines.iter().map(|l| l.variant_id).collect();
        variants.sort();
        variants.dedup();
        let damaged = self
            .stock
            .damaged_on_hand(ctx.register.location_id, &variants)
            .await?;
        let mut items = Vec::with_capacity(variants.len());
        for v in &variants {
            if let Some(s) = self.carts.sellable(*v, today).await? {
                items.push(StockItem {
                    variant_id: *v,
                    allows_fraction: s.unit.allows_fraction(),
                    track_stock: s.track_stock,
                    damaged: damaged.get(v).copied().unwrap_or(Quantity::ZERO),
                });
            }
        }
        let plan = plan_document(&draft, &items)?;
        let ids = ctx.ids.as_ref();
        let doc = DocumentSnapshot {
            id: InventoryDocumentId::generate(ids),
            plan,
            actor: actor.user_id,
            occurred_at: ctx.now(),
            business_date: today,
        };
        let movements = doc.movements(ctx.register.location_id, ids);
        let aggregate = inventory_document_aggregate(doc.id);
        let payload = DocumentPostedV1 {
            doc: &doc,
            location_id: ctx.register.location_id,
            register_id: ctx.register.register_id,
            stock_movements: &movements,
        };
        let event = OutboxDraft::new(
            ids,
            "inventory.document_posted",
            1,
            aggregate.clone(),
            None,
            &payload,
        )?
        .with_number(DOCUMENT_NUMBER, "documentSeq", "documentNo");
        let action = match doc.plan.doc_type {
            DocumentType::Opening => audit::INVENTORY_OPENING_POSTED,
            DocumentType::Receipt => audit::INVENTORY_RECEIPT_POSTED,
            DocumentType::Adjustment => audit::INVENTORY_ADJUSTMENT_POSTED,
            DocumentType::WriteOff => audit::INVENTORY_WRITE_OFF_POSTED,
        };
        let entry =
            AuditEntry::new(ids, action, "inventory_document", doc.id).with_metadata(serde_json::json!({
                "lines": draft.lines.len(),
                "reason": doc.plan.reason.map(|r| r.as_str()),
            }));
        let set = WriteSet::builder(ctx.write_context())
            .allocate_number(Counter::InventoryDocument, DOCUMENT_NUMBER)
            .insert_inventory_document(doc.clone(), DOCUMENT_NUMBER)
            .append_movements(movements, aggregate)
            .outbox(event)
            .audit(entry)
            .build()?;
        let applied = self.store.apply(set).await?;
        let document_no = applied
            .receipt_numbers
            .get(&DOCUMENT_NUMBER)
            .cloned()
            .ok_or_else(|| AppError::Internal("the store did not report the document number".into()))?;
        Ok(PostedDocument { doc, document_no })
    }
}
