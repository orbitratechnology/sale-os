//! Catalogue reads (E06-T04): full aggregates, owner checks, code lookup and FTS search.

use crate::SqliteStore;
use pos_application::queries::{CatalogHit, CatalogQueries, MatchedBy, QueryError, TaxCategorySummary};
use pos_domain::catalog::{
    Barcode, BarcodeQualifier, Product, ProductType, Symbology, Unit, Variant, normalize_barcode,
};
use pos_domain::{Money, ProductId, Quantity};
use std::collections::BTreeMap;

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
