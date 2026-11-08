//! Reading receipts back (E13-T03): the stored document, rendered for a paper width; and
//! saving it as a PDF (E13-T04).

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::ReceiptQueries;
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::{ErrorCode, Permission, ReceiptId, ReturnId, SaleId};
use pos_receipts::ReceiptDocument;
use pos_receipts::pdf::{PdfFormat, render_pdf};
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

/// Whose receipt to save as a PDF.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReceiptOf {
    Sale(SaleId),
    Return(ReturnId),
}

/// The paper of a PDF: the receipt as printed (the till's paper width), or an A4 page.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PdfPaper {
    Receipt,
    A4,
}

/// A rendered PDF, not yet saved.
#[derive(Debug)]
pub struct ReceiptPdf {
    pub receipt_id: ReceiptId,
    pub receipt_number: String,
    pub format: PdfFormat,
    /// A suggested file name, e.g. `receipt-R01-000123.pdf`.
    pub file_name: String,
    pub bytes: Vec<u8>,
}

/// Receipts as PDF files (E13-T04, RCP-003, AT-08): works with no printer at all. The PDF is
/// rendered from the stored document, so it says what the receipt said when it was made.
pub struct ReceiptPdfService {
    pub receipts: Arc<dyn ReceiptQueries>,
    pub store: Arc<dyn LocalStore>,
}

impl ReceiptPdfService {
    /// Renders the PDF. Nothing is written or recorded until [`Self::record`].
    pub async fn render(
        &self,
        ctx: &CommandContext,
        of: ReceiptOf,
        paper: PdfPaper,
    ) -> Result<ReceiptPdf, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let actor = ctx.actor()?;
        let stored = match of {
            ReceiptOf::Sale(id) => self.receipts.sale_receipt(id).await?,
            ReceiptOf::Return(id) => self.receipts.return_receipt(id).await?,
        };
        let (receipt_id, json) = stored.ok_or_else(|| {
            AppError::Validation("there is no receipt to save".into(), ErrorCode::ValidationFailed)
        })?;
        let document: ReceiptDocument =
            serde_json::from_str(&json).map_err(|e| AppError::Internal(format!("stored receipt: {e}")))?;
        let format = match paper {
            PdfPaper::A4 => PdfFormat::A4,
            PdfPaper::Receipt => {
                let width = self
                    .receipts
                    .receipt_info(ctx.register.register_id, actor.user_id)
                    .await?
                    .paper_width_mm;
                if width == 58 {
                    PdfFormat::Receipt58
                } else {
                    PdfFormat::Receipt80
                }
            }
        };
        let bytes = render_pdf(&document, format).map_err(|e| AppError::Internal(e.to_string()))?;
        let number: String = document
            .meta
            .receipt_number
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let kind = match of {
            ReceiptOf::Sale(_) => "receipt",
            ReceiptOf::Return(_) => "refund",
        };
        let suffix = if format == PdfFormat::A4 { "-A4" } else { "" };
        Ok(ReceiptPdf {
            receipt_id,
            receipt_number: document.meta.receipt_number,
            format,
            file_name: format!("{kind}-{number}{suffix}.pdf"),
            bytes,
        })
    }

    /// Records that the PDF was saved (`receipt.exported_pdf`), with the file's name (not its
    /// folder: paths can name people).
    pub async fn record(
        &self,
        ctx: &CommandContext,
        pdf: &ReceiptPdf,
        saved_as: &str,
    ) -> Result<(), AppError> {
        let ids = ctx.ids.as_ref();
        let format = match pdf.format {
            PdfFormat::Receipt58 => "receipt_58mm",
            PdfFormat::Receipt80 => "receipt_80mm",
            PdfFormat::A4 => "a4",
        };
        let entry = AuditEntry::new(ids, audit::RECEIPT_EXPORTED_PDF, "receipt", pdf.receipt_id).with_metadata(
            serde_json::json!({ "receiptNumber": pdf.receipt_number, "format": format, "file": saved_as }),
        );
        let set = WriteSet::builder(ctx.write_context())
            .outbox(OutboxDraft::audit_recorded(ids, &entry)?)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }
}
