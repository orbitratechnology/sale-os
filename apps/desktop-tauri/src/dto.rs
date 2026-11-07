//! IPC data shapes (docs/backlog/07-contracts.md §1).
//!
//! Money crosses as a decimal string of minor units, quantities as a decimal string of
//! milli-units (ADR-001, ADR-002): never a JS number. Versions and revisions are `u32`
//! on the wire: specta refuses `u64` because a JS number cannot hold it.

use crate::IpcError;
use pos_application::cart::CartView;
use pos_application::checkout::CompletedSale;
use pos_application::queries::{
    CatalogHit, MatchedBy, ProductFilter, ProductPage, ProductStatusFilter, TaxCategorySummary,
};
use pos_application::records::StaffRecord;
use pos_domain::cart::AddVia;
use pos_domain::catalog::{
    Barcode, BarcodeInput, BarcodeQualifier, Product, ProductInput, ProductType, Symbology, Unit, Variant,
    VariantInput,
};
use pos_domain::pricing::{Discount, DiscountValue, TaxLine, TaxMode};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{
    CartId, CartLineId, CategoryId, ErrorCode, Money, ProductId, Quantity, RatePpm, Role, SaleId,
    TaxCategoryId, UserId, VariantId,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeMap;

// ---------- wire conversions ----------

fn invalid(what: &str, value: &str) -> IpcError {
    IpcError::coded(
        ErrorCode::ValidationFailed,
        format!("{what}: {value:?} is not valid"),
    )
}

pub fn money_in(field: &str, s: &str) -> Result<Money, IpcError> {
    s.trim()
        .parse::<i64>()
        .map(Money::from_minor)
        .map_err(|_| invalid(field, s))
}

pub fn qty_in(field: &str, s: &str) -> Result<Quantity, IpcError> {
    s.trim()
        .parse::<i64>()
        .map(Quantity::from_milli)
        .map_err(|_| invalid(field, s))
}

pub fn rate_in(field: &str, ppm: u32) -> Result<RatePpm, IpcError> {
    RatePpm::new(ppm).map_err(|_| invalid(field, &ppm.to_string()))
}

pub fn version_out(v: u64) -> Result<u32, IpcError> {
    u32::try_from(v).map_err(IpcError::internal)
}

fn money(m: Money) -> String {
    m.minor().to_string()
}

fn qty(q: Quantity) -> String {
    q.milli().to_string()
}

// ---------- bootstrap, setup, session ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapState {
    pub setup_complete: bool,
    pub workspace: Option<WorkspaceDto>,
    pub session: Option<SessionDto>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDto {
    pub business_name: String,
    pub register_name: String,
    pub register_code: String,
    pub currency_code: String,
    /// Minor-unit digits of the currency, for parsing typed prices.
    pub currency_exponent: u8,
    /// `"sandbox"` until the owner goes live (PRD §7.1).
    pub mode: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkspaceInput {
    pub business_name: String,
    pub legal_name: Option<String>,
    /// Printed on receipts, one line each.
    pub address_lines: Vec<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub country_code: String,
    pub timezone: String,
    pub currency_code: String,
    pub prices_include_tax: bool,
    pub location_name: String,
    pub register_name: String,
    pub owner_name: String,
    pub owner_pin: String,
    pub tax_rates: Vec<TaxRateInput>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorkspaceDto {
    /// Sign the owner in with this and the PIN they just chose.
    pub owner_user_id: UserId,
    pub register_code: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TaxRateInput {
    pub name: String,
    /// Parts per million: 200000 = 20%.
    pub rate_ppm: u32,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    pub user_id: UserId,
    pub display_name: String,
    pub role: Role,
    /// The PIN was reset by an owner: ask for a new one before anything else.
    pub must_change_pin: bool,
}

// ---------- staff ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StaffDto {
    pub user_id: UserId,
    pub display_name: String,
    pub role: Role,
    pub active: bool,
    pub must_change_pin: bool,
    pub version: u32,
}

impl StaffDto {
    pub fn from_record(r: &StaffRecord) -> Result<Self, IpcError> {
        Ok(Self {
            user_id: r.user_id,
            display_name: r.display_name.clone(),
            role: r.role,
            active: r.active,
            must_change_pin: r.must_change_pin,
            version: version_out(r.version)?,
        })
    }
}

/// For the PIN pad: no PIN state, no versions.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SignInUser {
    pub user_id: UserId,
    pub display_name: String,
    pub role: Role,
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct NewStaffInput {
    pub display_name: String,
    pub role: Role,
    pub pin: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StaffUpdateInput {
    pub user_id: UserId,
    pub base_version: u32,
    pub display_name: Option<String>,
    pub role: Option<Role>,
}

// ---------- catalogue ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TaxCategoryDto {
    pub tax_category_id: TaxCategoryId,
    pub name: String,
    pub is_default: bool,
    pub rates: Vec<TaxRateDto>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TaxRateDto {
    pub name: String,
    pub rate_ppm: u32,
}

impl From<TaxCategorySummary> for TaxCategoryDto {
    fn from(c: TaxCategorySummary) -> Self {
        Self {
            tax_category_id: c.tax_category_id,
            name: c.name,
            is_default: c.is_default,
            rates: c
                .rates
                .into_iter()
                .map(|(name, r)| TaxRateDto {
                    name,
                    rate_ppm: r.ppm(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProductDto {
    pub product_id: ProductId,
    pub name: String,
    pub description: Option<String>,
    pub category_id: Option<CategoryId>,
    pub product_type: ProductType,
    pub unit: Unit,
    pub tax_category_id: TaxCategoryId,
    pub quick_select: bool,
    pub archived: bool,
    pub version: u32,
    pub variants: Vec<VariantDto>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VariantDto {
    pub variant_id: VariantId,
    pub is_default: bool,
    pub sku: Option<String>,
    pub options: BTreeMap<String, String>,
    pub price_minor: String,
    pub cost_minor: Option<String>,
    pub track_stock: bool,
    pub low_stock_threshold_milli: Option<String>,
    pub scan_qty_milli: String,
    pub barcodes: Vec<BarcodeDto>,
    pub archived: bool,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BarcodeDto {
    /// Canonical form, the lookup key.
    pub value: String,
    /// As entered or scanned.
    pub raw: String,
    pub symbology: Symbology,
    pub qualifier: BarcodeQualifier,
    pub pack_qty_milli: String,
}

impl ProductDto {
    pub fn from_product(p: &Product) -> Result<Self, IpcError> {
        Ok(Self {
            product_id: p.id,
            name: p.name.clone(),
            description: p.description.clone(),
            category_id: p.category_id,
            product_type: p.product_type,
            unit: p.unit,
            tax_category_id: p.tax_category_id,
            quick_select: p.quick_select,
            archived: p.archived,
            version: version_out(p.version)?,
            variants: p.variants.iter().map(VariantDto::from_variant).collect(),
        })
    }
}

impl VariantDto {
    fn from_variant(v: &Variant) -> Self {
        Self {
            variant_id: v.id,
            is_default: v.is_default,
            sku: v.sku.clone(),
            options: v.options.clone(),
            price_minor: money(v.price),
            cost_minor: v.cost.map(money),
            track_stock: v.track_stock,
            low_stock_threshold_milli: v.low_stock_threshold.map(qty),
            scan_qty_milli: qty(v.scan_qty),
            barcodes: v.barcodes.iter().map(BarcodeDto::from_barcode).collect(),
            archived: v.archived,
        }
    }
}

impl BarcodeDto {
    fn from_barcode(b: &Barcode) -> Self {
        Self {
            value: b.value.clone(),
            raw: b.raw.clone(),
            symbology: b.symbology,
            qualifier: b.qualifier,
            pack_qty_milli: qty(b.pack_qty),
        }
    }
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProductInputDto {
    pub name: String,
    pub description: Option<String>,
    pub category_id: Option<CategoryId>,
    pub product_type: ProductType,
    pub unit: Unit,
    pub tax_category_id: TaxCategoryId,
    pub quick_select: bool,
    /// The first variant becomes the default.
    pub variants: Vec<VariantInputDto>,
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VariantInputDto {
    /// Set when editing an existing variant.
    pub variant_id: Option<VariantId>,
    pub sku: Option<String>,
    pub options: BTreeMap<String, String>,
    pub price_minor: String,
    pub cost_minor: Option<String>,
    pub track_stock: bool,
    pub low_stock_threshold_milli: Option<String>,
    /// Defaults to one unit.
    pub scan_qty_milli: Option<String>,
    pub barcodes: Vec<BarcodeInputDto>,
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BarcodeInputDto {
    pub raw: String,
    pub symbology: Option<Symbology>,
    pub qualifier: BarcodeQualifier,
    /// Required for a pack barcode; ignored otherwise.
    pub pack_qty_milli: Option<String>,
}

impl ProductInputDto {
    pub fn into_domain(self) -> Result<ProductInput, IpcError> {
        let variants = self
            .variants
            .into_iter()
            .map(|v| {
                let barcodes = v
                    .barcodes
                    .into_iter()
                    .map(|b| {
                        Ok(BarcodeInput {
                            raw: b.raw,
                            symbology_hint: b.symbology,
                            qualifier: b.qualifier,
                            pack_qty: match b.pack_qty_milli {
                                Some(q) => qty_in("packQtyMilli", &q)?,
                                None => Quantity::ONE,
                            },
                        })
                    })
                    .collect::<Result<_, IpcError>>()?;
                Ok(VariantInput {
                    id: v.variant_id,
                    sku: v.sku,
                    options: v.options,
                    price: money_in("priceMinor", &v.price_minor)?,
                    cost: v
                        .cost_minor
                        .as_deref()
                        .map(|c| money_in("costMinor", c))
                        .transpose()?,
                    track_stock: v.track_stock,
                    low_stock_threshold: v
                        .low_stock_threshold_milli
                        .as_deref()
                        .map(|q| qty_in("lowStockThresholdMilli", q))
                        .transpose()?,
                    scan_qty: match v.scan_qty_milli {
                        Some(q) => qty_in("scanQtyMilli", &q)?,
                        None => Quantity::ONE,
                    },
                    barcodes,
                })
            })
            .collect::<Result<_, IpcError>>()?;
        Ok(ProductInput {
            name: self.name,
            description: self.description,
            category_id: self.category_id,
            product_type: self.product_type,
            unit: self.unit,
            tax_category_id: self.tax_category_id,
            quick_select: self.quick_select,
            variants,
        })
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CatalogHitDto {
    pub product_id: ProductId,
    pub variant_id: VariantId,
    pub name: String,
    /// Variant options, e.g. "M / Red"; empty for a simple product.
    pub label: String,
    pub sku: Option<String>,
    pub price_minor: String,
    pub unit: Unit,
    pub track_stock: bool,
    pub scan_qty_milli: String,
    /// `"barcode"`, `"sku"` or `"search"`.
    pub matched_by: String,
}

impl From<CatalogHit> for CatalogHitDto {
    fn from(h: CatalogHit) -> Self {
        Self {
            product_id: h.product_id,
            variant_id: h.variant_id,
            name: h.name,
            label: h.label,
            sku: h.sku,
            price_minor: money(h.price),
            unit: h.unit,
            track_stock: h.track_stock,
            scan_qty_milli: qty(h.scan_qty),
            matched_by: match h.matched_by {
                MatchedBy::Barcode => "barcode",
                MatchedBy::Sku => "sku",
                MatchedBy::Search => "search",
            }
            .into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ProductStatusDto {
    #[default]
    Active,
    Archived,
    All,
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProductFilterDto {
    /// Name, SKU or barcode; empty for all.
    pub query: String,
    pub status: ProductStatusDto,
    pub track_stock: Option<bool>,
    pub category_id: Option<CategoryId>,
}

impl From<ProductFilterDto> for ProductFilter {
    fn from(f: ProductFilterDto) -> Self {
        Self {
            query: f.query,
            status: match f.status {
                ProductStatusDto::Active => ProductStatusFilter::Active,
                ProductStatusDto::Archived => ProductStatusFilter::Archived,
                ProductStatusDto::All => ProductStatusFilter::All,
            },
            track_stock: f.track_stock,
            category_id: f.category_id,
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProductRowDto {
    pub product_id: ProductId,
    pub name: String,
    pub archived: bool,
    pub version: u32,
    pub variant_count: u32,
    pub sku: Option<String>,
    pub price_min_minor: String,
    pub price_max_minor: String,
    pub barcode_count: u32,
    pub track_stock: bool,
    pub quick_select: bool,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProductPageDto {
    pub rows: Vec<ProductRowDto>,
    /// Rows matching the filter, across all pages.
    pub total: u32,
}

impl ProductPageDto {
    pub fn from_page(p: ProductPage) -> Result<Self, IpcError> {
        let rows = p
            .rows
            .into_iter()
            .map(|r| {
                Ok(ProductRowDto {
                    product_id: r.product_id,
                    name: r.name,
                    archived: r.archived,
                    version: version_out(r.version)?,
                    variant_count: r.variant_count,
                    sku: r.sku,
                    price_min_minor: money(r.price_min),
                    price_max_minor: money(r.price_max),
                    barcode_count: r.barcode_count,
                    track_stock: r.track_stock,
                    quick_select: r.quick_select,
                })
            })
            .collect::<Result<_, IpcError>>()?;
        Ok(Self { rows, total: p.total })
    }
}

// ---------- cart ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CartViewDto {
    pub cart_id: CartId,
    /// Send back with `cart_clear`, `cart_void` and checkout.
    pub revision: u32,
    pub currency_code: String,
    pub currency_exponent: u8,
    pub tax_mode: TaxMode,
    /// Discounts above this rate need a reason, and a cashier needs an approval (E08-T02).
    pub discount_limit_ppm: u32,
    pub lines: Vec<CartLineDto>,
    pub order_discount: Option<DiscountDto>,
    pub subtotal_minor: String,
    pub line_discount_total_minor: String,
    pub order_discount_total_minor: String,
    pub tax_total_minor: String,
    pub tax_breakdown: Vec<TaxLineDto>,
    /// What the customer pays, before cash rounding.
    pub total_minor: String,
    pub item_count_milli: String,
    /// Lines whose catalogue price changed since they were added (POS-011).
    pub repriced_lines: Vec<CartLineId>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CartLineDto {
    pub line_id: CartLineId,
    pub variant_id: VariantId,
    pub product_id: ProductId,
    pub name: String,
    pub label: String,
    pub sku: Option<String>,
    pub unit: Unit,
    pub quantity_milli: String,
    /// The unit price charged: the override if any, else the catalogue price.
    pub unit_price_minor: String,
    pub price_overridden: bool,
    pub line_discount: Option<DiscountDto>,
    pub gross_minor: String,
    /// This line's discount plus its share of the order discount.
    pub discount_minor: String,
    pub tax_minor: String,
    pub total_minor: String,
    /// `"scan"`, `"search"`, `"quick_select"` or `"manual_code"`.
    pub added_via: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TaxLineDto {
    pub name: String,
    pub rate_ppm: u32,
    pub taxable_minor: String,
    pub tax_minor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DiscountDto {
    Percent {
        rate_ppm: u32,
        reason: Option<String>,
    },
    Fixed {
        amount_minor: String,
        reason: Option<String>,
    },
}

impl DiscountDto {
    fn from_domain(d: &Discount) -> Self {
        match &d.value {
            DiscountValue::Percent(r) => Self::Percent {
                rate_ppm: r.ppm(),
                reason: d.reason.clone(),
            },
            DiscountValue::Fixed(m) => Self::Fixed {
                amount_minor: money(*m),
                reason: d.reason.clone(),
            },
        }
    }

    pub fn into_domain(self) -> Result<Discount, IpcError> {
        let (value, reason) = match self {
            Self::Percent { rate_ppm, reason } => {
                (DiscountValue::Percent(rate_in("ratePpm", rate_ppm)?), reason)
            }
            Self::Fixed { amount_minor, reason } => (
                DiscountValue::Fixed(money_in("amountMinor", &amount_minor)?),
                reason,
            ),
        };
        Ok(Discount {
            value,
            reason: reason.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty()),
            authorized_by: None,
        })
    }
}

fn tax_line(t: &TaxLine) -> TaxLineDto {
    TaxLineDto {
        name: t.name.clone(),
        rate_ppm: t.rate.ppm(),
        taxable_minor: money(t.taxable),
        tax_minor: money(t.tax),
    }
}

fn via(v: &AddVia) -> &'static str {
    match v {
        AddVia::Scan { .. } => "scan",
        AddVia::Search => "search",
        AddVia::QuickSelect => "quick_select",
        AddVia::ManualCode => "manual_code",
    }
}

impl CartViewDto {
    pub fn from_view(v: &CartView) -> Result<Self, IpcError> {
        let lines = v
            .cart
            .lines
            .iter()
            .map(|l| {
                let p = v
                    .priced
                    .lines
                    .iter()
                    .find(|p| p.line_id == l.id)
                    .ok_or_else(|| IpcError::internal("priced line missing"))?;
                let discount = p
                    .line_discount
                    .checked_add(p.order_discount_alloc)
                    .map_err(|e| IpcError::from_error(&e))?;
                Ok(CartLineDto {
                    line_id: l.id,
                    variant_id: l.variant.variant_id,
                    product_id: l.variant.product_id,
                    name: l.variant.name.clone(),
                    label: l.variant.label.clone(),
                    sku: l.variant.sku.clone(),
                    unit: l.variant.unit,
                    quantity_milli: qty(l.quantity),
                    unit_price_minor: money(l.price_override.unwrap_or(l.variant.price)),
                    price_overridden: l.price_override.is_some(),
                    line_discount: l.line_discount.as_ref().map(DiscountDto::from_domain),
                    gross_minor: money(p.gross),
                    discount_minor: money(discount),
                    tax_minor: money(p.tax),
                    total_minor: money(p.total),
                    added_via: via(&l.added_via).into(),
                })
            })
            .collect::<Result<Vec<_>, IpcError>>()?;
        let items = v
            .cart
            .lines
            .iter()
            .try_fold(Quantity::ZERO, |acc, l| acc.checked_add(l.quantity))
            .map_err(|e| IpcError::from_error(&e))?;
        Ok(Self {
            cart_id: v.cart.id,
            revision: version_out(v.cart.revision)?,
            currency_code: v.settings.currency.code.to_string(),
            currency_exponent: v.settings.currency.exponent,
            tax_mode: v.settings.tax_mode,
            discount_limit_ppm: v.settings.discount_limit_ppm,
            lines,
            order_discount: v.cart.order_discount.as_ref().map(DiscountDto::from_domain),
            subtotal_minor: money(v.priced.subtotal),
            line_discount_total_minor: money(v.priced.line_discount_total),
            order_discount_total_minor: money(v.priced.order_discount_total),
            tax_total_minor: money(v.priced.tax_total),
            tax_breakdown: v.priced.tax_breakdown.iter().map(tax_line).collect(),
            total_minor: money(v.priced.total),
            item_count_milli: qty(items),
            repriced_lines: v.repriced_lines.clone(),
        })
    }
}

/// How a code reached `scan_submit`.
#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ScanSourceDto {
    Scanner,
    Manual,
}

/// `cart_add_variant`'s `via`: scans go through `scan_submit` instead.
#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AddViaDto {
    Search,
    QuickSelect,
}

#[derive(Debug, Serialize, Type)]
#[serde(tag = "outcome", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ScanOutcomeDto {
    /// A new line.
    Added { cart: CartViewDto, line_id: CartLineId },
    /// An existing line's quantity went up (BAR-005).
    Incremented { cart: CartViewDto, line_id: CartLineId },
    /// This delivery was already processed; the cart is unchanged.
    Duplicate { cart: CartViewDto },
    /// Nothing matches. The cart is unchanged (BAR-006).
    NotFound { code: String },
}

// ---------- checkout ----------

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TenderInputDto {
    pub kind: TenderKind,
    /// Cash: what the customer handed over. Other tenders: the amount charged.
    pub amount_minor: String,
    /// External tenders: the terminal's approval code or a transfer reference. Never a card number.
    pub reference: Option<String>,
}

impl TenderInputDto {
    pub fn into_domain(self) -> Result<TenderInput, IpcError> {
        Ok(TenderInput {
            kind: self.kind,
            amount: money_in("amountMinor", &self.amount_minor)?,
            external_reference: self.reference,
        })
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CompletedSaleDto {
    pub sale_id: SaleId,
    pub receipt_number: String,
    pub total_minor: String,
    pub tendered_minor: String,
    pub change_minor: String,
    pub rounding_minor: String,
    pub currency_exponent: u8,
    /// `"sandbox"` or `"live"`.
    pub mode: String,
    /// The sale is in the outbox, waiting for the cloud link (Stage 3).
    pub sync_pending: bool,
    /// Items this sale took below zero stock (allowed and flagged).
    pub negative_stock_items: u32,
}

impl CompletedSaleDto {
    pub fn from_completed(c: &CompletedSale) -> Self {
        Self {
            sale_id: c.sale.id,
            receipt_number: c.receipt_number.clone(),
            total_minor: money(c.sale.total),
            tendered_minor: money(c.sale.tendered_total),
            change_minor: money(c.sale.change),
            rounding_minor: money(c.sale.rounding),
            currency_exponent: c.sale.currency.exponent,
            mode: c.sale.mode.as_str().into(),
            sync_pending: c.outbox_sequence > 0,
            negative_stock_items: u32::try_from(c.negative_stock.len()).unwrap_or(u32::MAX),
        }
    }
}

// ---------- receipts ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptDto {
    pub receipt_number: String,
    /// 32 (58 mm paper) or 48 (80 mm).
    pub columns: u32,
    /// Exactly what the printer prints, line by line.
    pub lines: Vec<ReceiptLineDto>,
    pub sandbox: bool,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptLineDto {
    pub text: String,
    pub bold: bool,
}

impl ReceiptDto {
    pub fn from_rendered(r: &pos_application::receipts::RenderedReceipt) -> Self {
        Self {
            receipt_number: r.document.meta.receipt_number.clone(),
            columns: u32::try_from(r.columns).unwrap_or(48),
            lines: r
                .lines
                .iter()
                .map(|l| ReceiptLineDto {
                    text: l.text.clone(),
                    bold: l.style == pos_receipts::text::Style::Bold,
                })
                .collect(),
            sandbox: r.document.sandbox,
        }
    }
}
