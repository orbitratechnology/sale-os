//! Sales history and sale detail reads (E12-T03).
//!
//! The list is built with only the conditions that are set, so SQLite can use
//! `ix_sales_history` (or `ix_sales_cashier_history`) as a range scan in sort order and stop
//! after one page. Per-row figures (items, tenders, refunds) are looked up for the page's
//! rows only.

use crate::SqliteStore;
use pos_application::queries::{QueryError, SalesFilter, SalesQueries};
use pos_application::records::{
    SaleDetail, SaleDetailLine, SaleReturnRow, SaleRow, SaleTaxRow, SaleTenderRow, SalesCursor, SalesPage,
};
use pos_domain::catalog::Unit;
use pos_domain::sale::SaleMode;
use pos_domain::settlement::TenderKind;
use pos_domain::{Currency, CurrencyCode, Money, Quantity, SaleId, Timestamp};
use sqlx::sqlite::SqliteArguments;
use sqlx::{Arguments, Row};

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

fn mode(s: &str) -> SaleMode {
    if s == "live" {
        SaleMode::Live
    } else {
        SaleMode::Sandbox
    }
}

fn kinds(list: Option<String>) -> Result<Vec<TenderKind>, QueryError> {
    let mut out: Vec<TenderKind> = Vec::new();
    for code in list.unwrap_or_default().split(',').filter(|c| !c.is_empty()) {
        let k = TenderKind::from_code(code).ok_or_else(|| QueryError(format!("unknown tender {code:?}")))?;
        if !out.contains(&k) {
            out.push(k);
        }
    }
    Ok(out)
}

fn local(at: i64, zone: &str) -> String {
    pos_domain::time::local_date_time(Timestamp::from_millis(at), zone).unwrap_or_default()
}

#[derive(serde::Deserialize)]
struct StoredTax {
    name: String,
    rate: u32,
    taxable: i64,
    tax: i64,
}

#[async_trait::async_trait]
impl SalesQueries for SqliteStore {
    async fn list_sales(
        &self,
        filter: &SalesFilter,
        after: Option<&SalesCursor>,
        limit: u32,
    ) -> Result<SalesPage, QueryError> {
        let mut sql = String::from(
            "SELECT s.sale_id, s.receipt_number, s.occurred_at, s.timezone, s.business_date,
               coalesce(u.display_name, ''), s.mode, s.currency_exponent, s.total_minor,
               (SELECT coalesce(sum(l.quantity_milli), 0) FROM sale_lines l WHERE l.sale_id = s.sale_id),
               (SELECT group_concat(tender_type, ',') FROM (SELECT t.tender_type FROM tenders t WHERE t.sale_id = s.sale_id ORDER BY t.seq)),
               (SELECT coalesce(sum(r.total_minor), 0) FROM returns r WHERE r.original_sale_id = s.sale_id)
             FROM sales s LEFT JOIN staff_users u ON u.user_id = s.cashier_user_id
             WHERE 1 = 1",
        );
        let mut args = SqliteArguments::default();
        let bind = |args: &mut SqliteArguments<'_>, v: String| args.add(v).map_err(q);
        if let Some(c) = filter.cashier {
            sql.push_str(" AND s.cashier_user_id = ?");
            bind(&mut args, c.to_string())?;
        }
        if let Some(from) = filter.from {
            sql.push_str(" AND s.business_date >= ?");
            bind(&mut args, from.to_string())?;
        }
        if let Some(to) = filter.to {
            sql.push_str(" AND s.business_date <= ?");
            bind(&mut args, to.to_string())?;
        }
        if let Some(text) = filter.receipt.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            if text.chars().all(|c| c.is_ascii_digit()) {
                // Just the digits: the sequence part of the number, on any register.
                sql.push_str(" AND s.receipt_seq = ?");
                args.add(text.parse::<i64>().map_err(q)?).map_err(q)?;
            } else {
                sql.push_str(" AND s.receipt_number = ?");
                bind(&mut args, text.to_uppercase())?;
            }
        }
        if let Some(t) = filter.tender {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM tenders t WHERE t.sale_id = s.sale_id AND t.tender_type = ?)",
            );
            bind(&mut args, t.as_str().to_owned())?;
        }
        if let Some(c) = after {
            sql.push_str(" AND (s.business_date, s.occurred_at, s.sale_id) < (?, ?, ?)");
            bind(&mut args, c.business_date.clone())?;
            args.add(c.occurred_at).map_err(q)?;
            bind(&mut args, c.sale_id.to_string())?;
        }
        sql.push_str(" ORDER BY s.business_date DESC, s.occurred_at DESC, s.sale_id DESC LIMIT ?");
        // One extra row tells whether there is a next page.
        args.add(i64::from(limit) + 1).map_err(q)?;

        let rows = sqlx::query_with(&sql, args)
            .fetch_all(self.db().readers())
            .await
            .map_err(q)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let at: i64 = r.try_get(2).map_err(q)?;
            let zone: String = r.try_get(3).map_err(q)?;
            out.push(SaleRow {
                sale_id: r.try_get::<String, _>(0).map_err(q)?.parse().map_err(q)?,
                receipt_number: r.try_get(1).map_err(q)?,
                occurred_at: Timestamp::from_millis(at),
                local_time: local(at, &zone),
                business_date: r.try_get(4).map_err(q)?,
                cashier_name: r.try_get(5).map_err(q)?,
                mode: mode(&r.try_get::<String, _>(6).map_err(q)?),
                currency_exponent: u8::try_from(r.try_get::<i64, _>(7).map_err(q)?).map_err(q)?,
                total: Money::from_minor(r.try_get(8).map_err(q)?),
                item_count: Quantity::from_milli(r.try_get(9).map_err(q)?),
                tenders: kinds(r.try_get(10).map_err(q)?)?,
                refunded: Money::from_minor(r.try_get(11).map_err(q)?),
            });
        }
        let more = out.len() > limit as usize;
        out.truncate(limit as usize);
        let next = if more {
            out.last().map(|last| SalesCursor {
                business_date: last.business_date.clone(),
                occurred_at: last.occurred_at.as_millis(),
                sale_id: last.sale_id,
            })
        } else {
            None
        };
        Ok(SalesPage { rows: out, next })
    }

    async fn sale_detail(&self, sale: SaleId) -> Result<Option<SaleDetail>, QueryError> {
        let r = self.db().readers();
        let head = sqlx::query(
            "SELECT s.receipt_number, s.occurred_at, s.timezone, s.business_date, coalesce(u.display_name, ''),
               a.display_name, s.mode, s.currency_code, s.currency_exponent, s.prices_include_tax,
               s.subtotal_minor, s.line_discount_total_minor, s.order_discount_total_minor, s.tax_total_minor,
               s.rounding_minor, s.total_minor, s.tendered_total_minor, s.change_minor, s.tax_breakdown_json
             FROM sales s
             LEFT JOIN staff_users u ON u.user_id = s.cashier_user_id
             LEFT JOIN staff_users a ON a.user_id = s.approver_user_id
             WHERE s.sale_id = ?",
        )
        .bind(sale.to_string())
        .fetch_optional(r)
        .await
        .map_err(q)?;
        let Some(h) = head else { return Ok(None) };
        let m = |i: usize| -> Result<Money, QueryError> { Ok(Money::from_minor(h.try_get(i).map_err(q)?)) };
        let tax: Vec<StoredTax> = serde_json::from_str(&h.try_get::<String, _>(18).map_err(q)?).map_err(q)?;

        let lines = sqlx::query(
            "SELECT l.line_no, l.name_snapshot, l.variant_label_snapshot, l.sku_snapshot, l.unit_code, l.quantity_milli,
               l.unit_price_minor, l.price_overridden, l.gross_minor, l.line_discount_minor + l.order_discount_alloc_minor,
               l.tax_minor, l.total_minor,
               coalesce((SELECT sum(rl.quantity_milli) FROM return_lines rl WHERE rl.original_sale_line_id = l.sale_line_id), 0)
             FROM sale_lines l WHERE l.sale_id = ? ORDER BY l.line_no",
        )
        .bind(sale.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?
        .iter()
        .map(|l| {
            let unit: String = l.try_get(4).map_err(q)?;
            Ok(SaleDetailLine {
                line_no: u32::try_from(l.try_get::<i64, _>(0).map_err(q)?).map_err(q)?,
                name: l.try_get(1).map_err(q)?,
                variant_label: l.try_get(2).map_err(q)?,
                sku: l.try_get(3).map_err(q)?,
                unit: Unit::from_code(&unit).ok_or_else(|| QueryError(format!("unknown unit {unit:?}")))?,
                quantity: Quantity::from_milli(l.try_get(5).map_err(q)?),
                unit_price: Money::from_minor(l.try_get(6).map_err(q)?),
                price_overridden: l.try_get(7).map_err(q)?,
                gross: Money::from_minor(l.try_get(8).map_err(q)?),
                discount: Money::from_minor(l.try_get(9).map_err(q)?),
                tax: Money::from_minor(l.try_get(10).map_err(q)?),
                total: Money::from_minor(l.try_get(11).map_err(q)?),
                returned: Quantity::from_milli(l.try_get(12).map_err(q)?),
            })
        })
        .collect::<Result<Vec<_>, QueryError>>()?;

        let tenders = sqlx::query(
            "SELECT tender_type, amount_minor, received_minor, change_minor, status, confirmation_source, external_reference
             FROM tenders WHERE sale_id = ? ORDER BY seq",
        )
        .bind(sale.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?
        .iter()
        .map(|t| {
            let kind: String = t.try_get(0).map_err(q)?;
            Ok(SaleTenderRow {
                kind: TenderKind::from_code(&kind).ok_or_else(|| QueryError(format!("unknown tender {kind:?}")))?,
                amount: Money::from_minor(t.try_get(1).map_err(q)?),
                received: t.try_get::<Option<i64>, _>(2).map_err(q)?.map(Money::from_minor),
                change: Money::from_minor(t.try_get(3).map_err(q)?),
                status: t.try_get(4).map_err(q)?,
                confirmation_source: t.try_get(5).map_err(q)?,
                reference: t.try_get(6).map_err(q)?,
            })
        })
        .collect::<Result<Vec<_>, QueryError>>()?;

        let returns = sqlx::query(
            "SELECT r.return_id, r.return_number, r.occurred_at, s.timezone, r.reason_code, r.total_minor,
               (SELECT group_concat(tender_type, ',') FROM (SELECT f.tender_type FROM refunds f WHERE f.return_id = r.return_id ORDER BY f.seq))
             FROM returns r JOIN sales s ON s.sale_id = r.original_sale_id
             WHERE r.original_sale_id = ? ORDER BY r.return_seq",
        )
        .bind(sale.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?
        .iter()
        .map(|x| {
            Ok(SaleReturnRow {
                return_id: x.try_get::<String, _>(0).map_err(q)?.parse().map_err(q)?,
                return_number: x.try_get(1).map_err(q)?,
                local_time: local(x.try_get(2).map_err(q)?, &x.try_get::<String, _>(3).map_err(q)?),
                reason: x.try_get(4).map_err(q)?,
                total: Money::from_minor(x.try_get(5).map_err(q)?),
                refunds: kinds(x.try_get(6).map_err(q)?)?,
            })
        })
        .collect::<Result<Vec<_>, QueryError>>()?;

        let at: i64 = h.try_get(1).map_err(q)?;
        let zone: String = h.try_get(2).map_err(q)?;
        Ok(Some(SaleDetail {
            sale_id: sale,
            receipt_number: h.try_get(0).map_err(q)?,
            local_time: local(at, &zone),
            business_date: h.try_get(3).map_err(q)?,
            cashier_name: h.try_get(4).map_err(q)?,
            approver_name: h.try_get(5).map_err(q)?,
            mode: mode(&h.try_get::<String, _>(6).map_err(q)?),
            currency: Currency::new(
                CurrencyCode::new(&h.try_get::<String, _>(7).map_err(q)?).map_err(q)?,
                u8::try_from(h.try_get::<i64, _>(8).map_err(q)?).map_err(q)?,
            )
            .map_err(q)?,
            prices_include_tax: h.try_get(9).map_err(q)?,
            subtotal: m(10)?,
            line_discounts: m(11)?,
            order_discount: m(12)?,
            tax_total: m(13)?,
            rounding: m(14)?,
            total: m(15)?,
            tendered: m(16)?,
            change: m(17)?,
            tax_breakdown: tax
                .into_iter()
                .map(|t| SaleTaxRow {
                    name: t.name,
                    rate_ppm: t.rate,
                    taxable: Money::from_minor(t.taxable),
                    tax: Money::from_minor(t.tax),
                })
                .collect(),
            lines,
            tenders,
            returns,
        }))
    }
}
