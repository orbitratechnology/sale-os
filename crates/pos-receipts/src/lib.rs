//! Canonical `ReceiptDocument` and its renderers (ADR-014): the document model (E13-T02),
//! monospace text for previews and printers (E13-T03), and the ESC/POS encoder (E13-T01).

pub mod document;
pub mod escpos;
pub mod text;

pub use document::{
    CONTENT_VERSION, CopyKind, DocumentKind, ReceiptContext, ReceiptDocument, return_receipt, sale_receipt,
};
