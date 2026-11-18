//! Re-creating a restored register's own events from the cloud (E19-T03).
//!
//! After a restore, the backup lacks what the register did since. The cloud has it: each
//! event as pushed. For every one, in order, this builds the write set the original command
//! built (the same rows, receipt and stock movements) and marks it as a replay, so the store
//! gives it its original numbers and writes the original event, already handled. The register
//! is then as it was, numbering carries on after the last receipt the cloud has, and nothing
//! is sent twice.
//!
//! Master data (products, prices, staff, settings) comes back with the bootstrap that follows a
//! restore; its events are only put back in the outbox here.

use crate::LocalStore;
use crate::audit::{self, AuditEntry};
use crate::checkout::receipt_context;
use crate::context::AppError;
use crate::queries::{ReceiptQueries, ReturnQueries, SetupQueries};
use crate::records::AggregateRef;
use crate::write::{
    Counter, NumberSlot, OutboxDraft, Replay, WriteContext, WriteSet, cash_session_aggregate,
    inventory_document_aggregate, return_aggregate, sale_aggregate,
};
use pos_domain::cash::{ClosedSession, OpenedSession};
use pos_domain::inventory::StockMovement;
use pos_domain::returns::ReturnSnapshot;
use pos_domain::sale::{SaleMode, SaleSnapshot};
use pos_domain::stock_docs::DocumentSnapshot;
use pos_domain::{EventId, IdGenerator, Timestamp, UserId};
use pos_sync::pull::ReplayedEvent;
use serde::Deserialize;
use std::sync::Arc;

/// Re-creates one replayed event locally (implemented by [`ReplayService`]; the sync worker
/// only needs this).
#[async_trait::async_trait]
pub trait Replayer: Send + Sync {
    async fn replay(&self, event: &ReplayedEvent) -> Result<(), AppError>;
}

pub struct ReplayService {
    pub store: Arc<dyn LocalStore>,
    pub setup: Arc<dyn SetupQueries>,
    pub receipts: Arc<dyn ReceiptQueries>,
    pub returns: Arc<dyn ReturnQueries>,
    pub ids: Arc<dyn IdGenerator>,
}

const NUMBER: NumberSlot = NumberSlot("replayed");

use payloads::{ClosedPayload, DocumentPayload, OpenedPayload, ReturnPayload, SalePayload};

/// The payloads as pushed (each event's snapshot with its number and stock movements).
mod payloads {
    // Serde buffers a `#[serde(flatten)]` field in a value type that names f32/f64; no amount
    // here is a float.
    #![allow(clippy::disallowed_types)]
    use super::*;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct SalePayload {
        #[serde(flatten)]
        pub(super) sale: SaleSnapshot,
        pub(super) stock_movements: Vec<StockMovement>,
        pub(super) receipt_seq: u64,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct ReturnPayload {
        #[serde(flatten)]
        pub(super) ret: ReturnSnapshot,
        pub(super) stock_movements: Vec<StockMovement>,
        pub(super) receipt_seq: u64,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct DocumentPayload {
        #[serde(flatten)]
        pub(super) doc: DocumentSnapshot,
        pub(super) stock_movements: Vec<StockMovement>,
        pub(super) document_seq: u64,
    }

    #[derive(Deserialize)]
    pub(super) struct OpenedPayload {
        pub(super) session: OpenedSession,
    }

    #[derive(Deserialize)]
    pub(super) struct ClosedPayload {
        pub(super) session: ClosedSession,
    }
}

fn parse<T: serde::de::DeserializeOwned>(e: &ReplayedEvent) -> Result<T, AppError> {
    serde_json::from_value(e.payload.clone())
        .map_err(|err| AppError::Internal(format!("replayed {} #{}: {err}", e.event_type, e.local_sequence)))
}

/// The catalogue's own `&'static str` for an event type (the outbox draft needs one).
fn event_type(name: &str) -> Result<&'static str, AppError> {
    pos_sync::CATALOGUE
        .iter()
        .map(|(t, _)| *t)
        .find(|t| *t == name)
        .ok_or_else(|| AppError::Internal(format!("cannot replay an unknown event type {name}")))
}

fn aggregate_type(name: &str) -> Result<&'static str, AppError> {
    const KNOWN: &[&str] = &[
        "business",
        "location",
        "register",
        "setting",
        "staff_user",
        "tax_rate",
        "tax_category",
        "category",
        "product",
        "sale",
        "return",
        "inventory_document",
        "cash_session",
        "audit",
    ];
    KNOWN
        .iter()
        .copied()
        .find(|t| *t == name)
        .ok_or_else(|| AppError::Internal(format!("cannot replay an unknown aggregate type {name}")))
}

fn status(s: &str) -> Result<&'static str, AppError> {
    match s {
        "applied" => Ok("applied"),
        "conflict" => Ok("conflict"),
        "rejected" => Ok("rejected"),
        other => Err(AppError::Internal(format!("unknown outcome {other}"))),
    }
}

impl ReplayService {
    async fn context(&self, actor: Option<UserId>, occurred_at: Timestamp) -> Result<WriteContext, AppError> {
        let reg = self
            .setup
            .register_context()
            .await?
            .ok_or_else(|| AppError::Internal("no register to replay into".into()))?;
        Ok(WriteContext {
            business_id: reg.business_id,
            location_id: reg.location_id,
            register_id: reg.register_id,
            actor,
            occurred_at,
        })
    }

    fn replay_of(e: &ReplayedEvent, numbers: Vec<(Counter, u64)>) -> Result<Replay, AppError> {
        Ok(Replay {
            event_id: EventId::from_uuid(e.event_id),
            local_sequence: e.local_sequence,
            event_type: e.event_type.clone(),
            payload: e.payload.clone(),
            status: status(&e.status)?,
            numbers,
        })
    }

    fn draft(&self, e: &ReplayedEvent, aggregate: AggregateRef) -> Result<OutboxDraft, AppError> {
        Ok(OutboxDraft::new(
            self.ids.as_ref(),
            event_type(&e.event_type)?,
            e.schema_version,
            aggregate,
            e.base_version,
            &e.payload,
        )?)
    }

    fn restored(
        &self,
        action: audit::AuditAction,
        entity_type: &'static str,
        id: impl ToString,
        e: &ReplayedEvent,
    ) -> AuditEntry {
        AuditEntry::new(self.ids.as_ref(), action, entity_type, id).with_metadata(serde_json::json!({
            "restoredFromCloud": true,
            "localSequence": e.local_sequence,
        }))
    }

    async fn sale(&self, e: &ReplayedEvent) -> Result<WriteSet, AppError> {
        let p: SalePayload = parse(e)?;
        let sale = p.sale;
        let ctx = self.context(Some(sale.cashier), sale.occurred_at).await?;
        let info = self.receipts.receipt_info(ctx.register_id, sale.cashier).await?;
        let document = pos_receipts::sale_receipt(
            &sale,
            &receipt_context(&info),
            pos_receipts::document::PENDING_NUMBER,
            pos_receipts::CopyKind::Original,
        );
        let document = serde_json::to_value(&document).map_err(|err| AppError::Internal(err.to_string()))?;
        let aggregate = sale_aggregate(sale.id);
        Ok(WriteSet::builder(ctx)
            .allocate_number(Counter::Receipt, NUMBER)
            .insert_sale(sale.clone(), NUMBER, false)
            .insert_receipt(
                pos_domain::ReceiptId::generate(self.ids.as_ref()),
                ("sale", sale.id.to_string()),
                pos_receipts::CONTENT_VERSION,
                document,
                Some(NUMBER),
            )
            .append_movements(p.stock_movements, aggregate.clone())
            .outbox(self.draft(e, aggregate)?)
            .audit(self.restored(audit::SALE_COMPLETED, "sale", sale.id, e))
            .replaying(Self::replay_of(e, vec![(Counter::Receipt, p.receipt_seq)])?)
            .build()?)
    }

    async fn return_(&self, e: &ReplayedEvent) -> Result<WriteSet, AppError> {
        let p: ReturnPayload = parse(e)?;
        let ret = p.ret;
        let ctx = self.context(Some(ret.actor), ret.occurred_at).await?;
        let info = self.receipts.receipt_info(ctx.register_id, ret.actor).await?;
        let sandbox = self
            .returns
            .returnable_sale(ret.original_sale)
            .await?
            .is_some_and(|s| s.mode == SaleMode::Sandbox);
        let receipt = pos_receipts::return_receipt(
            &ret,
            &receipt_context(&info),
            pos_receipts::document::PENDING_NUMBER,
            sandbox,
        );
        let receipt = serde_json::to_value(&receipt).map_err(|err| AppError::Internal(err.to_string()))?;
        let aggregate = return_aggregate(ret.id);
        Ok(WriteSet::builder(ctx)
            .allocate_number(Counter::Return, NUMBER)
            .insert_return(ret.clone(), NUMBER, false)
            .append_movements(p.stock_movements, aggregate.clone())
            .insert_receipt(
                pos_domain::ReceiptId::generate(self.ids.as_ref()),
                ("return", ret.id.to_string()),
                pos_receipts::CONTENT_VERSION,
                receipt,
                Some(NUMBER),
            )
            .outbox(self.draft(e, aggregate)?)
            .audit(self.restored(audit::RETURN_COMPLETED, "return", ret.id, e))
            .replaying(Self::replay_of(e, vec![(Counter::Return, p.receipt_seq)])?)
            .build()?)
    }

    async fn document(&self, e: &ReplayedEvent) -> Result<WriteSet, AppError> {
        let p: DocumentPayload = parse(e)?;
        let doc = p.doc;
        let ctx = self.context(Some(doc.actor), doc.occurred_at).await?;
        let action = match doc.plan.doc_type {
            pos_domain::stock_docs::DocumentType::Opening => audit::INVENTORY_OPENING_POSTED,
            pos_domain::stock_docs::DocumentType::Receipt => audit::INVENTORY_RECEIPT_POSTED,
            pos_domain::stock_docs::DocumentType::Adjustment => audit::INVENTORY_ADJUSTMENT_POSTED,
            pos_domain::stock_docs::DocumentType::WriteOff => audit::INVENTORY_WRITE_OFF_POSTED,
        };
        let aggregate = inventory_document_aggregate(doc.id);
        Ok(WriteSet::builder(ctx)
            .allocate_number(Counter::InventoryDocument, NUMBER)
            .insert_inventory_document(doc.clone(), NUMBER)
            .append_movements(p.stock_movements, aggregate.clone())
            .outbox(self.draft(e, aggregate)?)
            .audit(self.restored(action, "inventory_document", doc.id, e))
            .replaying(Self::replay_of(
                e,
                vec![(Counter::InventoryDocument, p.document_seq)],
            )?)
            .build()?)
    }

    async fn opened(&self, e: &ReplayedEvent) -> Result<WriteSet, AppError> {
        let p: OpenedPayload = parse(e)?;
        let s = p.session;
        let ctx = self.context(Some(s.opened_by), s.opened_at).await?;
        let aggregate = cash_session_aggregate(s.id);
        // The Z number is the next after the sessions there are: the same, replayed in order.
        Ok(WriteSet::builder(ctx)
            .open_cash_session(s.clone(), NUMBER)
            .outbox(self.draft(e, aggregate)?)
            .audit(self.restored(audit::CASH_SESSION_OPENED, "cash_session", s.id, e))
            .replaying(Self::replay_of(e, Vec::new())?)
            .build()?)
    }

    async fn closed(&self, e: &ReplayedEvent) -> Result<WriteSet, AppError> {
        let p: ClosedPayload = parse(e)?;
        let s = p.session;
        let ctx = self.context(Some(s.closed_by), s.closed_at).await?;
        let aggregate = cash_session_aggregate(s.id);
        Ok(WriteSet::builder(ctx)
            .close_cash_session(s.clone())
            .outbox(self.draft(e, aggregate)?)
            .audit(self.restored(audit::CASH_SESSION_CLOSED, "cash_session", s.id, e))
            .replaying(Self::replay_of(e, Vec::new())?)
            .build()?)
    }

    /// Master data and standalone audit entries: only the event goes back in the outbox (the
    /// state itself comes with the bootstrap after the restore).
    async fn event_only(&self, e: &ReplayedEvent) -> Result<WriteSet, AppError> {
        let occurred_at = Timestamp::parse_rfc3339(&e.occurred_at)
            .ok_or_else(|| AppError::Internal(format!("bad time {}", e.occurred_at)))?;
        let ctx = self.context(None, occurred_at).await?;
        let aggregate = AggregateRef {
            aggregate_type: aggregate_type(&e.aggregate_type)?,
            aggregate_id: e.aggregate_id.clone(),
        };
        Ok(WriteSet::builder(ctx)
            .outbox(self.draft(e, aggregate.clone())?)
            .audit(
                AuditEntry::new(
                    self.ids.as_ref(),
                    audit::BACKUP_EVENT_RESTORED,
                    aggregate.aggregate_type,
                    &aggregate.aggregate_id,
                )
                .with_metadata(serde_json::json!({
                    "eventType": e.event_type,
                    "localSequence": e.local_sequence,
                })),
            )
            .replaying(Self::replay_of(e, Vec::new())?)
            .build()?)
    }
}

#[async_trait::async_trait]
impl Replayer for ReplayService {
    async fn replay(&self, e: &ReplayedEvent) -> Result<(), AppError> {
        let set = match e.event_type.as_str() {
            "sale.completed" => self.sale(e).await?,
            "return.completed" => self.return_(e).await?,
            "inventory.document_posted" => self.document(e).await?,
            "cash_session.opened" => self.opened(e).await?,
            "cash_session.closed" => self.closed(e).await?,
            _ => self.event_only(e).await?,
        };
        self.store.apply(set).await?;
        Ok(())
    }
}
