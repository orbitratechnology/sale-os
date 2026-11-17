//! Report queries (E14-T01/T02). Definitions: docs/reporting-definitions.md. Every query
//! filters on `business_date` (indexed) and `mode = 'live'`.

use crate::SqliteStore;
use pos_application::queries::QueryError;
use pos_application::reports::{
    DayFigures, ReportQueries, SalesFigures, StockRow, StockStatus, TenderMixRow, TopBy, TopProductRow,
};
use pos_domain::catalog::Unit;
use pos_domain::settlement::TenderKind;
use pos_domain::time::BusinessDate;
use pos_domain::{Money, Quantity, Timestamp};
use std::collections::BTreeMap;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

fn count(v: i64) -> Result<u32, QueryError> {
    u32::try_from(v).map_err(q)
}

#[async_trait::async_trait]
impl ReportQueries for SqliteStore {
    async fn daily_figures(
        &self,
        from: BusinessDate,
        to: BusinessDate,
    ) -> Result<Vec<DayFigures>, QueryError> {
        let r = self.db().readers();
        let sales: Vec<(String, i64, i64, i64, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT business_date, count(*), sum(subtotal_minor), sum(line_discount_total_minor),
               sum(order_discount_total_minor), sum(tax_total_minor), sum(rounding_minor), sum(total_minor)
             FROM sales WHERE mode = 'live' AND business_date BETWEEN ? AND ?
             GROUP BY business_date",
        )
        .bind(from.to_string())
        .bind(to.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?;
        // Returns count on their own business date; a return of a sandbox sale is sandbox too.
        let returns: Vec<(String, i64, i64, i64)> = sqlx::query_as(
            "SELECT r.business_date, count(*),
               coalesce(sum((SELECT sum(f.amount_minor) FROM refunds f WHERE f.return_id = r.return_id)), 0),
               sum(r.tax_minor)
             FROM returns r JOIN sales s ON s.sale_id = r.original_sale_id
             WHERE s.mode = 'live' AND r.business_date BETWEEN ? AND ?
             GROUP BY r.business_date",
        )
        .bind(from.to_string())
        .bind(to.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?;
        let mut days: BTreeMap<String, SalesFigures> = BTreeMap::new();
        for (date, n, gross, line, order, tax, rounding, total) in sales {
            let f = days.entry(date).or_default();
            f.sales_count = count(n)?;
            f.gross = Money::from_minor(gross);
            f.line_discounts = Money::from_minor(line);
            f.order_discounts = Money::from_minor(order);
            f.tax = Money::from_minor(tax);
            f.rounding = Money::from_minor(rounding);
            f.sales_total = Money::from_minor(total);
        }
        for (date, n, refunds, tax) in returns {
            let f = days.entry(date).or_default();
            f.returns_count = count(n)?;
            f.refunds = Money::from_minor(refunds);
            f.refunded_tax = Money::from_minor(tax);
        }
        days.into_iter()
            .map(|(date, figures)| {
                Ok(DayFigures {
                    date: date.parse().map_err(q)?,
                    figures,
                })
            })
            .collect()
    }

    async fn tender_mix(
        &self,
        from: BusinessDate,
        to: BusinessDate,
    ) -> Result<Vec<TenderMixRow>, QueryError> {
        let r = self.db().readers();
        let taken: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT t.tender_type, t.confirmation_source, count(*), sum(t.amount_minor)
             FROM tenders t JOIN sales s ON s.sale_id = t.sale_id
             WHERE s.mode = 'live' AND s.business_date BETWEEN ? AND ? AND t.status = 'confirmed'
             GROUP BY t.tender_type, t.confirmation_source",
        )
        .bind(from.to_string())
        .bind(to.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?;
        let refunded: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT f.tender_type, f.confirmation_source, count(*), sum(f.amount_minor)
             FROM refunds f JOIN returns x ON x.return_id = f.return_id JOIN sales s ON s.sale_id = x.original_sale_id
             WHERE s.mode = 'live' AND x.business_date BETWEEN ? AND ? AND f.status = 'confirmed'
             GROUP BY f.tender_type, f.confirmation_source",
        )
        .bind(from.to_string())
        .bind(to.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?;
        let mut rows: BTreeMap<(String, String), TenderMixRow> = BTreeMap::new();
        let mut row = |kind: &str, source: &str| -> Result<(String, String), QueryError> {
            let key = (kind.to_owned(), source.to_owned());
            if !rows.contains_key(&key) {
                rows.insert(
                    key.clone(),
                    TenderMixRow {
                        kind: TenderKind::from_code(kind)
                            .ok_or_else(|| QueryError(format!("unknown tender {kind:?}")))?,
                        confirmation_source: source.to_owned(),
                        count: 0,
                        taken: Money::ZERO,
                        refund_count: 0,
                        refunded: Money::ZERO,
                    },
                );
            }
            Ok(key)
        };
        let mut keys = Vec::new();
        for (kind, source, n, amount) in taken {
            keys.push((row(&kind, &source)?, n, amount, false));
        }
        for (kind, source, n, amount) in refunded {
            keys.push((row(&kind, &source)?, n, amount, true));
        }
        for (key, n, amount, refund) in keys {
            let r = rows.get_mut(&key).expect("inserted above");
            if refund {
                r.refund_count = count(n)?;
                r.refunded = Money::from_minor(amount);
            } else {
                r.count = count(n)?;
                r.taken = Money::from_minor(amount);
            }
        }
        Ok(rows.into_values().collect())
    }

    async fn top_products(
        &self,
        from: BusinessDate,
        to: BusinessDate,
        by: TopBy,
        limit: u32,
    ) -> Result<Vec<TopProductRow>, QueryError> {
        let order = match by {
            TopBy::Quantity => "qty DESC, value DESC",
            TopBy::Value => "value DESC, qty DESC",
        };
        let rows: Vec<(String, String, String, i64, i64)> = sqlx::query_as(&format!(
            "WITH sold AS (
               SELECT l.variant_id, l.quantity_milli AS qty, l.total_minor AS value, s.occurred_at AS at,
                      l.name_snapshot AS name, l.variant_label_snapshot AS label
               FROM sales s JOIN sale_lines l ON l.sale_id = s.sale_id
               WHERE s.mode = 'live' AND s.business_date BETWEEN ?1 AND ?2),
             back AS (
               SELECT l.variant_id, -rl.quantity_milli AS qty, -rl.refund_minor AS value, NULL AS at,
                      l.name_snapshot AS name, l.variant_label_snapshot AS label
               FROM returns r JOIN return_lines rl ON rl.return_id = r.return_id
               JOIN sale_lines l ON l.sale_line_id = rl.original_sale_line_id
               JOIN sales s ON s.sale_id = r.original_sale_id
               WHERE s.mode = 'live' AND r.business_date BETWEEN ?1 AND ?2),
             movement AS (SELECT * FROM sold UNION ALL SELECT * FROM back),
             named AS (
               SELECT variant_id, name, label, row_number() OVER (PARTITION BY variant_id ORDER BY at DESC NULLS LAST) AS n
               FROM movement)
             SELECT m.variant_id, (SELECT name FROM named WHERE named.variant_id = m.variant_id AND n = 1),
                    (SELECT label FROM named WHERE named.variant_id = m.variant_id AND n = 1),
                    sum(m.qty) AS qty, sum(m.value) AS value
             FROM movement m GROUP BY m.variant_id
             ORDER BY {order}, 2 LIMIT ?3"
        ))
        .bind(from.to_string())
        .bind(to.to_string())
        .bind(i64::from(limit))
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter()
            .map(|(v, name, label, qty, value)| {
                Ok(TopProductRow {
                    variant_id: v.parse().map_err(q)?,
                    name,
                    label,
                    quantity: Quantity::from_milli(qty),
                    value: Money::from_minor(value),
                })
            })
            .collect()
    }

    async fn stock_rows(&self) -> Result<Vec<StockRow>, QueryError> {
        type Row = (
            String,
            String,
            String,
            Option<String>,
            String,
            i64,
            i64,
            Option<i64>,
            Option<i64>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT v.variant_id, p.name, v.label, v.sku, p.unit_code,
               coalesce((SELECT sum(on_hand_milli) FROM stock_balances b WHERE b.variant_id = v.variant_id AND b.bucket = 'sellable'), 0),
               coalesce((SELECT sum(on_hand_milli) FROM stock_balances b WHERE b.variant_id = v.variant_id AND b.bucket = 'damaged'), 0),
               v.low_stock_threshold_milli,
               (SELECT max(last_movement_at) FROM stock_balances b WHERE b.variant_id = v.variant_id)
             FROM product_variants v JOIN products p ON p.product_id = v.product_id
             WHERE v.track_stock = 1 AND v.status = 'active' AND p.status = 'active'
             ORDER BY p.name COLLATE NOCASE, v.label",
        )
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter()
            .map(
                |(v, name, label, sku, unit, sellable, damaged, threshold, last)| {
                    let sellable = Quantity::from_milli(sellable);
                    let threshold = threshold.map(Quantity::from_milli);
                    Ok(StockRow {
                        variant_id: v.parse().map_err(q)?,
                        name,
                        label,
                        sku,
                        unit: Unit::from_code(&unit)
                            .ok_or_else(|| QueryError(format!("unknown unit {unit:?}")))?,
                        sellable,
                        damaged: Quantity::from_milli(damaged),
                        low_stock_threshold: threshold,
                        status: StockStatus::of(sellable, threshold),
                        last_movement_at: last.map(Timestamp::from_millis),
                    })
                },
            )
            .collect()
    }

    async fn report_settings(&self) -> Result<(bool, u8), QueryError> {
        let (inclusive, exponent): (bool, i64) =
            sqlx::query_as("SELECT prices_include_tax, currency_exponent FROM businesses LIMIT 1")
                .fetch_one(self.db().readers())
                .await
                .map_err(q)?;
        Ok((inclusive, u8::try_from(exponent).map_err(q)?))
    }
}
