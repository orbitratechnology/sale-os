//! VAT tax invoices for registered buyers (D-02, Sri Lanka Gazette Extraordinary 2481/22 and
//! IRD Circular SEC/2026/E/03).
//!
//! The till receipt stays the record of a sale. A VAT-registered buyer can ask for a "TAX
//! INVOICE" for it, which states:
//! - the supplier's and the purchaser's TIN (9 digits), names and addresses;
//! - a serial number `YYMMM_QQQQ_XXXXX` (here `QQQQ` is the register code and `XXXXX` the
//!   register's sequence, never restarted);
//! - the invoice date and the date of supply as `MM/DD/YYYY`;
//! - each VAT-taxable line excluding VAT, then the value of supply, the VAT and the total.
//!
//! Lines without VAT are left off and counted. Everything comes from what the sale recorded,
//! so the invoice agrees with the receipt to the cent.
//!
//! One invoice per sale. Issuing records the invoice and its document in one transaction,
//! audited `tax_invoice.issued`. It is local to this till for now: not synced to the cloud,
//! carried by backups and the full export.

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, ReceiptQueries, SalesQueries};
use crate::receipts::{ReceiptService, RenderedReceipt};
use crate::records::TaxInvoiceRecord;
use crate::write::NumberSlot;
use crate::{LocalStore, WriteSet};
use pos_domain::sale::SaleMode;
use pos_domain::{ErrorCode, Money, Permission, SaleId, TaxInvoiceId};
use pos_receipts::{Meta, TaxInvoiceLine, TaxInvoiceSection};
use std::sync::Arc;

const NUMBER: NumberSlot = NumberSlot("tax_invoice");

pub struct TaxInvoiceService {
    pub store: Arc<dyn LocalStore>,
    pub receipts: Arc<dyn ReceiptQueries>,
    pub sales: Arc<dyn SalesQueries>,
    /// For the business time zone.
    pub pricing: Arc<dyn CartQueries>,
}

/// Who the invoice is for, as on the purchaser's VAT registration certificate.
#[derive(Clone, Debug)]
pub struct Purchaser {
    pub tin: String,
    pub name: String,
    pub address: Vec<String>,
}

fn invalid(msg: impl Into<String>) -> AppError {
    AppError::Validation(msg.into(), ErrorCode::ValidationFailed)
}

/// A TIN as the IRD issues it: nine digits. Spaces and dashes typed in between are dropped.
pub fn normalize_tin(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    (digits.len() == 9 && digits.chars().all(|c| c.is_ascii_digit())).then_some(digits)
}

/// `MM/DD/YYYY`, as the gazette writes dates, from a business date (`YYYY-MM-DD`).
fn us_date(iso: &str) -> Option<String> {
    let (y, rest) = iso.split_once('-')?;
    let (m, d) = rest.split_once('-')?;
    (y.len() == 4 && m.len() == 2 && d.len() == 2).then(|| format!("{m}/{d}/{y}"))
}

/// The serial number's `YYMMM_` part from a business date (`YYYY-MM-DD`): `26NOV_`.
fn number_prefix(iso: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    let month: usize = iso.get(5..7)?.parse().ok()?;
    Some(format!(
        "{}{}_",
        iso.get(2..4)?,
        MONTHS.get(month.checked_sub(1)?)?
    ))
}

/// `amount ÷ quantity` per unit, rounded half up (amounts here are never negative).
fn unit_price(amount: Money, quantity_milli: i64) -> Money {
    let q = i128::from(quantity_milli.max(1));
    let v = (i128::from(amount.minor()) * 1000 + q / 2) / q;
    Money::from_minor(i64::try_from(v).unwrap_or(i64::MAX))
}

impl TaxInvoiceService {
    /// Issues the tax invoice of `sale`. Refused when the sale already has one, has no VAT, or
    /// a TIN, name or address is missing or malformed.
    pub async fn issue(
        &self,
        ctx: &CommandContext,
        sale: SaleId,
        purchaser: Purchaser,
    ) -> Result<RenderedReceipt, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let actor = ctx.actor()?.clone();
        let info = self
            .receipts
            .receipt_info(ctx.register.register_id, actor.user_id)
            .await?;
        let supplier_tin = info
            .tax_registration_no
            .as_deref()
            .and_then(normalize_tin)
            .ok_or_else(|| {
                invalid("enter the shop's 9-digit TIN in Settings > Business before issuing tax invoices")
            })?;
        let purchaser_tin =
            normalize_tin(&purchaser.tin).ok_or_else(|| invalid("the purchaser's TIN must be 9 digits"))?;
        let purchaser_name = purchaser.name.trim().to_owned();
        if purchaser_name.is_empty() || purchaser_name.chars().count() > 120 {
            return Err(invalid("enter the purchaser's name (up to 120 characters)"));
        }
        let purchaser_address: Vec<String> = purchaser
            .address
            .iter()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();
        if purchaser_address.is_empty()
            || purchaser_address.len() > 4
            || purchaser_address.iter().any(|l| l.chars().count() > 80)
        {
            return Err(invalid(
                "enter the purchaser's address (1 to 4 lines, up to 80 characters each)",
            ));
        }
        if let Some((_, json)) = self.receipts.tax_invoice_receipt(sale).await? {
            let number = serde_json::from_str::<pos_receipts::ReceiptDocument>(&json)
                .map(|d| d.meta.receipt_number)
                .unwrap_or_default();
            return Err(invalid(format!(
                "this sale already has tax invoice {number}; print it again from the sale"
            )));
        }
        let detail = self
            .sales
            .sale_detail(sale)
            .await?
            .ok_or_else(|| invalid("no such sale on this device"))?;
        if detail.tax_total.minor() <= 0 {
            return Err(invalid("this sale has no VAT, so it has no tax invoice"));
        }

        let exponent = detail.currency.exponent;
        let mut lines = Vec::new();
        let mut omitted = 0u32;
        let mut value = Money::ZERO;
        for l in detail.lines.iter() {
            // Only VAT-taxable supplies (Gazette 2481/22, 4.2).
            if l.tax.minor() <= 0 {
                omitted += 1;
                continue;
            }
            let amount = l
                .total
                .checked_sub(l.tax)
                .map_err(|e| AppError::Internal(e.to_string()))?;
            value = value
                .checked_add(amount)
                .map_err(|e| AppError::Internal(e.to_string()))?;
            // The unit price after discounts, so quantity × unit price ≈ amount.
            let unit_price = unit_price(amount, l.quantity.milli());
            let description = if l.variant_label.is_empty() {
                l.name.clone()
            } else {
                format!("{} {}", l.name, l.variant_label)
            };
            let quantity = match l.unit.code() {
                "ea" => l.quantity.to_string(),
                unit => format!("{} {unit}", l.quantity),
            };
            lines.push(TaxInvoiceLine {
                description,
                quantity,
                unit_price,
                amount,
            });
        }
        let vat: Vec<pos_receipts::document::TaxRow> = detail
            .tax_breakdown
            .iter()
            .filter(|t| t.tax.minor() > 0)
            .map(|t| pos_receipts::document::TaxRow {
                name: t.name.clone(),
                amount: t.tax,
            })
            .collect();
        let total = value
            .checked_add(detail.tax_total)
            .map_err(|e| AppError::Internal(e.to_string()))?;

        let timezone = self.pricing.pricing_settings().await?.timezone;
        let tz =
            pos_domain::time::parse_time_zone(&timezone).map_err(|e| AppError::Internal(e.to_string()))?;
        let today = pos_domain::time::business_date(ctx.now(), tz)
            .map_err(|e| AppError::Internal(e.to_string()))?
            .to_string();
        let bad_date = |d: &str| AppError::Internal(format!("unexpected date {d}"));
        let invoice_date = us_date(&today).ok_or_else(|| bad_date(&today))?;
        let supply_date = us_date(&detail.business_date).ok_or_else(|| bad_date(&detail.business_date))?;
        let prefix = number_prefix(&today).ok_or_else(|| bad_date(&today))?;
        let section = TaxInvoiceSection {
            supplier_tin,
            purchaser_tin: purchaser_tin.clone(),
            purchaser_name: purchaser_name.clone(),
            purchaser_address: purchaser_address.clone(),
            invoice_date,
            supply_date,
            sale_receipt: detail.receipt_number.clone(),
            lines,
            value_of_supply: value,
            vat,
            total,
            payment: detail
                .tenders
                .iter()
                .map(|t| payment_label(t.kind).to_owned())
                .fold(Vec::new(), |mut v, label| {
                    if !v.contains(&label) {
                        v.push(label);
                    }
                    v
                }),
            omitted_lines: omitted,
        };
        let document = pos_receipts::tax_invoice(
            &crate::checkout::receipt_context(&info),
            Meta {
                receipt_number: pos_receipts::document::PENDING_NUMBER.into(),
                local_time: pos_domain::time::local_date_time(ctx.now(), &timezone).unwrap_or_default(),
                register_name: info.register_name.clone(),
                register_code: info.register_code.clone(),
                cashier_name: info.cashier_name.clone(),
            },
            detail.mode == SaleMode::Sandbox,
            exponent,
            section,
        );
        let document = serde_json::to_value(&document).map_err(|e| AppError::Internal(e.to_string()))?;

        let ids = ctx.ids.as_ref();
        let id = TaxInvoiceId::generate(ids);
        let entry =
            AuditEntry::new(ids, audit::TAX_INVOICE_ISSUED, "sale", sale).with_metadata(serde_json::json!({
                "taxInvoice": id,
                "totalMinor": total.minor().to_string(),
                "vatMinor": detail.tax_total.minor().to_string(),
            }));
        let set = WriteSet::builder(ctx.write_context())
            .insert_tax_invoice(
                TaxInvoiceRecord {
                    id,
                    sale_id: sale,
                    number_prefix: prefix,
                    purchaser_tin,
                    purchaser_name,
                    purchaser_address,
                    issued_by: actor.user_id,
                    issued_at: ctx.now(),
                },
                NUMBER,
            )
            .insert_receipt(
                pos_domain::ReceiptId::generate(ids),
                ("tax_invoice", sale.to_string()),
                pos_receipts::CONTENT_VERSION,
                document,
                Some(NUMBER),
            )
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        self.get(ctx, sale, None)
            .await?
            .ok_or_else(|| AppError::Internal("the tax invoice was not stored".into()))
    }

    /// The sale's tax invoice as stored, rendered for 58 or 80 mm paper; `None` if none was
    /// issued.
    pub async fn get(
        &self,
        ctx: &CommandContext,
        sale: SaleId,
        width_mm: Option<u32>,
    ) -> Result<Option<RenderedReceipt>, AppError> {
        ctx.authorize(Permission::SalesView)?;
        let Some((_, json)) = self.receipts.tax_invoice_receipt(sale).await? else {
            return Ok(None);
        };
        let reader = ReceiptService {
            receipts: self.receipts.clone(),
        };
        Ok(Some(reader.rendered(ctx, &json, width_mm).await?))
    }
}

fn payment_label(kind: pos_domain::settlement::TenderKind) -> &'static str {
    use pos_domain::settlement::TenderKind;
    match kind {
        TenderKind::Cash => "Cash",
        TenderKind::CardExternal => "Card",
        TenderKind::BankTransferExternal => "Bank transfer",
        TenderKind::QrExternal => "Mobile payment",
        TenderKind::OtherExternal => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tin_is_nine_digits() {
        assert_eq!(normalize_tin("104567890").as_deref(), Some("104567890"));
        assert_eq!(normalize_tin(" 104-567 890 ").as_deref(), Some("104567890"));
        for bad in ["", "10456789", "1045678901", "10456789O", "GB123456789"] {
            assert_eq!(normalize_tin(bad), None, "{bad}");
        }
    }

    #[test]
    fn dates_are_month_first_and_the_number_starts_yymmm() {
        assert_eq!(us_date("2026-07-03").as_deref(), Some("07/03/2026"));
        assert_eq!(number_prefix("2026-07-03").as_deref(), Some("26JUL_"));
        assert_eq!(number_prefix("2027-12-31").as_deref(), Some("27DEC_"));
        assert_eq!(us_date("20260703"), None);
    }

    #[test]
    fn unit_price_is_the_amount_per_unit_rounded() {
        assert_eq!(unit_price(Money::from_minor(1_850_000), 10_000).minor(), 185_000);
        // 10.00 for 3 is 3.33; 2.5 kg for 7.50 is 3.00.
        assert_eq!(unit_price(Money::from_minor(1_000), 3_000).minor(), 333);
        assert_eq!(unit_price(Money::from_minor(750), 2_500).minor(), 300);
    }
}
