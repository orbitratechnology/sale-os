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

/// A sale receipt, a refund receipt for a return, or a cash session's Z summary.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    #[default]
    Sale,
    Return,
    CashSession,
}

impl DocumentKind {
    fn is_sale(&self) -> bool {
        *self == DocumentKind::Sale
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptDocument {
    pub version: u32,
    /// Omitted for sales, so sale documents are unchanged.
    #[serde(default, skip_serializing_if = "DocumentKind::is_sale")]
    pub kind: DocumentKind,
    /// For a return: the receipt of the original sale (RET-003).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_receipt: Option<String>,
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
    /// The Z summary of a closed cash session (kind `cash_session` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<ZSection>,
}

/// What a Z summary adds: the session, its takings and the drawer count (E15-T02).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZSection {
    pub opened_local: String,
    pub opened_by: String,
    pub opening_float: Money,
    pub totals: pos_domain::cash::SessionTotals,
    pub expected: Money,
    pub counted: Money,
    pub variance: Money,
    pub note: Option<String>,
}

/// Who opened and closed a session, in words, for its Z summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZContext {
    pub session_number: String,
    pub opened_local: String,
    pub opened_by: String,
    pub closed_local: String,
    pub currency_exponent: u8,
    pub sandbox: bool,
}

/// The Z summary of a closed session. `ctx.cashier_name` is who closed it.
pub fn z_summary(
    closed: &pos_domain::cash::ClosedSession,
    ctx: &ReceiptContext,
    z: &ZContext,
) -> ReceiptDocument {
    ReceiptDocument {
        version: CONTENT_VERSION,
        kind: DocumentKind::CashSession,
        original_receipt: None,
        copy: CopyKind::Original,
        sandbox: z.sandbox,
        header: Header {
            business_name: ctx.business_name.clone(),
            address_lines: ctx.address_lines.clone(),
            phone: ctx.phone.clone(),
            tax_registration_no: ctx.tax_registration_no.clone(),
            lines: Vec::new(),
        },
        meta: Meta {
            receipt_number: z.session_number.clone(),
            local_time: z.closed_local.clone(),
            register_name: ctx.register_name.clone(),
            register_code: ctx.register_code.clone(),
            cashier_name: ctx.cashier_name.clone(),
        },
        currency_exponent: z.currency_exponent,
        lines: Vec::new(),
        totals: Totals {
            subtotal: Money::ZERO,
            line_discounts: Money::ZERO,
            order_discount: Money::ZERO,
            order_discount_label: None,
            tax: Vec::new(),
            prices_include_tax: true,
            rounding: Money::ZERO,
            total: Money::ZERO,
        },
        tenders: Vec::new(),
        change: Money::ZERO,
        footer: Vec::new(),
        barcode: z.session_number.clone(),
        z: Some(ZSection {
            opened_local: z.opened_local.clone(),
            opened_by: z.opened_by.clone(),
            opening_float: closed.opening_float,
            totals: closed.totals.clone(),
            expected: closed.expected,
            counted: closed.counted,
            variance: closed.variance,
            note: closed.note.clone(),
        }),
    }
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
        kind: DocumentKind::Sale,
        original_receipt: None,
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
        z: None,
    }
}

/// The refund receipt for a return (E13-T02), from the return snapshot only.
pub fn return_receipt(
    ret: &pos_domain::returns::ReturnSnapshot,
    ctx: &ReceiptContext,
    return_number: &str,
    sandbox: bool,
) -> ReceiptDocument {
    let lines = ret
        .lines
        .iter()
        .map(|l| Line {
            name: l.line.name.clone(),
            variant_label: l.line.variant_label.clone(),
            quantity: quantity(l.line.quantity, l.line.unit),
            unit_price: l.line.refund,
            gross: l.line.refund,
            discount_label: None,
            discount: Money::ZERO,
            price_overridden: false,
        })
        .collect();
    ReceiptDocument {
        version: CONTENT_VERSION,
        kind: DocumentKind::Return,
        original_receipt: Some(ret.original_receipt_number.clone()),
        copy: CopyKind::Original,
        sandbox,
        header: Header {
            business_name: ctx.business_name.clone(),
            address_lines: ctx.address_lines.clone(),
            phone: ctx.phone.clone(),
            tax_registration_no: ctx.tax_registration_no.clone(),
            lines: ctx.header_lines.clone(),
        },
        meta: Meta {
            receipt_number: return_number.into(),
            local_time: pos_domain::time::local_date_time(ret.occurred_at, &ret.timezone).unwrap_or_default(),
            register_name: ctx.register_name.clone(),
            register_code: ctx.register_code.clone(),
            cashier_name: ctx.cashier_name.clone(),
        },
        currency_exponent: ret.currency.exponent,
        lines,
        totals: Totals {
            // Like a sale with tax included: the lines add up to the subtotal; the tax
            // refunded is shown as included.
            subtotal: ret.total,
            line_discounts: Money::ZERO,
            order_discount: Money::ZERO,
            order_discount_label: None,
            tax: vec![TaxRow {
                name: "Tax refunded".into(),
                amount: ret.tax,
            }],
            prices_include_tax: true,
            rounding: Money::ZERO,
            total: ret.total,
        },
        tenders: ret
            .refunds
            .iter()
            .map(|r| TenderRow {
                label: tender_label(r.tender.kind).into(),
                amount: r.tender.amount,
                received: None,
                external: !r.tender.kind.is_cash(),
                reference: r.tender.external_reference.clone(),
            })
            .collect(),
        change: Money::ZERO,
        footer: ctx.footer_lines.clone(),
        barcode: return_number.into(),
        z: None,
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

pub(crate) fn tender_label(k: TenderKind) -> &'static str {
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
