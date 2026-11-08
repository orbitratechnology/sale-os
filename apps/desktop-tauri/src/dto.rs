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
    /// Sellable on hand across tracked variants; `null` when nothing tracks stock.
    pub on_hand_milli: Option<String>,
    /// A tracked variant is below zero: show the red "Negative stock" flag (INV-008).
    pub negative_stock: bool,
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
                    on_hand_milli: r.on_hand.map(qty),
                    negative_stock: r.negative_stock,
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
    /// E11-T04: set when the cart holds more of this item than is in stock: what is on hand
    /// (may be 0 or negative). A hint; selling anyway is the negative-stock policy's call.
    pub only_in_stock_milli: Option<String>,
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
                    only_in_stock_milli: v.short_stock.get(&l.variant.variant_id).map(|q| qty(*q)),
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
    /// Print the receipt right away (`receipt.auto_print`).
    pub auto_print: bool,
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
            auto_print: c.auto_print,
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

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PrintJobDto {
    pub job_id: pos_domain::PrintJobId,
    /// `completed`, `failed` (the printer refused it; retry is safe) or `unknown` (no
    /// answer: ask the operator whether it printed).
    pub status: String,
    pub reprint: bool,
    /// For `failed`: `paper_out`, `cover_open`, `disconnected` or `rejected`.
    pub error_code: Option<String>,
    pub attempts: u32,
}

impl PrintJobDto {
    pub fn from_record(j: &pos_application::records::PrintJobRecord) -> Self {
        Self {
            job_id: j.job_id,
            status: j.status.as_str().into(),
            reprint: j.reprint,
            error_code: j.last_error_code.clone(),
            attempts: j.attempt_count,
        }
    }
}

// ---------- returns (E12) ----------

/// A sale found for a return: per line, what was sold and what can still come back.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReturnableSaleDto {
    pub sale_id: SaleId,
    pub receipt_number: String,
    /// `"sandbox"` or `"live"`.
    pub mode: String,
    pub currency_exponent: u8,
    pub lines: Vec<ReturnableLineDto>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReturnableLineDto {
    pub sale_line_id: pos_domain::SaleLineId,
    pub line_no: u32,
    pub name: String,
    pub label: String,
    pub unit: Unit,
    /// Whether returning it can put stock back (false for services).
    pub track_stock: bool,
    pub sold_milli: String,
    pub returned_milli: String,
    pub available_milli: String,
    /// What the customer paid for the whole line, after discounts.
    pub total_minor: String,
}

impl ReturnableSaleDto {
    pub fn from_sale(s: &pos_domain::returns::ReturnableSale) -> Self {
        Self {
            sale_id: s.sale_id,
            receipt_number: s.receipt_number.clone(),
            mode: s.mode.as_str().into(),
            currency_exponent: s.currency.exponent,
            lines: s
                .lines
                .iter()
                .map(|l| ReturnableLineDto {
                    sale_line_id: l.sale_line_id,
                    line_no: l.line_no,
                    name: l.name.clone(),
                    label: l.variant_label.clone(),
                    unit: l.unit,
                    track_stock: l.track_stock,
                    sold_milli: qty(l.quantity),
                    returned_milli: qty(l.returned),
                    available_milli: qty(l.available()),
                    total_minor: money(l.total),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReturnLineInputDto {
    pub sale_line_id: pos_domain::SaleLineId,
    pub quantity_milli: String,
    pub disposition: pos_domain::returns::Disposition,
}

impl ReturnLineInputDto {
    pub fn into_domain(self) -> Result<pos_domain::returns::ReturnLineRequest, IpcError> {
        Ok(pos_domain::returns::ReturnLineRequest {
            sale_line_id: self.sale_line_id,
            quantity: qty_in("quantityMilli", &self.quantity_milli)?,
            disposition: self.disposition,
        })
    }
}

/// What a return would refund, before it is made.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReturnPlanDto {
    pub subtotal_minor: String,
    pub tax_minor: String,
    pub total_minor: String,
    pub lines: Vec<PlannedReturnLineDto>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlannedReturnLineDto {
    pub sale_line_id: pos_domain::SaleLineId,
    pub name: String,
    pub quantity_milli: String,
    pub refund_minor: String,
    /// What will happen to the goods (a service line is always `no_stock_change`).
    pub disposition: pos_domain::returns::Disposition,
}

impl ReturnPlanDto {
    pub fn from_plan(p: &pos_domain::returns::ReturnPlan) -> Self {
        Self {
            subtotal_minor: money(p.subtotal),
            tax_minor: money(p.tax),
            total_minor: money(p.total),
            lines: p
                .lines
                .iter()
                .map(|l| PlannedReturnLineDto {
                    sale_line_id: l.sale_line_id,
                    name: l.name.clone(),
                    quantity_milli: qty(l.quantity),
                    refund_minor: money(l.refund),
                    disposition: l.disposition,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CompletedReturnDto {
    pub return_id: pos_domain::ReturnId,
    pub return_number: String,
    pub original_receipt_number: String,
    pub total_minor: String,
    pub currency_exponent: u8,
    /// Whether a manager approved it.
    pub approved: bool,
}

impl CompletedReturnDto {
    pub fn from_completed(c: &pos_application::returns::CompletedReturn) -> Self {
        Self {
            return_id: c.ret.id,
            return_number: c.return_number.clone(),
            original_receipt_number: c.ret.original_receipt_number.clone(),
            total_minor: money(c.ret.total),
            currency_exponent: c.ret.currency.exponent,
            approved: c.ret.approver.is_some(),
        }
    }
}

// ---------- sales history (E12-T03) ----------

#[derive(Debug, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SalesFilterDto {
    /// Business dates, `YYYY-MM-DD`, inclusive.
    pub from: Option<String>,
    pub to: Option<String>,
    /// A receipt number, or just its digits.
    pub receipt: Option<String>,
    pub cashier_user_id: Option<UserId>,
    pub tender: Option<TenderKind>,
}

impl SalesFilterDto {
    pub fn into_filter(self) -> Result<pos_application::queries::SalesFilter, IpcError> {
        let date =
            |field: &str, v: Option<String>| -> Result<Option<pos_domain::time::BusinessDate>, IpcError> {
                match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    None => Ok(None),
                    Some(s) => s.parse().map(Some).map_err(|_| invalid(field, s)),
                }
            };
        Ok(pos_application::queries::SalesFilter {
            from: date("from", self.from)?,
            to: date("to", self.to)?,
            receipt: self.receipt,
            cashier: self.cashier_user_id,
            tender: self.tender,
        })
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaleRowDto {
    pub sale_id: SaleId,
    pub receipt_number: String,
    pub local_time: String,
    pub business_date: String,
    pub cashier_name: String,
    /// `"sandbox"` or `"live"`.
    pub mode: String,
    pub currency_exponent: u8,
    pub total_minor: String,
    pub item_count_milli: String,
    pub tenders: Vec<TenderKind>,
    pub refunded_minor: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SalesPageDto {
    pub rows: Vec<SaleRowDto>,
    /// Pass back as `after` for the next page; `null` on the last page.
    pub next: Option<String>,
}

impl SalesPageDto {
    pub fn from_page(p: &pos_application::records::SalesPage) -> Self {
        Self {
            rows: p
                .rows
                .iter()
                .map(|r| SaleRowDto {
                    sale_id: r.sale_id,
                    receipt_number: r.receipt_number.clone(),
                    local_time: r.local_time.clone(),
                    business_date: r.business_date.clone(),
                    cashier_name: r.cashier_name.clone(),
                    mode: r.mode.as_str().into(),
                    currency_exponent: r.currency_exponent,
                    total_minor: money(r.total),
                    item_count_milli: qty(r.item_count),
                    tenders: r.tenders.clone(),
                    refunded_minor: money(r.refunded),
                })
                .collect(),
            next: p.next.as_ref().map(|c| c.encode()),
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaleDetailDto {
    pub sale_id: SaleId,
    pub receipt_number: String,
    pub local_time: String,
    pub business_date: String,
    pub cashier_name: String,
    pub approver_name: Option<String>,
    pub mode: String,
    pub currency_code: String,
    pub currency_exponent: u8,
    pub tax_mode: TaxMode,
    pub subtotal_minor: String,
    pub line_discount_total_minor: String,
    pub order_discount_total_minor: String,
    pub tax_total_minor: String,
    pub rounding_minor: String,
    pub total_minor: String,
    pub tendered_minor: String,
    pub change_minor: String,
    pub tax_breakdown: Vec<TaxLineDto>,
    pub lines: Vec<SaleDetailLineDto>,
    pub tenders: Vec<SaleTenderDto>,
    pub returns: Vec<SaleReturnDto>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaleDetailLineDto {
    pub line_no: u32,
    pub name: String,
    pub label: String,
    pub sku: Option<String>,
    pub unit: Unit,
    pub quantity_milli: String,
    pub unit_price_minor: String,
    pub price_overridden: bool,
    pub gross_minor: String,
    /// The line's discount plus its share of the order discount.
    pub discount_minor: String,
    pub tax_minor: String,
    pub total_minor: String,
    pub returned_milli: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaleTenderDto {
    pub kind: TenderKind,
    pub amount_minor: String,
    pub received_minor: Option<String>,
    pub change_minor: String,
    pub status: String,
    /// `"operator"`: recorded by staff. `"provider"`: confirmed by a payment integration.
    pub confirmation_source: String,
    pub reference: Option<String>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaleReturnDto {
    pub return_id: pos_domain::ReturnId,
    pub return_number: String,
    pub local_time: String,
    pub reason: String,
    pub total_minor: String,
    pub refunds: Vec<TenderKind>,
}

impl SaleDetailDto {
    pub fn from_detail(d: &pos_application::records::SaleDetail) -> Self {
        Self {
            sale_id: d.sale_id,
            receipt_number: d.receipt_number.clone(),
            local_time: d.local_time.clone(),
            business_date: d.business_date.clone(),
            cashier_name: d.cashier_name.clone(),
            approver_name: d.approver_name.clone(),
            mode: d.mode.as_str().into(),
            currency_code: d.currency.code.as_str().into(),
            currency_exponent: d.currency.exponent,
            tax_mode: if d.prices_include_tax {
                TaxMode::Inclusive
            } else {
                TaxMode::Exclusive
            },
            subtotal_minor: money(d.subtotal),
            line_discount_total_minor: money(d.line_discounts),
            order_discount_total_minor: money(d.order_discount),
            tax_total_minor: money(d.tax_total),
            rounding_minor: money(d.rounding),
            total_minor: money(d.total),
            tendered_minor: money(d.tendered),
            change_minor: money(d.change),
            tax_breakdown: d
                .tax_breakdown
                .iter()
                .map(|t| TaxLineDto {
                    name: t.name.clone(),
                    rate_ppm: t.rate_ppm,
                    taxable_minor: money(t.taxable),
                    tax_minor: money(t.tax),
                })
                .collect(),
            lines: d
                .lines
                .iter()
                .map(|l| SaleDetailLineDto {
                    line_no: l.line_no,
                    name: l.name.clone(),
                    label: l.variant_label.clone(),
                    sku: l.sku.clone(),
                    unit: l.unit,
                    quantity_milli: qty(l.quantity),
                    unit_price_minor: money(l.unit_price),
                    price_overridden: l.price_overridden,
                    gross_minor: money(l.gross),
                    discount_minor: money(l.discount),
                    tax_minor: money(l.tax),
                    total_minor: money(l.total),
                    returned_milli: qty(l.returned),
                })
                .collect(),
            tenders: d
                .tenders
                .iter()
                .map(|t| SaleTenderDto {
                    kind: t.kind,
                    amount_minor: money(t.amount),
                    received_minor: t.received.map(money),
                    change_minor: money(t.change),
                    status: t.status.clone(),
                    confirmation_source: t.confirmation_source.clone(),
                    reference: t.reference.clone(),
                })
                .collect(),
            returns: d
                .returns
                .iter()
                .map(|r| SaleReturnDto {
                    return_id: r.return_id,
                    return_number: r.return_number.clone(),
                    local_time: r.local_time.clone(),
                    reason: r.reason.clone(),
                    total_minor: money(r.total),
                    refunds: r.refunds.clone(),
                })
                .collect(),
        }
    }
}

// ---------- cash sessions (E15-T02) ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CashSessionDto {
    pub cash_session_id: pos_domain::CashSessionId,
    /// `R01-Z-0001`.
    pub number: String,
    pub open: bool,
    pub opened_by_name: String,
    pub opened_local: String,
    pub opening_float_minor: String,
    pub business_date: String,
    pub closed_by_name: Option<String>,
    pub closed_local: Option<String>,
    pub currency_exponent: u8,
}

impl CashSessionDto {
    pub fn from_record(r: &pos_application::records::CashSessionRecord) -> Self {
        Self {
            cash_session_id: r.id,
            number: r.number.clone(),
            open: r.open,
            opened_by_name: r.opened_by_name.clone(),
            opened_local: r.opened_local.clone(),
            opening_float_minor: money(r.opening_float),
            business_date: r.business_date.clone(),
            closed_by_name: r.closed_by_name.clone(),
            closed_local: r.closed_local.clone(),
            currency_exponent: r.currency_exponent,
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TenderTotalDto {
    pub kind: TenderKind,
    pub count: u32,
    pub amount_minor: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SessionTotalsDto {
    pub sales_count: u32,
    pub sales_total_minor: String,
    pub tax_total_minor: String,
    pub discount_total_minor: String,
    pub rounding_total_minor: String,
    /// Amounts applied to sales by tender kind (cash net of change).
    pub tenders: Vec<TenderTotalDto>,
    pub returns_count: u32,
    pub refunds_total_minor: String,
    pub refunds: Vec<TenderTotalDto>,
}

impl SessionTotalsDto {
    pub fn from_totals(t: &pos_domain::cash::SessionTotals) -> Self {
        let list = |v: &[pos_domain::cash::TenderTotal]| {
            v.iter()
                .map(|t| TenderTotalDto {
                    kind: t.kind,
                    count: t.count,
                    amount_minor: money(t.amount),
                })
                .collect()
        };
        Self {
            sales_count: t.sales_count,
            sales_total_minor: money(t.sales_total),
            tax_total_minor: money(t.tax_total),
            discount_total_minor: money(t.discount_total),
            rounding_total_minor: money(t.rounding_total),
            tenders: list(&t.tenders),
            returns_count: t.returns_count,
            refunds_total_minor: money(t.refunds_total),
            refunds: list(&t.refunds),
        }
    }
}

/// The drawer as the till sees it. Totals and expected cash are `null` for a role that may
/// not see the variance (a cashier's count is blind).
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CashStateDto {
    pub session: Option<CashSessionDto>,
    /// Sales need an open session right now (live mode with the setting on).
    pub required: bool,
    pub totals: Option<SessionTotalsDto>,
    pub expected_minor: Option<String>,
}

impl CashStateDto {
    pub fn from_state(s: &pos_application::cash::CashState) -> Self {
        Self {
            session: s.session.as_ref().map(CashSessionDto::from_record),
            required: s.required,
            totals: s.totals.as_ref().map(SessionTotalsDto::from_totals),
            expected_minor: s.expected.map(money),
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ClosedCashDto {
    pub session: CashSessionDto,
    pub counted_minor: String,
    /// `null` for a blind count.
    pub expected_minor: Option<String>,
    pub variance_minor: Option<String>,
    pub totals: Option<SessionTotalsDto>,
}

impl ClosedCashDto {
    pub fn from_closed(c: &pos_application::cash::ClosedCash) -> Self {
        Self {
            session: CashSessionDto::from_record(&c.session),
            counted_minor: money(c.counted),
            expected_minor: c.expected.map(money),
            variance_minor: c.variance.map(money),
            totals: c.totals.as_ref().map(SessionTotalsDto::from_totals),
        }
    }
}

// ---------- inventory documents (E11-T02/T03) ----------

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DocumentLineInputDto {
    pub variant_id: VariantId,
    /// Milli-units. Signed only for a count correction or "other" adjustment.
    pub quantity_milli: String,
    pub unit_cost_minor: Option<String>,
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DocumentDraftDto {
    pub doc_type: pos_domain::stock_docs::DocumentType,
    pub reason: Option<pos_domain::stock_docs::AdjustReason>,
    pub supplier_name: Option<String>,
    pub reference: Option<String>,
    pub note: Option<String>,
    pub lines: Vec<DocumentLineInputDto>,
}

impl DocumentDraftDto {
    pub fn into_domain(self) -> Result<pos_domain::stock_docs::DocumentDraft, IpcError> {
        Ok(pos_domain::stock_docs::DocumentDraft {
            doc_type: self.doc_type,
            reason: self.reason,
            supplier_name: self.supplier_name,
            reference: self.reference,
            note: self.note,
            lines: self
                .lines
                .into_iter()
                .map(|l| {
                    Ok(pos_domain::stock_docs::DocumentLine {
                        variant_id: l.variant_id,
                        quantity: qty_in("quantityMilli", &l.quantity_milli)?,
                        unit_cost: l
                            .unit_cost_minor
                            .as_deref()
                            .map(|c| money_in("unitCostMinor", c))
                            .transpose()?,
                    })
                })
                .collect::<Result<_, IpcError>>()?,
        })
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PostedDocumentDto {
    pub document_id: pos_domain::InventoryDocumentId,
    /// `R01-IN-000001`.
    pub document_no: String,
    pub doc_type: pos_domain::stock_docs::DocumentType,
    /// Stock movements written (a damage adjustment writes two per line).
    pub movement_count: u32,
}

impl PostedDocumentDto {
    pub fn from_posted(p: &pos_application::stock::PostedDocument) -> Self {
        Self {
            document_id: p.doc.id,
            document_no: p.document_no.clone(),
            doc_type: p.doc.plan.doc_type,
            movement_count: u32::try_from(p.doc.plan.movements.len()).unwrap_or(u32::MAX),
        }
    }
}

// ---------- stock projection (E11-T06) ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StockDriftDto {
    pub variant_id: VariantId,
    pub bucket: String,
    pub recorded_milli: String,
    pub from_movements_milli: String,
}

impl StockDriftDto {
    pub fn list(d: &[pos_application::records::StockDrift]) -> Vec<Self> {
        d.iter()
            .map(|d| Self {
                variant_id: d.variant_id,
                bucket: d.bucket.clone(),
                recorded_milli: qty(d.recorded),
                from_movements_milli: qty(d.from_movements),
            })
            .collect()
    }
}

// ---------- reports (E14) ----------

/// Business dates, `YYYY-MM-DD`, inclusive.
#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DateRangeDto {
    pub from: String,
    pub to: String,
}

impl DateRangeDto {
    pub fn parse(
        &self,
    ) -> Result<(pos_domain::time::BusinessDate, pos_domain::time::BusinessDate), IpcError> {
        Ok((
            self.from
                .trim()
                .parse()
                .map_err(|_| invalid("from", &self.from))?,
            self.to.trim().parse().map_err(|_| invalid("to", &self.to))?,
        ))
    }
}

/// Where the numbers come from (PRD §15): `"device"` until the cloud link (Stage 3).
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReportScopeDto {
    pub scope: String,
    /// Milliseconds since the epoch, as a string.
    pub as_of_ms: String,
}

impl ReportScopeDto {
    pub fn device(now: pos_domain::Timestamp) -> Self {
        Self {
            scope: "device".into(),
            as_of_ms: now.as_millis().to_string(),
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SalesFiguresDto {
    pub sales_count: u32,
    pub gross_minor: String,
    pub line_discounts_minor: String,
    pub order_discounts_minor: String,
    pub discounts_minor: String,
    pub tax_minor: String,
    pub rounding_minor: String,
    pub sales_total_minor: String,
    pub returns_count: u32,
    pub refunds_minor: String,
    pub refunded_tax_minor: String,
    pub net_incl_tax_minor: String,
    pub net_excl_tax_minor: String,
}

impl SalesFiguresDto {
    fn from_figures(f: &pos_application::reports::SalesFigures) -> Self {
        Self {
            sales_count: f.sales_count,
            gross_minor: money(f.gross),
            line_discounts_minor: money(f.line_discounts),
            order_discounts_minor: money(f.order_discounts),
            discounts_minor: money(f.discounts()),
            tax_minor: money(f.tax),
            rounding_minor: money(f.rounding),
            sales_total_minor: money(f.sales_total),
            returns_count: f.returns_count,
            refunds_minor: money(f.refunds),
            refunded_tax_minor: money(f.refunded_tax),
            net_incl_tax_minor: money(f.net_incl_tax()),
            net_excl_tax_minor: money(f.net_excl_tax()),
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DayFiguresDto {
    pub date: String,
    pub figures: SalesFiguresDto,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SalesSummaryDto {
    pub days: Vec<DayFiguresDto>,
    pub total: SalesFiguresDto,
    /// Gross is tax-inclusive when true.
    pub prices_include_tax: bool,
    pub currency_exponent: u8,
    pub scope: ReportScopeDto,
}

impl SalesSummaryDto {
    pub fn from_summary(s: &pos_application::reports::SalesSummary, now: pos_domain::Timestamp) -> Self {
        Self {
            days: s
                .days
                .iter()
                .map(|d| DayFiguresDto {
                    date: d.date.to_string(),
                    figures: SalesFiguresDto::from_figures(&d.figures),
                })
                .collect(),
            total: SalesFiguresDto::from_figures(&s.total),
            prices_include_tax: s.prices_include_tax,
            currency_exponent: s.currency_exponent,
            scope: ReportScopeDto::device(now),
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TenderMixRowDto {
    pub kind: TenderKind,
    /// `"operator"` (recorded by staff) or `"provider"` (confirmed by a payment integration).
    pub confirmation_source: String,
    pub count: u32,
    pub taken_minor: String,
    pub refund_count: u32,
    pub refunded_minor: String,
    pub net_minor: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TenderMixDto {
    pub rows: Vec<TenderMixRowDto>,
    pub scope: ReportScopeDto,
}

impl TenderMixDto {
    pub fn from_rows(rows: &[pos_application::reports::TenderMixRow], now: pos_domain::Timestamp) -> Self {
        Self {
            rows: rows
                .iter()
                .map(|r| TenderMixRowDto {
                    kind: r.kind,
                    confirmation_source: r.confirmation_source.clone(),
                    count: r.count,
                    taken_minor: money(r.taken),
                    refund_count: r.refund_count,
                    refunded_minor: money(r.refunded),
                    net_minor: money(r.net()),
                })
                .collect(),
            scope: ReportScopeDto::device(now),
        }
    }
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum TopByDto {
    Quantity,
    Value,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TopProductDto {
    pub variant_id: VariantId,
    pub name: String,
    pub label: String,
    pub quantity_milli: String,
    pub value_minor: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TopProductsDto {
    pub rows: Vec<TopProductDto>,
    pub scope: ReportScopeDto,
}

impl TopProductsDto {
    pub fn from_rows(rows: &[pos_application::reports::TopProductRow], now: pos_domain::Timestamp) -> Self {
        Self {
            rows: rows
                .iter()
                .map(|r| TopProductDto {
                    variant_id: r.variant_id,
                    name: r.name.clone(),
                    label: r.label.clone(),
                    quantity_milli: qty(r.quantity),
                    value_minor: money(r.value),
                })
                .collect(),
            scope: ReportScopeDto::device(now),
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StockRowDto {
    pub variant_id: VariantId,
    pub name: String,
    pub label: String,
    pub sku: Option<String>,
    pub unit: Unit,
    pub sellable_milli: String,
    pub damaged_milli: String,
    pub low_stock_threshold_milli: Option<String>,
    /// `"ok"`, `"low"` or `"negative"`.
    pub status: String,
    pub last_movement_ms: Option<String>,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StockReportDto {
    pub rows: Vec<StockRowDto>,
    pub scope: ReportScopeDto,
}

impl StockReportDto {
    pub fn from_rows(rows: &[pos_application::reports::StockRow], now: pos_domain::Timestamp) -> Self {
        Self {
            rows: rows
                .iter()
                .map(|r| StockRowDto {
                    variant_id: r.variant_id,
                    name: r.name.clone(),
                    label: r.label.clone(),
                    sku: r.sku.clone(),
                    unit: r.unit,
                    sellable_milli: qty(r.sellable),
                    damaged_milli: qty(r.damaged),
                    low_stock_threshold_milli: r.low_stock_threshold.map(qty),
                    status: r.status.as_str().into(),
                    last_movement_ms: r.last_movement_at.map(|t| t.as_millis().to_string()),
                })
                .collect(),
            scope: ReportScopeDto::device(now),
        }
    }
}

// ---------- receipt PDF (E13-T04) ----------

/// `receipt`: the receipt as printed, on the till's paper width; `a4`: an A4 page.
#[derive(Copy, Clone, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PdfPaperDto {
    Receipt,
    A4,
}

impl PdfPaperDto {
    pub fn into_domain(self) -> pos_application::receipts::PdfPaper {
        match self {
            PdfPaperDto::Receipt => pos_application::receipts::PdfPaper::Receipt,
            PdfPaperDto::A4 => pos_application::receipts::PdfPaper::A4,
        }
    }
}

/// A file saved where the user chose.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SavedFileDto {
    pub path: String,
    pub file_name: String,
}

// ---------- CSV exports (E14-T04) ----------

#[derive(Copy, Clone, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportDatasetDto {
    Transactions,
    Tenders,
    Returns,
    Products,
    Stock,
    SalesSummary,
    TenderMix,
    TopProducts,
}

impl ExportDatasetDto {
    pub fn into_domain(self) -> pos_application::export::Dataset {
        use pos_application::export::Dataset as D;
        match self {
            Self::Transactions => D::Transactions,
            Self::Tenders => D::Tenders,
            Self::Returns => D::Returns,
            Self::Products => D::Products,
            Self::Stock => D::Stock,
            Self::SalesSummary => D::SalesSummary,
            Self::TenderMix => D::TenderMix,
            Self::TopProducts => D::TopProducts,
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExportResultDto {
    /// Where the file was saved.
    pub path: String,
    pub file_name: String,
    pub rows: u32,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StockHistoryDto {
    pub movement_id: pos_domain::MovementId,
    pub local_time: String,
    pub movement_type: String,
    /// `sellable` or `damaged`.
    pub bucket: String,
    pub delta_milli: String,
    /// `sale`, `return` or `inventory_document`.
    pub source_type: String,
    pub source_id: String,
    /// The receipt, return or document number.
    pub source_number: String,
    pub actor_name: String,
}

impl StockHistoryDto {
    pub fn list(rows: &[pos_application::records::StockHistoryRow]) -> Vec<Self> {
        rows.iter()
            .map(|r| Self {
                movement_id: r.movement_id,
                local_time: r.local_time.clone(),
                movement_type: r.movement_type.clone(),
                bucket: r.bucket.clone(),
                delta_milli: qty(r.delta),
                source_type: r.source_type.clone(),
                source_id: r.source_id.clone(),
                source_number: r.source_number.clone(),
                actor_name: r.actor_name.clone(),
            })
            .collect()
    }
}

// ---------- audit viewer (E15-T01) ----------

#[derive(Debug, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AuditFilterDto {
    /// An exact action (`sale.completed`) or a group ending in `.` (`cart.`).
    pub action: Option<String>,
    pub user_id: Option<UserId>,
    /// Business dates, `YYYY-MM-DD`, inclusive.
    pub from: Option<String>,
    pub to: Option<String>,
}

impl AuditFilterDto {
    pub fn into_filter(self) -> Result<pos_application::audit_log::AuditFilter, IpcError> {
        let date =
            |field: &str, v: Option<String>| -> Result<Option<pos_domain::time::BusinessDate>, IpcError> {
                match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    None => Ok(None),
                    Some(s) => s.parse().map(Some).map_err(|_| invalid(field, s)),
                }
            };
        Ok(pos_application::audit_log::AuditFilter {
            action: self.action,
            user: self.user_id,
            from: date("from", self.from)?,
            to: date("to", self.to)?,
        })
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AuditRowDto {
    pub audit_id: String,
    pub local_time: String,
    pub action: String,
    pub actor_name: Option<String>,
    pub approver_name: Option<String>,
    pub entity_type: String,
    pub entity_id: String,
    /// A receipt, return, document or session number, a staff or product name.
    pub entity_label: Option<String>,
    /// The details as recorded (JSON).
    pub metadata: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AuditPageDto {
    pub rows: Vec<AuditRowDto>,
    pub next: Option<String>,
}

impl AuditPageDto {
    pub fn from_page(p: pos_application::audit_log::AuditPage) -> Self {
        Self {
            rows: p
                .rows
                .into_iter()
                .map(|r| AuditRowDto {
                    audit_id: r.audit_id,
                    local_time: r.local_time,
                    action: r.action,
                    actor_name: r.actor_name,
                    approver_name: r.approver_name,
                    entity_type: r.entity_type,
                    entity_id: r.entity_id,
                    entity_label: r.entity_label,
                    metadata: r.metadata,
                })
                .collect(),
            next: p.next,
        }
    }
}

// ---------- settings (E04-T03) ----------

#[derive(Debug, Serialize, Type)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SettingKindDto {
    Bool,
    Int {
        min: i32,
        max: i32,
    },
    /// Parts per million (100000 = 10%).
    Ppm,
    /// One of these JSON values.
    Choice {
        options: Vec<String>,
    },
    Lines {
        max_lines: u32,
        max_chars: u32,
    },
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SettingDto {
    pub key: String,
    pub group: String,
    pub label: String,
    pub help: String,
    pub kind: SettingKindDto,
    /// The current value as JSON text.
    pub value_json: String,
    pub default_json: String,
    /// Pass back as `baseVersion` when changing it.
    pub version: u32,
}

impl SettingDto {
    pub fn from_value(v: &pos_application::settings::SettingValue) -> Result<Self, IpcError> {
        use pos_application::settings::SettingKind as K;
        let d = v.def;
        Ok(Self {
            key: d.key.into(),
            group: d.group.into(),
            label: d.label.into(),
            help: d.help.into(),
            kind: match d.kind {
                K::Bool => SettingKindDto::Bool,
                K::Int { min, max } => SettingKindDto::Int {
                    min: i32::try_from(min).map_err(IpcError::internal)?,
                    max: i32::try_from(max).map_err(IpcError::internal)?,
                },
                K::Ppm => SettingKindDto::Ppm,
                K::Choice(o) => SettingKindDto::Choice {
                    options: o.iter().map(|s| (*s).to_owned()).collect(),
                },
                K::Lines { max_lines, max_chars } => SettingKindDto::Lines {
                    max_lines: u32::try_from(max_lines).map_err(IpcError::internal)?,
                    max_chars: u32::try_from(max_chars).map_err(IpcError::internal)?,
                },
            },
            value_json: v.value.to_string(),
            default_json: d.default.into(),
            version: version_out(v.version)?,
        })
    }
}

/// What the till needs from the settings, for anyone signed in.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesDto {
    /// `"Enter"`, `"Tab"` or `"none"`.
    pub scanner_suffix: String,
    pub scanner_max_key_gap_ms: u32,
    /// 0: never lock.
    pub lock_timeout_secs: u32,
}

// ---------- categories and tax configuration (E06-T05) ----------

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CategoryDto {
    pub category_id: CategoryId,
    pub parent_id: Option<CategoryId>,
    pub name: String,
    pub archived: bool,
    pub version: u32,
}

impl CategoryDto {
    pub fn from_record(r: &pos_application::records::CategoryRecord) -> Result<Self, IpcError> {
        Ok(Self {
            category_id: r.category_id,
            parent_id: r.parent_id,
            name: r.name.clone(),
            archived: r.archived,
            version: version_out(r.version)?,
        })
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TaxRateConfigDto {
    pub tax_rate_id: pos_domain::TaxRateId,
    pub name: String,
    pub rate_ppm: u32,
    /// Business date, inclusive; `null`: always.
    pub from: Option<String>,
    /// Business date, exclusive; `null`: open.
    pub to: Option<String>,
    pub archived: bool,
    pub version: u32,
}

impl TaxRateConfigDto {
    pub fn from_record(r: &pos_application::records::TaxRateRecord) -> Result<Self, IpcError> {
        Ok(Self {
            tax_rate_id: r.tax_rate_id,
            name: r.name.clone(),
            rate_ppm: r.rate.ppm(),
            from: r.effective_from.map(|d| d.to_string()),
            to: r.effective_to.map(|d| d.to_string()),
            archived: r.archived,
            version: version_out(r.version)?,
        })
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TaxCategoryConfigDto {
    pub tax_category_id: TaxCategoryId,
    pub name: String,
    pub is_default: bool,
    pub rate_ids: Vec<pos_domain::TaxRateId>,
    pub archived: bool,
    pub version: u32,
}

impl TaxCategoryConfigDto {
    pub fn from_record(r: &pos_application::records::TaxCategoryRecord) -> Result<Self, IpcError> {
        Ok(Self {
            tax_category_id: r.tax_category_id,
            name: r.name.clone(),
            is_default: r.is_default,
            rate_ids: r.rate_ids.clone(),
            archived: r.archived,
            version: version_out(r.version)?,
        })
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TaxConfigDto {
    pub rates: Vec<TaxRateConfigDto>,
    pub categories: Vec<TaxCategoryConfigDto>,
}

pub fn opt_date(field: &str, v: Option<String>) -> Result<Option<pos_domain::time::BusinessDate>, IpcError> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => s.parse().map(Some).map_err(|_| invalid(field, s)),
    }
}

// ---------- business profile (E04-T03) ----------

/// The business details printed on receipts and this till's name. Country, time zone,
/// currency and tax mode are shown but fixed after setup.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BusinessProfileDto {
    pub name: String,
    pub legal_name: Option<String>,
    pub tax_registration_no: Option<String>,
    pub address_lines: Vec<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub register_name: String,
    pub register_code: String,
    pub country_code: String,
    pub timezone: String,
    pub currency_code: String,
    pub prices_include_tax: bool,
    pub business_version: u32,
    pub register_version: u32,
}

impl BusinessProfileDto {
    pub fn from_view(v: &pos_application::config::ProfileView) -> Result<Self, IpcError> {
        let b = &v.business;
        Ok(Self {
            name: b.name.clone(),
            legal_name: b.legal_name.clone(),
            tax_registration_no: b.tax_registration_no.clone(),
            address_lines: v.address_lines(),
            phone: b.phone.clone(),
            email: b.email.clone(),
            register_name: v.register.name.clone(),
            register_code: v.register.code.clone(),
            country_code: b.country_code.clone(),
            timezone: b.timezone.clone(),
            currency_code: b.currency.code.to_string(),
            prices_include_tax: b.prices_include_tax,
            business_version: version_out(b.version)?,
            register_version: version_out(v.register.version)?,
        })
    }
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BusinessProfileInputDto {
    pub name: String,
    pub legal_name: Option<String>,
    pub tax_registration_no: Option<String>,
    pub address_lines: Vec<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub register_name: String,
}

impl BusinessProfileInputDto {
    pub fn into_domain(self) -> pos_domain::catalog::config::BusinessProfile {
        pos_domain::catalog::config::BusinessProfile {
            name: self.name,
            legal_name: self.legal_name,
            tax_registration_no: self.tax_registration_no,
            address_lines: self.address_lines,
            phone: self.phone,
            email: self.email,
            register_name: self.register_name,
        }
    }
}
