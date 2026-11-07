//! Returns and refunds (E12-T01/T02, RET-003..007, INV-009, AT-10).
//!
//! One command, one transaction: the return and its lines, the refunds (separate records,
//! RET-005), stock movements by disposition, the refund receipt, the audit row and the
//! `return.completed` event. RET-004 (no more returned than sold) is checked by the domain
//! for the preview and again by the store inside the transaction, so two returns of the
//! last unit cannot both succeed. The original sale is never modified (RET-002).

use crate::audit::{self, AuditEntry};
use crate::checkout::receipt_context;
use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, ReceiptQueries, ReturnQueries, SetupQueries};
use crate::records::RegisterMode;
use crate::write::{Counter, NumberSlot, return_aggregate};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::returns::{
    RefundSnapshot, ReturnLineRequest, ReturnLineSnapshot, ReturnPlan, ReturnReason, ReturnSnapshot,
    ReturnableSale, plan_return, settle_refunds,
};
use pos_domain::sale::SaleMode;
use pos_domain::settlement::TenderInput;
use pos_domain::{ErrorCode, Permission, RefundId, ReturnId, ReturnLineId, SaleId, role_has};
use serde::Serialize;
use std::sync::Arc;

const RETURN_NUMBER: NumberSlot = NumberSlot("return");

pub struct ReturnService {
    pub store: Arc<dyn LocalStore>,
    pub returns: Arc<dyn ReturnQueries>,
    pub setup: Arc<dyn SetupQueries>,
    pub receipts: Arc<dyn ReceiptQueries>,
    /// For the business time zone.
    pub pricing: Arc<dyn CartQueries>,
}

#[derive(Clone, Debug)]
pub struct CreateReturn {
    pub sale_id: SaleId,
    pub lines: Vec<ReturnLineRequest>,
    pub reason: ReturnReason,
    pub note: Option<String>,
    pub refunds: Vec<TenderInput>,
}

#[derive(Clone, Debug)]
pub struct CompletedReturn {
    pub ret: ReturnSnapshot,
    pub return_number: String,
}

/// The `return.completed` v1 payload. `receiptNumber`/`receiptSeq` (filled in the
/// transaction) carry the return's own number.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReturnCompletedV1<'a> {
    #[serde(flatten)]
    ret: &'a ReturnSnapshot,
    location_id: pos_domain::LocationId,
    register_id: pos_domain::RegisterId,
    stock_movements: &'a [pos_domain::inventory::StockMovement],
}

fn not_found() -> AppError {
    AppError::Validation(
        "no sale with that receipt number on this device".into(),
        ErrorCode::ValidationFailed,
    )
}

impl ReturnService {
    async fn mode(&self) -> Result<SaleMode, AppError> {
        Ok(match self.setup.device_identity().await?.map(|d| d.mode) {
            Some(RegisterMode::Live) => SaleMode::Live,
            _ => SaleMode::Sandbox,
        })
    }

    /// Finds a sale to return by its receipt number (typed, or scanned from the receipt).
    pub async fn find(&self, ctx: &CommandContext, receipt_number: &str) -> Result<ReturnableSale, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let id = self
            .returns
            .sale_by_receipt(receipt_number)
            .await?
            .ok_or_else(not_found)?;
        self.returns.returnable_sale(id).await?.ok_or_else(not_found)
    }

    /// What a return would refund (no writes).
    pub async fn preview(
        &self,
        ctx: &CommandContext,
        sale: SaleId,
        lines: &[ReturnLineRequest],
    ) -> Result<ReturnPlan, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let sale = self.returns.returnable_sale(sale).await?.ok_or_else(not_found)?;
        Ok(plan_return(&sale, self.mode().await?, lines)?)
    }

    pub async fn create(
        &self,
        ctx: &CommandContext,
        input: CreateReturn,
    ) -> Result<CompletedReturn, AppError> {
        let actor = ctx.actor()?.clone();
        // A cashier needs a manager's approval unless the shop has turned that off.
        let approver = if role_has(actor.role, Permission::ReturnCreate)
            || !self.returns.cashier_returns_need_approval().await?
        {
            ctx.authorize(Permission::SalesView)?;
            None
        } else {
            ctx.authorize_with_approval(Permission::ReturnCreate, input.sale_id, None)?
        };
        if input.refunds.iter().any(|r| !r.kind.is_cash()) {
            ctx.authorize(Permission::ExternalTenderRecord)?;
        }
        let sale = self
            .returns
            .returnable_sale(input.sale_id)
            .await?
            .ok_or_else(not_found)?;
        let mode = self.mode().await?;
        let plan = plan_return(&sale, mode, &input.lines)?;
        let refunds = settle_refunds(plan.total, &input.refunds)?;
        // Cash paid out in live mode comes from the open drawer session.
        let require_session = mode == SaleMode::Live
            && refunds.iter().any(|r| r.kind.is_cash())
            && self.setup.cash_session_required().await?;
        let ids = ctx.ids.as_ref();
        let timezone = self.pricing.pricing_settings().await?.timezone;
        let business_date = pos_domain::time::business_date(
            ctx.now(),
            pos_domain::time::parse_time_zone(&timezone).map_err(|e| AppError::Internal(e.to_string()))?,
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;
        let ret = ReturnSnapshot {
            id: ReturnId::generate(ids),
            original_sale: sale.sale_id,
            original_receipt_number: sale.receipt_number.clone(),
            reason: input.reason,
            note: input.note.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()),
            currency: sale.currency,
            subtotal: plan.subtotal,
            tax: plan.tax,
            total: plan.total,
            lines: plan
                .lines
                .into_iter()
                .map(|line| ReturnLineSnapshot {
                    id: ReturnLineId::generate(ids),
                    line,
                })
                .collect(),
            refunds: refunds
                .into_iter()
                .enumerate()
                .map(|(i, tender)| RefundSnapshot {
                    id: RefundId::generate(ids),
                    seq: u32::try_from(i + 1).unwrap_or(u32::MAX),
                    tender,
                })
                .collect(),
            actor: actor.user_id,
            approver,
            occurred_at: ctx.now(),
            business_date,
            timezone,
        };
        let movements = ret.movements(ctx.register.location_id, ids);
        let info = self
            .receipts
            .receipt_info(ctx.register.register_id, actor.user_id)
            .await?;
        let receipt = pos_receipts::return_receipt(
            &ret,
            &receipt_context(&info),
            pos_receipts::document::PENDING_NUMBER,
            sale.mode == SaleMode::Sandbox,
        );
        let receipt = serde_json::to_value(&receipt).map_err(|e| AppError::Internal(e.to_string()))?;
        let aggregate = return_aggregate(ret.id);
        let payload = ReturnCompletedV1 {
            ret: &ret,
            location_id: ctx.register.location_id,
            register_id: ctx.register.register_id,
            stock_movements: &movements,
        };
        let event = OutboxDraft::new(ids, "return.completed", 1, aggregate.clone(), None, &payload)?
            .with_receipt_number(RETURN_NUMBER);
        let mut entry = AuditEntry::new(ids, audit::RETURN_COMPLETED, "return", ret.id).with_metadata(
            serde_json::json!({
                "originalSale": ret.original_sale,
                "totalMinor": ret.total.minor().to_string(),
                "reason": ret.reason.as_str(),
            }),
        );
        if let Some(a) = approver {
            entry = entry.with_approver(a);
        }
        let set = WriteSet::builder(ctx.write_context())
            .allocate_number(Counter::Return, RETURN_NUMBER)
            .insert_return(ret.clone(), RETURN_NUMBER, require_session)
            .append_movements(movements, aggregate)
            .insert_receipt(
                pos_domain::ReceiptId::generate(ids),
                ("return", ret.id.to_string()),
                pos_receipts::CONTENT_VERSION,
                receipt,
                Some(RETURN_NUMBER),
            )
            .outbox(event)
            .audit(entry)
            .build()?;
        let applied = self.store.apply(set).await?;
        let return_number = applied
            .receipt_numbers
            .get(&RETURN_NUMBER)
            .cloned()
            .ok_or_else(|| AppError::Internal("the store did not report the return number".into()))?;
        Ok(CompletedReturn { ret, return_number })
    }
}
