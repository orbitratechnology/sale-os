//! Canonical `ReceiptDocument` and its renderers (ADR-014): the document model (E13-T02),
//! monospace text for previews and printers (E13-T03), the printer encoder for ESC/POS, Star
//! Line Mode and plain text with code pages (E13-T01/T05), and PDF (E13-T04).

pub mod codepage;
#[rustfmt::skip]
mod codepage_tables;
pub mod document;
pub mod escpos;
pub mod pdf;
pub mod printer;
pub mod star;
pub mod text;

pub use codepage::CodePage;
pub use printer::{CutMode, Dialect, DrawerPin, EncodeOptions, drawer_kick, encode, fit_line};

pub use document::{
    CONTENT_VERSION, CopyKind, DocumentKind, ReceiptContext, ReceiptDocument, ZContext, ZSection,
    return_receipt, sale_receipt, z_summary,
};
