//! Catalogue reads (E06-T04): full aggregates, owner checks, code lookup and FTS search.

use crate::SqliteStore;
use pos_application::queries::{
    CatalogHit, CatalogQueries, MatchedBy, ProductFilter, ProductPage, ProductRow, ProductStatusFilter,
    QueryError, TaxCategorySummary,
};
use pos_domain::catalog::{
    Barcode, BarcodeQualifier, Product, ProductType, Symbology, Unit, Variant, normalize_barcode,
};
use pos_domain::{Money, ProductId, Quantity};
use std::collections::BTreeMap;

/// `FROM` an active, stock-tracked variant `lv` of product `p`, with its sellable balance `lb`
/// (no balance row yet means nothing on hand).
macro_rules! tracked_variants_with_stock {
    () => {
        "FROM product_variants lv LEFT JOIN stock_balances lb
           ON lb.variant_id = lv.variant_id AND lb.bucket = 'sellable'
         WHERE lv.product_id = p.product_id AND lv.status = 'active' AND lv.track_stock = 1"
    };
}
macro_rules! sellable {
    () => {
        "coalesce(lb.on_hand_milli, 0)"
    };
}
/// The stock report's rule (`StockStatus::of`): low when at or below a set threshold.
macro_rules! at_or_below_threshold {
    () => {
        "(lv.low_stock_threshold_milli IS NOT NULL AND coalesce(lb.on_hand_milli, 0) <= lv.low_stock_threshold_milli)"
    };
}

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

fn corrupt(what: &str, value: &str) -> QueryError {
    QueryError(format!("unexpected {what} {value:?} in catalogue"))
}

type HitRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    String,
    bool,
    i64,
);

const HIT_COLUMNS: &str = "p.product_id, v.variant_id, p.name, v.label, v.sku, v.price_minor, p.unit_code, v.track_stock, v.scan_qty_milli";

fn hit(r: HitRow, matched_by: MatchedBy, scan_qty: Option<i64>) -> Result<CatalogHit, QueryError> {
    Ok(CatalogHit {
        product_id: r.0.parse().map_err(q)?,
        variant_id: r.1.parse().map_err(q)?,
        name: r.2,
        label: r.3,
        sku: r.4,
        price: Money::from_minor(r.5),
        unit: Unit::from_code(&r.6).ok_or_else(|| corrupt("unit", &r.6))?,
        track_stock: r.7,
        scan_qty: Quantity::from_milli(scan_qty.unwrap_or(r.8)),
        matched_by,
    })
}

/// FTS5 query from free text: each alphanumeric token becomes a prefix term, ANDed.
/// Anything else is dropped, so user input can never inject FTS syntax.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(8)
        .map(|t| format!("\"{}\"*", t.to_lowercase()))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[async_trait::async_trait]
impl CatalogQueries for SqliteStore {
    async fn product(&self, id: ProductId) -> Result<Option<Product>, QueryError> {
        let r = self.db().readers();
        let row: Option<(String, Option<String>, Option<String>, String, String, String, bool, String, i64)> = sqlx::query_as(
            "SELECT name, description, category_id, product_type, unit_code, tax_category_id, quick_select, status, version
             FROM products WHERE product_id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(r)
        .await
        .map_err(q)?;
        let Some((name, description, category, ptype, unit, tax, quick, status, version)) = row else {
            return Ok(None);
        };
        type VariantRow = (
            String,
            bool,
            Option<String>,
            String,
            i64,
            Option<i64>,
            bool,
            Option<i64>,
            i64,
            String,
        );
        let vrows: Vec<VariantRow> = sqlx::query_as(
            "SELECT variant_id, is_default, sku, options_json, price_minor, cost_minor, track_stock,
                    low_stock_threshold_milli, scan_qty_milli, status
             FROM product_variants WHERE product_id = ? ORDER BY is_default DESC, sort_order",
        )
        .bind(id.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?;
        let brows: Vec<(String, String, String, String, String, String, i64, String)> = sqlx::query_as(
            "SELECT b.barcode_id, b.variant_id, b.value_normalized, b.value_raw, b.symbology, b.qualifier, b.pack_qty_milli, b.status
             FROM product_barcodes b JOIN product_variants v USING (variant_id) WHERE v.product_id = ? ORDER BY b.created_at, b.barcode_id",
        )
        .bind(id.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?;
        let mut variants = Vec::with_capacity(vrows.len());
        for (vid, is_default, sku, options, price, cost, track, low, scan, vstatus) in vrows {
            let options: BTreeMap<String, String> = serde_json::from_str(&options).map_err(q)?;
            let barcodes = brows
                .iter()
                .filter(|b| b.1 == vid && b.7 == "active")
                .map(|b| {
                    Ok(Barcode {
                        id: b.0.parse().map_err(q)?,
                        value: b.2.clone(),
                        raw: b.3.clone(),
                        symbology: Symbology::from_code(&b.4).ok_or_else(|| corrupt("symbology", &b.4))?,
                        qualifier: BarcodeQualifier::from_code(&b.5)
                            .ok_or_else(|| corrupt("qualifier", &b.5))?,
                        pack_qty: Quantity::from_milli(b.6),
                    })
                })
                .collect::<Result<Vec<_>, QueryError>>()?;
            variants.push(Variant {
                id: vid.parse().map_err(q)?,
                is_default,
                sku,
                options,
                price: Money::from_minor(price),
                cost: cost.map(Money::from_minor),
                track_stock: track,
                low_stock_threshold: low.map(Quantity::from_milli),
                scan_qty: Quantity::from_milli(scan),
                barcodes,
                archived: vstatus != "active",
            });
        }
        Ok(Some(Product {
            id,
            name,
            description,
            category_id: category.map(|c| c.parse()).transpose().map_err(q)?,
            product_type: ProductType::from_code(&ptype).ok_or_else(|| corrupt("product type", &ptype))?,
            unit: Unit::from_code(&unit).ok_or_else(|| corrupt("unit", &unit))?,
            tax_category_id: tax.parse().map_err(q)?,
            quick_select: quick,
            archived: status != "active",
            variants,
            version: u64::try_from(version).map_err(q)?,
        }))
    }

    async fn barcode_owner(&self, value: &str) -> Result<Option<ProductId>, QueryError> {
        let row: Option<String> = sqlx::query_scalar(
            "SELECT v.product_id FROM product_barcodes b JOIN product_variants v USING (variant_id)
             WHERE b.value_normalized = ? AND b.status = 'active'",
        )
        .bind(value)
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        row.map(|s| s.parse().map_err(q)).transpose()
    }

    async fn sku_owner(&self, sku: &str) -> Result<Option<ProductId>, QueryError> {
        let row: Option<String> = sqlx::query_scalar(
            "SELECT product_id FROM product_variants WHERE sku = ? COLLATE NOCASE AND status = 'active'",
        )
        .bind(sku.trim())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        row.map(|s| s.parse().map_err(q)).transpose()
    }

    async fn lookup_code(&self, raw: &str) -> Result<Option<CatalogHit>, QueryError> {
        let r = self.db().readers();
        if let Ok(n) = normalize_barcode(raw, None) {
            let row: Option<(String, String, String, String, Option<String>, i64, String, bool, i64, String, i64)> =
                sqlx::query_as(&format!(
                    "SELECT {HIT_COLUMNS}, b.qualifier, b.pack_qty_milli
                     FROM product_barcodes b JOIN product_variants v USING (variant_id) JOIN products p USING (product_id)
                     WHERE b.value_normalized = ? AND b.status = 'active' AND v.status = 'active' AND p.status = 'active'"
                ))
                .bind(&n.value)
                .fetch_optional(r)
                .await
                .map_err(q)?;
            if let Some(row) = row {
                let pack = (row.9 == "pack").then_some(row.10);
                let base = (row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8);
                return hit(base, MatchedBy::Barcode, pack).map(Some);
            }
        }
        let row: Option<HitRow> = sqlx::query_as(&format!(
            "SELECT {HIT_COLUMNS} FROM product_variants v JOIN products p USING (product_id)
             WHERE v.sku = ? COLLATE NOCASE AND v.status = 'active' AND p.status = 'active'"
        ))
        .bind(raw.trim())
        .fetch_optional(r)
        .await
        .map_err(q)?;
        row.map(|row| hit(row, MatchedBy::Sku, None)).transpose()
    }

    async fn search(&self, query: &str, limit: u32) -> Result<Vec<CatalogHit>, QueryError> {
        let Some(fts) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let rows: Vec<HitRow> = sqlx::query_as(&format!(
            "SELECT {HIT_COLUMNS} FROM product_search s
             JOIN product_variants v ON v.variant_id = s.variant_id JOIN products p USING (product_id)
             WHERE product_search MATCH ? AND v.status = 'active' AND p.status = 'active'
             ORDER BY (lower(v.sku) = lower(?)) DESC, bm25(product_search) LIMIT ?"
        ))
        .bind(&fts)
        .bind(query.trim())
        .bind(i64::from(limit.clamp(1, 200)))
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter()
            .map(|row| hit(row, MatchedBy::Search, None))
            .collect()
    }

    async fn quick_select(
        &self,
        category: Option<pos_domain::CategoryId>,
        offset: u32,
        limit: u32,
    ) -> Result<pos_application::queries::QuickSelectPage, QueryError> {
        use pos_application::queries::{QuickProduct, QuickSelectPage};
        let r = self.db().readers();
        let categories: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT c.category_id, c.name, c.sort_order FROM products p
               JOIN categories c ON c.category_id = p.category_id AND c.status = 'active'
              WHERE p.quick_select = 1 AND p.status = 'active'
              ORDER BY c.sort_order, c.name COLLATE NOCASE",
        )
        .fetch_all(r)
        .await
        .map_err(q)
        .map(|rows: Vec<(String, String, i64)>| rows.into_iter().map(|(id, n, _)| (id, n)).collect())?;
        let cat = category.map(|c| c.to_string());
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM products p WHERE p.quick_select = 1 AND p.status = 'active'
               AND (?1 IS NULL OR p.category_id = ?1)
               AND EXISTS (SELECT 1 FROM product_variants v WHERE v.product_id = p.product_id AND v.status = 'active')",
        )
        .bind(&cat)
        .fetch_one(r)
        .await
        .map_err(q)?;
        let rows: Vec<HitRow> = sqlx::query_as(&format!(
            "WITH page AS (
               SELECT p.product_id, p.name FROM products p
                WHERE p.quick_select = 1 AND p.status = 'active' AND (?1 IS NULL OR p.category_id = ?1)
                  AND EXISTS (SELECT 1 FROM product_variants v WHERE v.product_id = p.product_id AND v.status = 'active')
                ORDER BY p.name COLLATE NOCASE, p.product_id LIMIT ?2 OFFSET ?3)
             SELECT {HIT_COLUMNS} FROM page JOIN products p USING (product_id)
               JOIN product_variants v ON v.product_id = p.product_id AND v.status = 'active'
              ORDER BY p.name COLLATE NOCASE, p.product_id, v.sort_order, v.variant_id"
        ))
        .bind(&cat)
        .bind(i64::from(limit))
        .bind(i64::from(offset))
        .fetch_all(r)
        .await
        .map_err(q)?;
        let mut products: Vec<QuickProduct> = Vec::new();
        for row in rows {
            let h = hit(row, MatchedBy::Search, None)?;
            match products.last_mut() {
                Some(last) if last.product_id == h.product_id => last.variants.push(h),
                _ => products.push(QuickProduct {
                    product_id: h.product_id,
                    name: h.name.clone(),
                    variants: vec![h],
                }),
            }
        }
        Ok(QuickSelectPage {
            categories: categories
                .into_iter()
                .map(|(id, name)| Ok((id.parse().map_err(q)?, name)))
                .collect::<Result<_, QueryError>>()?,
            total: u32::try_from(total).unwrap_or(u32::MAX),
            products,
        })
    }

    async fn tax_categories(&self) -> Result<Vec<TaxCategorySummary>, QueryError> {
        let rows: Vec<(String, String, bool, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT c.tax_category_id, c.name, c.is_default, r.name, r.rate_ppm
             FROM tax_categories c
             LEFT JOIN tax_category_rates cr ON cr.tax_category_id = c.tax_category_id
             LEFT JOIN tax_rates r ON r.tax_rate_id = cr.tax_rate_id AND r.status = 'active'
             WHERE c.status = 'active'
             ORDER BY c.is_default DESC, c.name COLLATE NOCASE, c.tax_category_id, r.name",
        )
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        let mut out: Vec<TaxCategorySummary> = Vec::new();
        for (id, name, is_default, rate_name, rate) in rows {
            let id: pos_domain::TaxCategoryId = id.parse().map_err(q)?;
            if out.last().is_none_or(|c| c.tax_category_id != id) {
                out.push(TaxCategorySummary {
                    tax_category_id: id,
                    name,
                    is_default,
                    rates: Vec::new(),
                });
            }
            if let (Some(n), Some(r), Some(cat)) = (rate_name, rate, out.last_mut()) {
                let rate = pos_domain::RatePpm::new(u32::try_from(r).map_err(q)?).map_err(q)?;
                cat.rates.push((n, rate));
            }
        }
        Ok(out)
    }

    async fn list_products(
        &self,
        filter: &ProductFilter,
        offset: u32,
        limit: u32,
    ) -> Result<ProductPage, QueryError> {
        // Shared WHERE clause; every value is bound, never spliced in.
        fn filtered<'a>(
            b: &mut sqlx::QueryBuilder<'a, sqlx::Sqlite>,
            f: &'a ProductFilter,
            fts: Option<String>,
        ) {
            b.push(" WHERE 1 = 1");
            match f.status {
                ProductStatusFilter::Active => b.push(" AND p.status = 'active'"),
                ProductStatusFilter::Archived => b.push(" AND p.status = 'archived'"),
                ProductStatusFilter::All => b,
            };
            if let Some(c) = f.category_id {
                b.push(" AND p.category_id = ").push_bind(c.to_string());
            }
            if let Some(t) = f.track_stock {
                b.push(" AND EXISTS (SELECT 1 FROM product_variants tv WHERE tv.product_id = p.product_id AND tv.status = 'active' AND tv.track_stock = ")
                    .push_bind(t)
                    .push(")");
            }
            if f.low_stock {
                b.push(concat!(
                    " AND EXISTS (SELECT 1 ",
                    tracked_variants_with_stock!(),
                    " AND (",
                    sellable!(),
                    " < 0 OR ",
                    at_or_below_threshold!(),
                    "))"
                ));
            }
            if let Some(q) = fts {
                b.push(
                    " AND p.product_id IN (SELECT sv.product_id FROM product_search s
                       JOIN product_variants sv ON sv.variant_id = s.variant_id WHERE product_search MATCH ",
                )
                .push_bind(q)
                .push(")");
            }
        }
        let fts = if filter.query.trim().is_empty() {
            None
        } else {
            match fts_query(&filter.query) {
                Some(q) => Some(q),
                // Only punctuation: nothing can match.
                None => {
                    return Ok(ProductPage {
                        rows: Vec::new(),
                        total: 0,
                    });
                }
            }
        };

        let mut count = sqlx::QueryBuilder::new("SELECT count(*) FROM products p");
        filtered(&mut count, filter, fts.clone());
        let total: i64 = count
            .build_query_scalar()
            .fetch_one(self.db().readers())
            .await
            .map_err(q)?;

        let mut b = sqlx::QueryBuilder::new(concat!(
            "SELECT p.product_id, p.name, p.status = 'archived', p.version, p.quick_select,
               (SELECT count(*) FROM product_variants v WHERE v.product_id = p.product_id AND v.status = 'active'),
               (SELECT v.sku FROM product_variants v WHERE v.product_id = p.product_id AND v.is_default = 1),
               (SELECT min(v.price_minor) FROM product_variants v WHERE v.product_id = p.product_id AND v.status = 'active'),
               (SELECT max(v.price_minor) FROM product_variants v WHERE v.product_id = p.product_id AND v.status = 'active'),
               (SELECT count(*) FROM product_barcodes bc JOIN product_variants v ON v.variant_id = bc.variant_id
                 WHERE v.product_id = p.product_id AND v.status = 'active' AND bc.status = 'active'),
               EXISTS (SELECT 1 FROM product_variants v WHERE v.product_id = p.product_id AND v.status = 'active' AND v.track_stock = 1),
               (SELECT coalesce(sum(b.on_hand_milli), 0) FROM stock_balances b JOIN product_variants v ON v.variant_id = b.variant_id
                 WHERE v.product_id = p.product_id AND v.status = 'active' AND v.track_stock = 1 AND b.bucket = 'sellable'),
               EXISTS (SELECT 1 FROM stock_balances b JOIN product_variants v ON v.variant_id = b.variant_id
                 WHERE v.product_id = p.product_id AND v.status = 'active' AND v.track_stock = 1 AND b.bucket = 'sellable'
                   AND b.on_hand_milli < 0),
               p.sync_status = 'conflict',
               EXISTS (SELECT 1 ",
            tracked_variants_with_stock!(),
            " AND ",
            sellable!(),
            " >= 0 AND ",
            at_or_below_threshold!(),
            ")
             FROM products p",
        ));
        filtered(&mut b, filter, fts);
        b.push(" ORDER BY p.name COLLATE NOCASE, p.product_id LIMIT ")
            .push_bind(i64::from(limit))
            .push(" OFFSET ")
            .push_bind(i64::from(offset));
        type Row = (
            String,
            String,
            bool,
            i64,
            bool,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
            i64,
            bool,
            i64,
            bool,
            bool,
            bool,
        );
        let rows: Vec<Row> = b
            .build_query_as()
            .fetch_all(self.db().readers())
            .await
            .map_err(q)?;
        let rows = rows
            .into_iter()
            .map(
                |(
                    id,
                    name,
                    archived,
                    version,
                    quick,
                    variants,
                    sku,
                    min,
                    max,
                    barcodes,
                    track,
                    on_hand,
                    negative,
                    conflict,
                    low,
                )| {
                    Ok(ProductRow {
                        product_id: id.parse().map_err(q)?,
                        name,
                        archived,
                        version: u64::try_from(version).map_err(q)?,
                        variant_count: u32::try_from(variants).map_err(q)?,
                        sku,
                        price_min: Money::from_minor(min.unwrap_or(0)),
                        price_max: Money::from_minor(max.unwrap_or(0)),
                        barcode_count: u32::try_from(barcodes).map_err(q)?,
                        track_stock: track,
                        quick_select: quick,
                        on_hand: track.then(|| pos_domain::Quantity::from_milli(on_hand)),
                        negative_stock: negative,
                        low_stock: low,
                        sync_conflict: conflict,
                    })
                },
            )
            .collect::<Result<_, QueryError>>()?;
        Ok(ProductPage {
            rows,
            total: u32::try_from(total).map_err(q)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::fts_query;

    #[test]
    fn fts_queries_are_sanitised_prefix_terms() {
        assert_eq!(fts_query("Coca cola").as_deref(), Some("\"coca\"* \"cola\"*"));
        assert_eq!(
            fts_query("name:x OR \"y\" NEAR(a b)").as_deref(),
            Some("\"name\"* \"x\"* \"or\"* \"y\"* \"near\"* \"a\"* \"b\"*")
        );
        assert_eq!(fts_query("  -- ").as_deref(), None);
    }
}
