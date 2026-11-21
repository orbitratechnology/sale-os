//! The print queue (E13-T06, RCP-006/007, AT-09).
//!
//! A job goes `queued → submitted → completed | failed | unknown`, and every change is in
//! `print_job_events`. Rules:
//!
//! - Printing happens after the sale is committed, as its own command: a printer problem can
//!   never undo or block a sale.
//! - A job that was **submitted** and got no answer is `unknown`, never retried
//!   automatically: the receipt may already be in the customer's hand. The operator says
//!   "Printed" or "Not printed", and "Not printed" prints a new **reprint** job.
//! - A **failed** job (the printer refused it: paper out, cover open) printed nothing, so
//!   retrying it is safe and it stays the original copy.
//! - Any copy after the first is a reprint: it carries the REPRINT marker and is audited.

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::{PrintQueries, ReceiptQueries};
use crate::records::{PrintJobRecord, PrintStatus};
use crate::{LocalStore, WriteSet};
use pos_device_ports::{CommandDialect, CutSupport, PrinterCapabilities};
use pos_device_ports::{PrintResult, ReceiptOutputPort};
use pos_domain::{ErrorCode, Permission, PrintJobId, ReceiptId, ReturnId, SaleId};
use pos_receipts::text::{COLUMNS_58MM, COLUMNS_80MM, TextLine, render};
use pos_receipts::{CodePage, CutMode, Dialect, EncodeOptions, ReceiptDocument};
use std::sync::Arc;
use std::time::Duration;

/// The encoder options for a printer (ADR-024): its command language, code pages (only the
/// ones the encoder has tables for), cutter, and native barcode/QR support.
pub fn encode_options(caps: &PrinterCapabilities, columns: usize) -> EncodeOptions {
    EncodeOptions {
        dialect: match caps.dialect {
            CommandDialect::EscPos => Dialect::EscPos,
            CommandDialect::StarLine => Dialect::StarLine,
            CommandDialect::PlainText => Dialect::PlainText,
        },
        code_pages: caps
            .code_pages
            .iter()
            .filter_map(|(name, n)| CodePage::from_name(name).map(|p| (p, *n)))
            .collect(),
        cut: match caps.cut {
            CutSupport::None => CutMode::None,
            CutSupport::Partial => CutMode::Partial,
            CutSupport::Full => CutMode::Full,
        },
        feed_before_cut: caps.feed_before_cut,
        barcode: caps.barcode,
        qr: caps.qr,
        drawer_kick: None,
        columns: Some(columns),
        // Every ESC/POS printer prints `GS v 0` images: Sinhala and Tamil names print as
        // shaped images instead of transliterated Latin (crate::raster).
        raster: caps.dialect == CommandDialect::EscPos,
    }
}

/// Characters per line: the printer's own when it says, else the paper-width setting.
pub fn columns_for(caps: &PrinterCapabilities, paper_width_mm: u16) -> usize {
    match caps.columns {
        Some(c) => usize::from(c).clamp(24, 64),
        None if paper_width_mm == 58 => COLUMNS_58MM,
        None => COLUMNS_80MM,
    }
}

/// Encodes rendered lines for a printer.
pub fn encode_for(caps: &PrinterCapabilities, lines: &[TextLine], columns: usize) -> Vec<u8> {
    pos_receipts::encode(lines, &encode_options(caps, columns))
}

/// How long a printer may take before the outcome is treated as unknown.
pub const DEFAULT_PRINT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PrintService {
    pub store: Arc<dyn LocalStore>,
    pub prints: Arc<dyn PrintQueries>,
    pub receipts: Arc<dyn ReceiptQueries>,
    pub printer: Arc<dyn ReceiptOutputPort>,
    pub timeout: Duration,
}

fn invalid(msg: &str) -> AppError {
    AppError::Validation(msg.into(), ErrorCode::ValidationFailed)
}

impl PrintService {
    /// Prints a sale's receipt (the last sale's when `sale` is `None`). The first copy is the
    /// original; any later copy is a reprint (RCP-007).
    pub async fn print_sale(
        &self,
        ctx: &CommandContext,
        sale: Option<SaleId>,
    ) -> Result<PrintJobRecord, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let sale = match sale {
            Some(s) => s,
            None => self
                .receipts
                .last_sale(ctx.register.register_id)
                .await?
                .ok_or_else(|| invalid("no sale on this till yet"))?,
        };
        let (receipt, _) = self
            .receipts
            .sale_receipt(sale)
            .await?
            .ok_or_else(|| invalid("this sale has no receipt"))?;
        self.print_receipt(ctx, receipt).await
    }

    /// Prints a return's refund receipt, with the same original/reprint rules.
    pub async fn print_return(
        &self,
        ctx: &CommandContext,
        ret: ReturnId,
    ) -> Result<PrintJobRecord, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let (receipt, _) = self
            .receipts
            .return_receipt(ret)
            .await?
            .ok_or_else(|| invalid("this return has no receipt"))?;
        self.print_receipt(ctx, receipt).await
    }

    /// Prints a sale's tax invoice (D-02), with the same original/copy rules.
    pub async fn print_tax_invoice(
        &self,
        ctx: &CommandContext,
        sale: SaleId,
    ) -> Result<PrintJobRecord, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let (receipt, _) = self
            .receipts
            .tax_invoice_receipt(sale)
            .await?
            .ok_or_else(|| invalid("this sale has no tax invoice"))?;
        self.print_receipt(ctx, receipt).await
    }

    /// Prints a stored receipt (the caller has checked who may print it).
    pub async fn print_receipt(
        &self,
        ctx: &CommandContext,
        receipt: ReceiptId,
    ) -> Result<PrintJobRecord, AppError> {
        let printed_before = self.prints.receipt_jobs(receipt).await?.iter().any(|j| {
            matches!(
                j.status,
                PrintStatus::Submitted | PrintStatus::Completed | PrintStatus::Unknown
            )
        });
        let job = self
            .enqueue(ctx, receipt, printed_before, serde_json::json!({}))
            .await?;
        self.run(ctx, job).await
    }

    /// Tries a **failed** job again: nothing was printed, so it stays the original copy.
    pub async fn retry(&self, ctx: &CommandContext, job: PrintJobId) -> Result<PrintJobRecord, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let job = self
            .prints
            .print_job(job)
            .await?
            .ok_or_else(|| invalid("print job not found"))?;
        if job.status != PrintStatus::Failed {
            return Err(invalid(
                "only a failed job can be tried again; an unknown one must be resolved first",
            ));
        }
        self.run(ctx, job).await
    }

    /// The operator settles an unknown job. Printed: done. Not printed: a new reprint job.
    pub async fn resolve_unknown(
        &self,
        ctx: &CommandContext,
        job: PrintJobId,
        printed: bool,
    ) -> Result<PrintJobRecord, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let actor = ctx.actor()?.clone();
        let mut job = self
            .prints
            .print_job(job)
            .await?
            .ok_or_else(|| invalid("print job not found"))?;
        if job.status != PrintStatus::Unknown {
            return Err(invalid("this print job is not waiting for an answer"));
        }
        job.status = if printed {
            PrintStatus::Completed
        } else {
            PrintStatus::Failed
        };
        job.updated_at = ctx.now();
        let detail = serde_json::json!({ "resolvedBy": actor.user_id, "printed": printed });
        let set = WriteSet::builder(ctx.write_context())
            .put_print_job(job.clone(), detail.clone())
            .audit(
                AuditEntry::new(
                    ctx.ids.as_ref(),
                    audit::PRINT_UNKNOWN_RESOLVED,
                    "print_job",
                    job.job_id,
                )
                .with_metadata(detail),
            )
            .build()?;
        self.store.apply(set).await?;
        if printed {
            return Ok(job);
        }
        let reprint = self
            .enqueue(
                ctx,
                job.receipt_id,
                true,
                serde_json::json!({ "replaces": job.job_id }),
            )
            .await?;
        self.run(ctx, reprint).await
    }

    async fn enqueue(
        &self,
        ctx: &CommandContext,
        receipt: ReceiptId,
        reprint: bool,
        detail: serde_json::Value,
    ) -> Result<PrintJobRecord, AppError> {
        let actor = ctx.actor()?.clone();
        if reprint {
            ctx.authorize(Permission::ReceiptReprint)?;
        }
        let now = ctx.now();
        let job = PrintJobRecord {
            job_id: PrintJobId::generate(ctx.ids.as_ref()),
            receipt_id: receipt,
            adapter: self.printer.adapter(),
            target: self.printer.target(),
            reprint,
            status: PrintStatus::Queued,
            attempt_count: 0,
            last_error_code: None,
            requested_by: actor.user_id,
            created_at: now,
            updated_at: now,
        };
        let mut b = WriteSet::builder(ctx.write_context()).put_print_job(job.clone(), detail);
        if reprint {
            b = b.audit(AuditEntry::new(
                ctx.ids.as_ref(),
                audit::RECEIPT_REPRINTED,
                "receipt",
                receipt,
            ));
        }
        self.store.apply(b.build()?).await?;
        Ok(job)
    }

    /// Submits once and records the outcome. Never resubmits.
    async fn run(&self, ctx: &CommandContext, mut job: PrintJobRecord) -> Result<PrintJobRecord, AppError> {
        let actor = ctx.actor()?.clone();
        let json = self
            .prints
            .receipt_document(job.receipt_id)
            .await?
            .ok_or_else(|| invalid("receipt not found"))?;
        let mut doc: ReceiptDocument =
            serde_json::from_str(&json).map_err(|e| AppError::Internal(format!("stored receipt: {e}")))?;
        if job.reprint {
            doc = doc.as_reprint();
        }
        let info = self
            .receipts
            .receipt_info(ctx.register.register_id, actor.user_id)
            .await?;
        let caps = self.printer.capabilities();
        let columns = columns_for(&caps, u16::try_from(info.paper_width_mm).unwrap_or(80));
        let bytes = encode_for(&caps, &render(&doc, columns), columns);
        let name = format!(
            "{}{}",
            doc.meta.receipt_number,
            if job.reprint { "-reprint" } else { "" }
        );

        job.status = PrintStatus::Submitted;
        job.attempt_count += 1;
        job.updated_at = ctx.now();
        self.save(ctx, &job, serde_json::json!({ "attempt": job.attempt_count }))
            .await?;

        let outcome = tokio::time::timeout(self.timeout, self.printer.submit(&name, &bytes)).await;
        let detail = match outcome {
            Ok(PrintResult::Printed) => {
                job.status = PrintStatus::Completed;
                job.last_error_code = None;
                serde_json::json!({})
            }
            Ok(PrintResult::Failed(f)) => {
                job.status = PrintStatus::Failed;
                job.last_error_code = Some(f.code().into());
                serde_json::json!({ "error": f.code() })
            }
            // Part of the job may be on paper: never resubmit automatically (RCP-007).
            Ok(PrintResult::Uncertain(reason)) => {
                job.status = PrintStatus::Unknown;
                serde_json::json!({ "uncertain": reason })
            }
            // No answer: the paper may be out of the printer. Never resubmit automatically.
            Err(_) => {
                job.status = PrintStatus::Unknown;
                serde_json::json!({ "timeoutMs": u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX) })
            }
        };
        job.updated_at = ctx.now();
        self.save(ctx, &job, detail).await?;
        Ok(job)
    }

    async fn save(
        &self,
        ctx: &CommandContext,
        job: &PrintJobRecord,
        detail: serde_json::Value,
    ) -> Result<(), AppError> {
        let set = WriteSet::builder(ctx.write_context())
            .put_print_job(job.clone(), detail)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }
}
