//! Catalogue rules (E06). Barcode identification is separate from description and price
//! (PRD §6.2): a barcode carries an identifier; the store's catalogue supplies the rest.

pub mod barcode;
pub mod config;
pub mod import;
pub mod product;
pub mod unit;

pub use barcode::{BarcodeError, NormalizedBarcode, Symbology, gtin_check_digit, normalize_barcode};
pub use product::{
    Barcode, BarcodeInput, BarcodeQualifier, CatalogError, Product, ProductInput, ProductType, Variant,
    VariantInput,
};
pub use unit::Unit;
