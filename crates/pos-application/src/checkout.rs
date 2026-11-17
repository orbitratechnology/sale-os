//! Completing a sale (E09-T08, POS-008..010, AT-04/05). The flow of
//! docs/backlog/02-architecture-and-modules.md §4: one command, one transaction.
//!
//! Everything is decided before writing: price the cart, settle the tenders, build the
//! immutable snapshot, work out the stock movements and apply the negative-stock policy.
//! Then one write set records the receipt number, sale, lines, tenders, movements and
//! balances, closes the cart, and adds the audit row and the `sale.completed` event. It all
//! commits or none of it does; the UI shows "completed" only after the commit.
//!
//! The receipt number is allocated inside the transaction (gapless per register). The
//! event payload is built before that, so the store fills `receiptSeq`/`receiptNumber` into
//! it from the same slot ([`crate::OutboxDraft::with_receipt_number`]).

use crate::audit::{self, AuditEntry};
use crate::cart::{CartService, Loaded};
use crate::context::{AppError, CommandContext};
use crate::queries::{ReceiptQueries, SetupQueries, StockQueries};
use crate::records::RegisterMode;
use crate::write::{Counter, NumberSlot, sale_aggregate};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::inventory::{NegativeStockFlag, StockMovement, check_negative};
use pos_domain::pricing::PricingPolicy;
use pos_domain::sale::{SaleHeader, SaleMode, SaleSnapshot};
use pos_domain::settlement::{TenderInput, settle};
use pos_domain::{CartId, ErrorCode, Permission};
use serde::Serialize;
use std::sync::Arc;

const RECEIPT: NumberSlot = NumberSlot("receipt");

pub struct CheckoutService {
    pub carts: Arc<CartService>,
    pub store: Arc<dyn LocalStore>,
    pub setup: Arc<dyn SetupQueries>,
    pub stock: Arc<dyn StockQueries>,
    pub receipts: Arc<dyn ReceiptQueries>,
}

/// `checkout_complete`'s input: the cart and revision the cashier saw, and the tenders.
#[derive(Clone, Debug)]
pub struct CompleteSale {
    pub cart_id: CartId,
    pub revision: u64,
    pub tenders: Vec<TenderInput>,
}

#[derive(Clone, Debug)]
pub struct CompletedSale {
    pub sale: SaleSnapshot,
    pub receipt_number: String,
    /// The `sale.completed` event's position in the outbox ("sync pending").
    pub outbox_sequence: u64,
    /// Variants this sale took below zero (allowed and flagged by the policy).
    pub negative_stock: Vec<NegativeStockFlag>,
    /// `receipt.auto_print`: the UI prints right away (after the commit, RCP-006).
    pub auto_print: bool,
}

/// The `sale.completed` v1 payload (07-contracts §4): self-contained.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SaleCompletedV1<'a> {
    #[serde(flatten)]
    sale: &'a SaleSnapshot,
    location_id: pos_domain::LocationId,
    register_id: pos_domain::RegisterId,
    stock_movements: &'a [StockMovement],
}

impl CheckoutService {
    pub async fn complete_sale(
        &self,
        ctx: &CommandContext,
        input: CompleteSale,
    ) -> Result<CompletedSale, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        if input.tenders.iter().any(|t| !t.kind.is_cash()) {
            ctx.authorize(Permission::ExternalTenderRecord)?;
            // The shop may keep card and other external payments to managers and owners.
            if ctx.actor()?.role == pos_domain::Role::Cashier && !self.setup.cashier_external_tender().await?
            {
                return Err(AppError::PermissionDenied(Permission::ExternalTenderRecord));
            }
        }
        let actor = ctx.actor()?.clone();
        // Hold the cart lock: no scan or edit can slip in between pricing and commit.
        let _guard = self.carts.exclusive().await;
        let Loaded { cart, base } = self.carts.load(ctx).await?;
        if base.is_none() || cart.id != input.cart_id || cart.revision != input.revision {
            return Err(AppError::Validation(
                "the cart changed since it was shown; check it and pay again".into(),
                ErrorCode::CartRevisionMismatch,
            ));
        }
        if cart.lines.is_empty() {
            return Err(AppError::Validation(
                "the cart is empty".into(),
                ErrorCode::CartEmpty,
            ));
        }

        let view = self.carts.view(ctx, cart.clone(), Vec::new()).await?;
        let policy = PricingPolicy::new(view.settings.tax_mode);
        let settlement = settle(view.priced.total, &input.tenders, &policy)?;
        let mode = match self.setup.device_identity().await?.map(|d| d.mode) {
            Some(RegisterMode::Live) => SaleMode::Live,
            _ => SaleMode::Sandbox,
        };
        let header = SaleHeader {
            mode,
            currency: view.settings.currency,
            prices_include_tax: view.settings.tax_mode == pos_domain::pricing::TaxMode::Inclusive,
            cashier: actor.user_id,
            occurred_at: ctx.now(),
            business_date: self.carts.today(ctx, &view.settings)?,
            timezone: view.settings.timezone.clone(),
        };
        let ids = ctx.ids.as_ref();
        let sale = SaleSnapshot::build(&cart, &view.priced, settlement, header, ids)?;

        let location = ctx.register.location_id;
        let movements = sale.movements(location, ids);
        let variants: Vec<_> = movements.iter().map(|m| m.variant_id).collect();
        let on_hand = self.stock.on_hand(location, &variants).await?;
        let negative = check_negative(self.stock.negative_stock_policy().await?, &on_hand, &movements)?;

        // The receipt is part of the sale's transaction (02 §4 step 8); its number is filled
        // in by the store from the same slot as the sale's.
        let info = self
            .receipts
            .receipt_info(ctx.register.register_id, actor.user_id)
            .await?;
        let document = pos_receipts::sale_receipt(
            &sale,
            &receipt_context(&info),
            pos_receipts::document::PENDING_NUMBER,
            pos_receipts::CopyKind::Original,
        );
        let document = serde_json::to_value(&document).map_err(|e| AppError::Internal(e.to_string()))?;

        let aggregate = sale_aggregate(sale.id);
        let payload = SaleCompletedV1 {
            sale: &sale,
            location_id: location,
            register_id: ctx.register.register_id,
            stock_movements: &movements,
        };
        // DER-001: a live sale needs an open drawer session (checked again in the transaction).
        let require_session = mode == SaleMode::Live && self.setup.cash_session_required().await?;
        let event = OutboxDraft::new(ids, "sale.completed", 1, aggregate.clone(), None, &payload)?
            .with_receipt_number(RECEIPT);
        let mut completed =
            AuditEntry::new(ids, audit::SALE_COMPLETED, "sale", sale.id).with_metadata(serde_json::json!({
                "totalMinor": sale.total.minor().to_string(),
                "lines": sale.lines.len(),
                "tenders": sale.tenders.iter().map(|t| t.tender.kind.as_str()).collect::<Vec<_>>(),
            }));
        if let Some(a) = sale.approver {
            completed = completed.with_approver(a);
        }
        let mut b = WriteSet::builder(ctx.write_context())
            .allocate_number(Counter::Receipt, RECEIPT)
            .insert_sale(sale.clone(), RECEIPT, require_session)
            .insert_receipt(
                pos_domain::ReceiptId::generate(ids),
                ("sale", sale.id.to_string()),
                pos_receipts::CONTENT_VERSION,
                document,
                Some(RECEIPT),
            )
            .append_movements(movements.clone(), aggregate)
            .close_cart(cart.id, sale.id, cart.revision)
            .outbox(event)
            .audit(completed);
        for flag in &negative {
            b = b.audit(
                AuditEntry::new(ids, audit::INVENTORY_NEGATIVE_FLAGGED, "product_variant", flag.variant_id)
                    .with_metadata(serde_json::json!({ "saleId": sale.id, "resultingMilli": flag.resulting.milli().to_string() })),
            );
        }
        // PAY-003: an external payment is recorded by staff, never confirmed by the till.
        for t in sale.tenders.iter().filter(|t| !t.tender.kind.is_cash()) {
            b = b.audit(
                AuditEntry::new(ids, audit::TENDER_RECORDED_EXTERNAL, "sale", sale.id).with_metadata(
                    serde_json::json!({
                        "tender": t.tender.kind.as_str(),
                        "amountMinor": t.tender.amount.minor().to_string(),
                        "hasReference": t.tender.external_reference.is_some(),
                    }),
                ),
            );
        }
        let applied = self.store.apply(b.build()?).await?;
        let receipt_number = applied
            .receipt_numbers
            .get(&RECEIPT)
            .cloned()
            .ok_or_else(|| AppError::Internal("the store did not report the receipt number".into()))?;
        let outbox_sequence = applied.outbox.first().map(|(_, seq)| *seq).unwrap_or_default();
        Ok(CompletedSale {
            sale,
            receipt_number,
            outbox_sequence,
            negative_stock: negative,
            auto_print: info.auto_print,
        })
    }
}

/// The receipt's view of the shop, till and cashier.
pub(crate) fn receipt_context(info: &crate::queries::ReceiptInfo) -> pos_receipts::ReceiptContext {
    pos_receipts::ReceiptContext {
        business_name: info.business_name.clone(),
        address_lines: info.address_lines.clone(),
        phone: info.phone.clone(),
        tax_registration_no: info.tax_registration_no.clone(),
        header_lines: info.header_lines.clone(),
        footer_lines: info.footer_lines.clone(),
        register_name: info.register_name.clone(),
        register_code: info.register_code.clone(),
        cashier_name: info.cashier_name.clone(),
    }
}
