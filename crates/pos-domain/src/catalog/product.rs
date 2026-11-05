//! The product aggregate (E06-T02, CAT-001/002/005/006/007, ADR-019).
//!
//! A product always has at least one variant, and the **variant is the sellable unit**:
//! prices, SKUs, barcodes, stock and sale lines all hang off variants. A "simple" product
//! has exactly one default variant.

use super::barcode::{BarcodeError, Symbology, normalize_barcode};
use super::unit::Unit;
use crate::{
    BarcodeId, CategoryId, ErrorCode, HasErrorCode, IdGenerator, Money, ProductId, Quantity, TaxCategoryId,
    VariantId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const MAX_NAME_LEN: usize = 120;
pub const MAX_SKU_LEN: usize = 40;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ProductType {
    /// Physical goods with tracked stock.
    Stock,
    /// Physical goods without stock tracking (e.g. loose bags).
    NonStock,
    /// Services; never stock-tracked.
    Service,
}

impl ProductType {
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "stock" => Some(Self::Stock),
            "non_stock" => Some(Self::NonStock),
            "service" => Some(Self::Service),
            _ => None,
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            ProductType::Stock => "stock",
            ProductType::NonStock => "non_stock",
            ProductType::Service => "service",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum BarcodeQualifier {
    Primary,
    Alias,
    /// A case or multipack code: one scan adds `pack_qty` units (e.g. 12).
    Pack,
}

impl BarcodeQualifier {
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "primary" => Some(Self::Primary),
            "alias" => Some(Self::Alias),
            "pack" => Some(Self::Pack),
            _ => None,
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            BarcodeQualifier::Primary => "primary",
            BarcodeQualifier::Alias => "alias",
            BarcodeQualifier::Pack => "pack",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Barcode {
    pub id: BarcodeId,
    /// Canonical form (`normalize_barcode`); the lookup key.
    pub value: String,
    /// As entered or scanned.
    pub raw: String,
    pub symbology: Symbology,
    pub qualifier: BarcodeQualifier,
    pub pack_qty: Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variant {
    pub id: VariantId,
    pub is_default: bool,
    pub sku: Option<String>,
    /// Option axes, e.g. `{"size": "M", "colour": "Red"}`. Empty for a simple product.
    pub options: BTreeMap<String, String>,
    pub price: Money,
    pub cost: Option<Money>,
    pub track_stock: bool,
    pub low_stock_threshold: Option<Quantity>,
    /// Quantity added per scan of a non-pack barcode (BAR-005).
    pub scan_qty: Quantity,
    pub barcodes: Vec<Barcode>,
    pub archived: bool,
}

impl Variant {
    /// Display suffix from the options, e.g. `"M / Red"` (sorted by axis name).
    pub fn label(&self) -> String {
        self.options.values().cloned().collect::<Vec<_>>().join(" / ")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub id: ProductId,
    pub name: String,
    pub description: Option<String>,
    pub category_id: Option<CategoryId>,
    pub product_type: ProductType,
    pub unit: Unit,
    pub tax_category_id: TaxCategoryId,
    pub quick_select: bool,
    pub archived: bool,
    pub variants: Vec<Variant>,
    /// Aggregate version (product + variants + barcodes), ADR-011.
    pub version: u64,
}

/// Input for a barcode, before normalisation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarcodeInput {
    pub raw: String,
    pub symbology_hint: Option<Symbology>,
    pub qualifier: BarcodeQualifier,
    pub pack_qty: Quantity,
}

impl BarcodeInput {
    pub fn alias(raw: &str) -> Self {
        Self {
            raw: raw.into(),
            symbology_hint: None,
            qualifier: BarcodeQualifier::Alias,
            pack_qty: Quantity::ONE,
        }
    }
}

/// Input for a variant. `id: None` creates a new one; `Some` keeps an existing one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantInput {
    pub id: Option<VariantId>,
    pub sku: Option<String>,
    pub options: BTreeMap<String, String>,
    pub price: Money,
    pub cost: Option<Money>,
    pub track_stock: bool,
    pub low_stock_threshold: Option<Quantity>,
    pub scan_qty: Quantity,
    pub barcodes: Vec<BarcodeInput>,
}

impl VariantInput {
    pub fn simple(price: Money) -> Self {
        Self {
            id: None,
            sku: None,
            options: BTreeMap::new(),
            price,
            cost: None,
            track_stock: true,
            low_stock_threshold: None,
            scan_qty: Quantity::ONE,
            barcodes: Vec::new(),
        }
    }
}

/// The full desired state of a product (create, or edit by replacing the variant set).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductInput {
    pub name: String,
    pub description: Option<String>,
    pub category_id: Option<CategoryId>,
    pub product_type: ProductType,
    pub unit: Unit,
    pub tax_category_id: TaxCategoryId,
    pub quick_select: bool,
    /// The first variant becomes the default.
    pub variants: Vec<VariantInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("a product name is required")]
    NameRequired,
    #[error("names can be at most {MAX_NAME_LEN} characters")]
    NameTooLong,
    #[error("a product needs at least one variant")]
    NoVariants,
    #[error("variant prices and costs cannot be negative")]
    NegativePrice,
    #[error("{0} cannot be fractional for unit {1:?}")]
    FractionNotAllowed(&'static str, Unit),
    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),
    #[error("SKU {0:?} is used more than once")]
    DuplicateSku(String),
    #[error("SKUs can be at most {MAX_SKU_LEN} characters")]
    SkuTooLong,
    #[error("barcode {0} is used more than once")]
    DuplicateBarcode(String),
    #[error("two variants have the same options")]
    DuplicateOptions,
    #[error("variant {0} does not belong to this product")]
    UnknownVariant(VariantId),
    #[error(transparent)]
    Barcode(#[from] BarcodeError),
    #[error("the product is archived")]
    Archived,
    #[error("product not found")]
    NotFound,
    #[error("barcode {value} already belongs to another product")]
    BarcodeTaken { value: String, owner: ProductId },
    #[error("SKU {sku} already belongs to another product")]
    SkuTaken { sku: String, owner: ProductId },
}

impl HasErrorCode for CatalogError {
    fn error_code(&self) -> ErrorCode {
        match self {
            CatalogError::DuplicateBarcode(_) | CatalogError::BarcodeTaken { .. } => {
                ErrorCode::CatalogDuplicateBarcode
            }
            CatalogError::DuplicateSku(_) | CatalogError::SkuTaken { .. } => ErrorCode::CatalogDuplicateSku,
            CatalogError::NotFound => ErrorCode::CatalogNotFound,
            CatalogError::Barcode(e) => e.error_code(),
            _ => ErrorCode::ValidationFailed,
        }
    }
}

fn clean_name(name: &str) -> Result<String, CatalogError> {
    let n = name.trim();
    if n.is_empty() || n.chars().all(char::is_control) {
        return Err(CatalogError::NameRequired);
    }
    if n.chars().count() > MAX_NAME_LEN {
        return Err(CatalogError::NameTooLong);
    }
    Ok(n.to_owned())
}

fn clean_sku(sku: Option<String>) -> Result<Option<String>, CatalogError> {
    let Some(s) = sku.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if s.chars().count() > MAX_SKU_LEN {
        return Err(CatalogError::SkuTooLong);
    }
    Ok(Some(s))
}

fn check_qty(q: Quantity, what: &'static str, unit: Unit, whole: bool) -> Result<(), CatalogError> {
    if !q.is_positive() {
        return Err(CatalogError::NotPositive(what));
    }
    if whole && !q.is_whole() {
        return Err(CatalogError::FractionNotAllowed(what, unit));
    }
    Ok(())
}

impl Product {
    /// Validates the input and builds a new product at version 1.
    pub fn create(input: ProductInput, ids: &dyn IdGenerator) -> Result<Product, CatalogError> {
        let mut p = Product {
            id: ProductId::generate(ids),
            name: String::new(),
            description: None,
            category_id: None,
            product_type: input.product_type,
            unit: input.unit,
            tax_category_id: input.tax_category_id,
            quick_select: false,
            archived: false,
            variants: Vec::new(),
            version: 0,
        };
        p.apply(input, ids)?;
        Ok(p)
    }

    /// Replaces the product's state with `input` and bumps the version. Variants and
    /// barcodes missing from the input are archived, never deleted (history).
    pub fn edit(&mut self, input: ProductInput, ids: &dyn IdGenerator) -> Result<(), CatalogError> {
        if self.archived {
            return Err(CatalogError::Archived);
        }
        self.apply(input, ids)
    }

    pub fn archive(&mut self) {
        self.archived = true;
        self.version += 1;
    }

    /// Active variants only.
    pub fn active_variants(&self) -> impl Iterator<Item = &Variant> {
        self.variants.iter().filter(|v| !v.archived)
    }

    fn apply(&mut self, input: ProductInput, ids: &dyn IdGenerator) -> Result<(), CatalogError> {
        let name = clean_name(&input.name)?;
        if input.variants.is_empty() {
            return Err(CatalogError::NoVariants);
        }
        let stock_possible = input.product_type == ProductType::Stock;
        let whole = !input.unit.allows_fraction();

        let mut skus = HashSet::new();
        let mut codes = HashSet::new();
        let mut option_sets = HashSet::new();
        let mut kept: Vec<Variant> = Vec::with_capacity(input.variants.len());
        for (i, v) in input.variants.into_iter().enumerate() {
            if v.price.is_negative() || v.cost.is_some_and(|c| c.is_negative()) {
                return Err(CatalogError::NegativePrice);
            }
            check_qty(v.scan_qty, "scan quantity", input.unit, whole)?;
            if let Some(t) = v.low_stock_threshold
                && t.is_negative()
            {
                return Err(CatalogError::NotPositive("low-stock threshold"));
            }
            let sku = clean_sku(v.sku)?;
            if let Some(s) = &sku
                && !skus.insert(s.to_lowercase())
            {
                return Err(CatalogError::DuplicateSku(s.clone()));
            }
            let options: BTreeMap<String, String> = v
                .options
                .into_iter()
                .map(|(k, val)| (k.trim().to_owned(), val.trim().to_owned()))
                .filter(|(k, val)| !k.is_empty() && !val.is_empty())
                .collect();
            if !option_sets.insert(options.clone()) {
                return Err(CatalogError::DuplicateOptions);
            }
            let existing = match v.id {
                Some(id) => Some(
                    self.variants
                        .iter()
                        .find(|x| x.id == id)
                        .ok_or(CatalogError::UnknownVariant(id))?,
                ),
                None => None,
            };
            let mut barcodes = Vec::with_capacity(v.barcodes.len());
            for b in v.barcodes {
                let n = normalize_barcode(&b.raw, b.symbology_hint)?;
                if !codes.insert(n.value.clone()) {
                    return Err(CatalogError::DuplicateBarcode(n.value));
                }
                if b.qualifier == BarcodeQualifier::Pack {
                    check_qty(b.pack_qty, "pack quantity", input.unit, whole)?;
                }
                // Keep the barcode's identity when the same code stays on the same variant.
                let id = existing
                    .and_then(|e| e.barcodes.iter().find(|x| x.value == n.value))
                    .map(|x| x.id)
                    .unwrap_or_else(|| BarcodeId::generate(ids));
                barcodes.push(Barcode {
                    id,
                    value: n.value,
                    raw: b.raw.trim().to_owned(),
                    symbology: n.symbology,
                    qualifier: b.qualifier,
                    pack_qty: if b.qualifier == BarcodeQualifier::Pack {
                        b.pack_qty
                    } else {
                        Quantity::ONE
                    },
                });
            }
            kept.push(Variant {
                id: existing.map(|e| e.id).unwrap_or_else(|| VariantId::generate(ids)),
                is_default: i == 0,
                sku,
                options,
                price: v.price,
                cost: v.cost,
                // CAT-006: non-stock and service items never track stock.
                track_stock: stock_possible && v.track_stock,
                low_stock_threshold: v.low_stock_threshold,
                scan_qty: v.scan_qty,
                barcodes,
                archived: false,
            });
        }
        // Variants dropped from the input are archived, keeping their history.
        let kept_ids: HashSet<VariantId> = kept.iter().map(|v| v.id).collect();
        for old in self.variants.iter().filter(|v| !kept_ids.contains(&v.id)) {
            let mut archived = old.clone();
            archived.archived = true;
            archived.is_default = false;
            kept.push(archived);
        }

        self.name = name;
        self.description = input
            .description
            .map(|d| d.trim().to_owned())
            .filter(|d| !d.is_empty());
        self.category_id = input.category_id;
        self.product_type = input.product_type;
        self.unit = input.unit;
        self.tax_category_id = input.tax_category_id;
        self.quick_select = input.quick_select;
        self.variants = kept;
        self.version += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UuidV7Generator;

    fn ids() -> UuidV7Generator {
        UuidV7Generator
    }

    fn input(variants: Vec<VariantInput>) -> ProductInput {
        ProductInput {
            name: " Cola 330ml ".into(),
            description: None,
            category_id: None,
            product_type: ProductType::Stock,
            unit: Unit::Ea,
            tax_category_id: TaxCategoryId::generate(&ids()),
            quick_select: false,
            variants,
        }
    }

    fn sized(size: &str, colour: &str, sku: &str, code: &str) -> VariantInput {
        let mut v = VariantInput::simple(Money::from_minor(1999));
        v.options = BTreeMap::from([("size".into(), size.into()), ("colour".into(), colour.into())]);
        v.sku = Some(sku.into());
        v.barcodes = vec![BarcodeInput::alias(code)];
        v
    }

    fn ean(n: u64) -> String {
        // Valid EAN-13s for tests: 12-digit body + check digit.
        let body: Vec<u8> = format!("{n:012}").bytes().map(|b| b - b'0').collect();
        format!("{n:012}{}", super::super::barcode::gtin_check_digit(&body))
    }

    #[test]
    fn simple_product_gets_one_default_variant() {
        let p = Product::create(input(vec![VariantInput::simple(Money::from_minor(120))]), &ids()).unwrap();
        assert_eq!(
            (p.name.as_str(), p.version, p.variants.len()),
            ("Cola 330ml", 1, 1)
        );
        assert!(p.variants[0].is_default && p.variants[0].label().is_empty());
    }

    #[test]
    fn variant_product_with_six_variants() {
        let mut vs = Vec::new();
        let mut n = 501_234_567_000;
        for size in ["S", "M", "L"] {
            for colour in ["Red", "Blue"] {
                n += 1;
                vs.push(sized(size, colour, &format!("TEE-{size}-{colour}"), &ean(n)));
            }
        }
        let p = Product::create(input(vs), &ids()).unwrap();
        assert_eq!(p.variants.len(), 6);
        assert_eq!(p.variants.iter().filter(|v| v.is_default).count(), 1);
        assert_eq!(p.variants[0].label(), "Red / S");
    }

    #[test]
    fn non_stock_and_service_never_track_stock() {
        for t in [ProductType::NonStock, ProductType::Service] {
            let mut i = input(vec![VariantInput::simple(Money::from_minor(500))]);
            i.product_type = t;
            assert!(!Product::create(i, &ids()).unwrap().variants[0].track_stock);
        }
    }

    #[test]
    fn units_govern_fractional_quantities() {
        let mut v = VariantInput::simple(Money::from_minor(1249));
        v.scan_qty = Quantity::from_milli(250);
        assert_eq!(
            Product::create(input(vec![v.clone()]), &ids()),
            Err(CatalogError::FractionNotAllowed("scan quantity", Unit::Ea))
        );
        let mut i = input(vec![v]);
        i.unit = Unit::Kg;
        assert!(Product::create(i, &ids()).is_ok());
    }

    #[test]
    fn pack_barcodes_carry_their_quantity() {
        let mut v = VariantInput::simple(Money::from_minor(120));
        v.barcodes = vec![
            BarcodeInput::alias(&ean(1)),
            BarcodeInput {
                raw: ean(2),
                symbology_hint: None,
                qualifier: BarcodeQualifier::Pack,
                pack_qty: Quantity::from_whole(12).unwrap(),
            },
        ];
        let p = Product::create(input(vec![v]), &ids()).unwrap();
        let pack = p.variants[0]
            .barcodes
            .iter()
            .find(|b| b.qualifier == BarcodeQualifier::Pack)
            .unwrap();
        assert_eq!(pack.pack_qty.milli(), 12_000);
    }

    #[test]
    fn duplicates_within_a_product_are_rejected() {
        let code = ean(7);
        let a = sized("S", "Red", "TEE-1", &code);
        let mut b = sized("M", "Red", "tee-1", &ean(8));
        assert!(matches!(
            Product::create(input(vec![a.clone(), b.clone()]), &ids()),
            Err(CatalogError::DuplicateSku(_))
        ));
        b.sku = Some("TEE-2".into());
        b.barcodes = vec![BarcodeInput::alias(&format!(" {code}\r"))];
        let err = Product::create(input(vec![a.clone(), b.clone()]), &ids()).unwrap_err();
        assert_eq!(err, CatalogError::DuplicateBarcode(code));
        assert_eq!(err.error_code(), ErrorCode::CatalogDuplicateBarcode);
        b.barcodes = vec![];
        b.options = a.options.clone();
        assert_eq!(
            Product::create(input(vec![a, b]), &ids()),
            Err(CatalogError::DuplicateOptions)
        );
    }

    #[test]
    fn validation_errors() {
        let mut i = input(vec![]);
        assert_eq!(Product::create(i.clone(), &ids()), Err(CatalogError::NoVariants));
        i.variants = vec![VariantInput::simple(Money::from_minor(-1))];
        assert_eq!(
            Product::create(i.clone(), &ids()),
            Err(CatalogError::NegativePrice)
        );
        i.variants = vec![VariantInput::simple(Money::ZERO)];
        i.name = "  ".into();
        assert_eq!(
            Product::create(i.clone(), &ids()),
            Err(CatalogError::NameRequired)
        );
        i.name = "x".repeat(121);
        assert_eq!(Product::create(i, &ids()), Err(CatalogError::NameTooLong));
        let mut v = VariantInput::simple(Money::ZERO);
        v.barcodes = vec![BarcodeInput {
            raw: "4006381333932".into(),
            symbology_hint: Some(Symbology::Ean13),
            qualifier: BarcodeQualifier::Primary,
            pack_qty: Quantity::ONE,
        }];
        assert!(matches!(
            Product::create(input(vec![v]), &ids()),
            Err(CatalogError::Barcode(_))
        ));
    }

    #[test]
    fn edits_bump_the_version_keep_identities_and_archive_dropped_variants() {
        let mut p = Product::create(
            input(vec![
                sized("S", "Red", "A", &ean(1)),
                sized("M", "Red", "B", &ean(2)),
            ]),
            &ids(),
        )
        .unwrap();
        let (first, second) = (p.variants[0].clone(), p.variants[1].clone());
        // Keep the first variant (same id, same barcode) and drop the second.
        let mut keep = sized("S", "Red", "A", &ean(1));
        keep.id = Some(first.id);
        keep.price = Money::from_minor(2499);
        p.edit(input(vec![keep]), &ids()).unwrap();
        assert_eq!(p.version, 2);
        let v1 = p.variants.iter().find(|v| v.id == first.id).unwrap();
        assert_eq!(
            (v1.price.minor(), v1.barcodes[0].id),
            (2499, first.barcodes[0].id)
        );
        let v2 = p.variants.iter().find(|v| v.id == second.id).unwrap();
        assert!(v2.archived && !v2.is_default);
        assert_eq!(p.active_variants().count(), 1);
        // A variant id from another product is refused.
        let mut stray = VariantInput::simple(Money::ZERO);
        stray.id = Some(VariantId::generate(&ids()));
        assert!(matches!(
            p.edit(input(vec![stray]), &ids()),
            Err(CatalogError::UnknownVariant(_))
        ));
        p.archive();
        assert_eq!(
            p.edit(input(vec![VariantInput::simple(Money::ZERO)]), &ids()),
            Err(CatalogError::Archived)
        );
    }
}
