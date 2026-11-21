//! Catalogue rules (E06). Barcode identification is separate from description and price
//! (PRD §6.2): a barcode carries an identifier; the store's catalogue supplies the rest.

pub mod barcode;
pub mod config;
pub mod import;
pub mod label;
pub mod product;
pub mod scan;
pub mod unit;

pub use barcode::{BarcodeError, NormalizedBarcode, Symbology, gtin_check_digit, normalize_barcode};
pub use label::{LabelError, LabelLine, LabelMeasure, label_line};
pub use product::{
    Barcode, BarcodeInput, BarcodeQualifier, CatalogError, Product, ProductInput, ProductType, Variant,
    VariantInput,
};
pub use scan::{
    CandidateSource, DecodedScan, EmbeddedMeasure, Gs1Data, Gs1Element, Gs1Price, Gs1Problem,
    LookupCandidate, MAX_SCAN_INPUT_LEN, MeasureKind, ScaledNumber, ScanDecodeConfig, ScanNote,
    ScanRuleError, ScannedSymbology, VariableMeasureRule, decode_scan, variable_measure_rules,
};
pub use unit::Unit;
