//! Reading receipts back (E13-T03): the stored document, rendered for a paper width.

use crate::context::{AppError, CommandContext};
use crate::queries::ReceiptQueries;
use pos_domain::{ErrorCode, Permission, ReturnId, SaleId};
use pos_receipts::ReceiptDocument;
use pos_receipts::text::{COLUMNS_58MM, COLUMNS_80MM, TextLine, render};
use std::sync::Arc;

pub struct ReceiptService {
    pub receipts: Arc<dyn ReceiptQueries>,
}

/// A receipt ready to show: the document and its lines for the chosen width.
#[derive(Debug)]
pub struct RenderedReceipt {
    pub document: ReceiptDocument,
    pub columns: usize,
    pub lines: Vec<TextLine>,
}

impl ReceiptService {
    /// A sale's receipt as stored at completion (never rebuilt from the catalogue).
    /// `width_mm`: 58 or 80; `None` uses the `receipt.paper_width_mm` setting.
    pub async fn for_sale(
        &self,
        ctx: &CommandContext,
        sale: Option<SaleId>,
        width_mm: Option<u32>,
    ) -> Result<RenderedReceipt, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let sale = match sale {
            Some(s) => s,
            None => self
                .receipts
                .last_sale(ctx.register.register_id)
                .await?
                .ok_or_else(|| {
                    AppError::Validation("no sale on this till yet".into(), ErrorCode::ValidationFailed)
                })?,
        };
        let (_, json) = self.receipts.sale_receipt(sale).await?.ok_or_else(|| {
            AppError::Validation("this sale has no receipt".into(), ErrorCode::ValidationFailed)
        })?;
        self.rendered(ctx, &json, width_mm).await
    }

    /// A return's refund receipt as stored when the return was made.
    pub async fn for_return(
        &self,
        ctx: &CommandContext,
        ret: ReturnId,
        width_mm: Option<u32>,
    ) -> Result<RenderedReceipt, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let (_, json) = self.receipts.return_receipt(ret).await?.ok_or_else(|| {
            AppError::Validation("this return has no receipt".into(), ErrorCode::ValidationFailed)
        })?;
        self.rendered(ctx, &json, width_mm).await
    }

    pub(crate) async fn rendered(
        &self,
        ctx: &CommandContext,
        json: &str,
        width_mm: Option<u32>,
    ) -> Result<RenderedReceipt, AppError> {
        let actor = ctx.actor()?;
        let document: ReceiptDocument =
            serde_json::from_str(json).map_err(|e| AppError::Internal(format!("stored receipt: {e}")))?;
        let width = match width_mm {
            Some(w) => w,
            None => {
                self.receipts
                    .receipt_info(ctx.register.register_id, actor.user_id)
                    .await?
                    .paper_width_mm
            }
        };
        let columns = if width == 58 { COLUMNS_58MM } else { COLUMNS_80MM };
        Ok(RenderedReceipt {
            lines: render(&document, columns),
            document,
            columns,
        })
    }
}
