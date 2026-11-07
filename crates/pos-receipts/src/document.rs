//! `ReceiptDocument` v1 (E13-T02, RCP-001, ADR-014): what a receipt says, independent of
//! paper width or output (preview, ESC/POS, PDF). Built **only** from the sale snapshot and
//! the shop's receipt settings, so changing the catalogue later never changes a receipt.

use pos_domain::pricing::DiscountValue;
use pos_domain::sale::SaleSnapshot;
use pos_domain::settlement::TenderKind;
use pos_domain::{Money, NumberFormat};
use serde::{Deserialize, Serialize};

/// Bumped when the document's shape changes; stored with each receipt.
pub const CONTENT_VERSION: u32 = 1;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyKind {
    Original,
    /// RCP-007: every printed copy after the first says so.
    Reprint,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptDocument {
    pub version: u32,
    pub copy: CopyKind,
    /// Practice sale: printed with a SANDBOX marker.
    pub sandbox: bool,
    pub header: Header,
    pub meta: Meta,
    pub currency_exponent: u8,
    pub lines: Vec<Line>,
    pub totals: Totals,
    pub tenders: Vec<TenderRow>,
    pub change: Money,
    pub footer: Vec<String>,
    /// Printed as a Code128 barcode where the printer can.
    pub barcode: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub business_name: String,
    pub address_lines: Vec<String>,
    pub phone: Option<String>,
    pub tax_registration_no: Option<String>,
    /// Custom lines from the receipt settings (E13-T07).
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub receipt_number: String,
    /// In the business's time zone, `YYYY-MM-DD HH:MM`.
    pub local_time: String,
    pub register_name: String,
    pub register_code: String,
    pub cashier_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub name: String,
    pub variant_label: String,
    /// Display quantity, e.g. "2" or "0.25 kg".
    pub quantity: String,
    pub unit_price: Money,
    /// Before discounts.
    pub gross: Money,
    /// "10%" or "" for a fixed amount; `None` when there is no line discount.
    pub discount_label: Option<String>,
    pub discount: Money,
    pub price_overridden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub subtotal: Money,
    pub line_discounts: Money,
    pub order_discount: Money,
    pub order_discount_label: Option<String>,
    pub tax: Vec<TaxRow>,
    pub prices_include_tax: bool,
    pub rounding: Money,
    pub total: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxRow {
    pub name: String,
    pub amount: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenderRow {
    pub label: String,
    pub amount: Money,
    /// Cash handed over (cash only).
    pub received: Option<Money>,
    /// Card on a separate terminal, transfer, QR: recorded by the operator (PAY-003).
    pub external: bool,
    pub reference: Option<String>,
}

/// What the receipt needs besides the sale: the shop, the till, who served.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptContext {
    pub business_name: String,
    pub address_lines: Vec<String>,
    pub phone: Option<String>,
    pub tax_registration_no: Option<String>,
    pub header_lines: Vec<String>,
    pub footer_lines: Vec<String>,
    pub register_name: String,
    pub register_code: String,
    pub cashier_name: String,
}

/// The receipt number before the store allocates it (filled in inside the transaction).
pub const PENDING_NUMBER: &str = "";

/// The sale receipt, from the immutable snapshot only.
pub fn sale_receipt(
    sale: &SaleSnapshot,
    ctx: &ReceiptContext,
    receipt_number: &str,
    copy: CopyKind,
) -> ReceiptDocument {
    let lines = sale
        .lines
        .iter()
        .map(|l| Line {
            name: l.name.clone(),
            variant_label: l.variant_label.clone(),
            quantity: quantity(l.quantity, l.unit),
            unit_price: l.unit_price,
            gross: l.gross,
            discount_label: l.discount.as_ref().map(|d| percent_label(&d.value)),
            discount: l.line_discount,
            price_overridden: l.price_overridden,
        })
        .collect();
    let tenders = sale
        .tenders
        .iter()
        .map(|t| TenderRow {
            label: tender_label(t.tender.kind).into(),
            amount: t.tender.amount,
            received: t.tender.received,
            external: !t.tender.kind.is_cash(),
            reference: t.tender.external_reference.clone(),
        })
        .collect();
    ReceiptDocument {
        version: CONTENT_VERSION,
        copy,
        sandbox: sale.mode == pos_domain::sale::SaleMode::Sandbox,
        header: Header {
            business_name: ctx.business_name.clone(),
            address_lines: ctx.address_lines.clone(),
            phone: ctx.phone.clone(),
            tax_registration_no: ctx.tax_registration_no.clone(),
            lines: ctx.header_lines.clone(),
        },
        meta: Meta {
            receipt_number: receipt_number.into(),
            local_time: pos_domain::time::local_date_time(sale.occurred_at, &sale.timezone)
                .unwrap_or_default(),
            register_name: ctx.register_name.clone(),
            register_code: ctx.register_code.clone(),
            cashier_name: ctx.cashier_name.clone(),
        },
        currency_exponent: sale.currency.exponent,
        lines,
        totals: Totals {
            subtotal: sale.subtotal,
            line_discounts: sale.line_discount_total,
            order_discount: sale.order_discount_total,
            order_discount_label: sale.order_discount.as_ref().map(|d| percent_label(&d.value)),
            tax: sale
                .tax_breakdown
                .iter()
                .map(|t| TaxRow {
                    name: t.name.clone(),
                    amount: t.tax,
                })
                .collect(),
            prices_include_tax: sale.prices_include_tax,
            rounding: sale.rounding,
            total: sale.total,
        },
        tenders,
        change: sale.change,
        footer: ctx.footer_lines.clone(),
        barcode: receipt_number.into(),
    }
}

impl ReceiptDocument {
    /// The same receipt marked as a reprint (RCP-007).
    pub fn as_reprint(&self) -> Self {
        Self {
            copy: CopyKind::Reprint,
            ..self.clone()
        }
    }
}

fn tender_label(k: TenderKind) -> &'static str {
    match k {
        TenderKind::Cash => "Cash",
        TenderKind::CardExternal => "Card (external)",
        TenderKind::BankTransferExternal => "Bank transfer",
        TenderKind::QrExternal => "QR payment",
        TenderKind::OtherExternal => "Other payment",
    }
}

fn percent_label(v: &DiscountValue) -> String {
    match v {
        DiscountValue::Percent(r) => {
            let ppm = r.ppm();
            let whole = ppm / 10_000;
            let frac = ppm % 10_000;
            if frac == 0 {
                format!("{whole}%")
            } else {
                format!("{whole}.{}%", format!("{frac:04}").trim_end_matches('0'))
            }
        }
        DiscountValue::Fixed(_) => String::new(),
    }
}

fn quantity(q: pos_domain::Quantity, unit: pos_domain::catalog::Unit) -> String {
    let m = q.milli();
    let (whole, frac) = (m / 1000, (m % 1000).abs());
    let num = if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{}", format!("{frac:03}").trim_end_matches('0'))
    };
    if unit.allows_fraction() {
        format!("{num} {}", unit.code())
    } else {
        num
    }
}

/// Money for receipts: plain digits with a dot (the receipt is not locale-formatted yet).
pub(crate) fn amount(m: Money, exponent: u8) -> String {
    pos_domain::Currency::new(
        pos_domain::CurrencyCode::new("XXX").expect("valid code"),
        exponent,
    )
    .map(|c| c.format(m, NumberFormat::EN_US))
    .unwrap_or_else(|_| m.minor().to_string())
}
