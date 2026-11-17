//! Categories and tax configuration reads (E06-T05), archived rows included.

use crate::SqliteStore;
use pos_application::config::ConfigQueries;
use pos_application::queries::QueryError;
use pos_application::records::{
    BusinessRecord, CategoryRecord, RegisterRecord, TaxCategoryRecord, TaxRateRecord,
};
use pos_domain::Timestamp;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

#[async_trait::async_trait]
impl ConfigQueries for SqliteStore {
    async fn business(&self) -> Result<Option<BusinessRecord>, QueryError> {
        type Row = (
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            i64,
            bool,
            i64,
            i64,
            i64,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT business_id, name, legal_name, tax_registration_no, address_json, phone, email, logo_path,
                    country_code, timezone, currency_code, currency_exponent, prices_include_tax, version,
                    created_at, updated_at
             FROM businesses LIMIT 1",
        )
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        row.map(
            |(
                id,
                name,
                legal,
                tax_no,
                address,
                phone,
                email,
                logo,
                country,
                tz,
                code,
                exp,
                incl,
                version,
                created,
                updated,
            )| {
                let code = pos_domain::CurrencyCode::new(&code).map_err(q)?;
                Ok(BusinessRecord {
                    business_id: id.parse().map_err(q)?,
                    name,
                    legal_name: legal,
                    tax_registration_no: tax_no,
                    address: serde_json::from_str(&address).map_err(q)?,
                    phone,
                    email,
                    logo_path: logo,
                    country_code: country,
                    timezone: tz,
                    currency: pos_domain::Currency::new(code, u8::try_from(exp).map_err(q)?).map_err(q)?,
                    prices_include_tax: incl,
                    version: u64::try_from(version).map_err(q)?,
                    created_at: Timestamp::from_millis(created),
                    updated_at: Timestamp::from_millis(updated),
                })
            },
        )
        .transpose()
    }

    async fn register(&self, id: pos_domain::RegisterId) -> Result<Option<RegisterRecord>, QueryError> {
        type Row = (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            i64,
            i64,
            i64,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT register_id, business_id, location_id, name, code, receipt_prefix, status, version, created_at,
                    updated_at
             FROM registers WHERE register_id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        row.map(
            |(id, b, loc, name, code, prefix, status, version, created, updated)| {
                Ok(RegisterRecord {
                    register_id: id.parse().map_err(q)?,
                    business_id: b.parse().map_err(q)?,
                    location_id: loc.parse().map_err(q)?,
                    name,
                    code,
                    receipt_prefix: prefix,
                    retired: status == "retired",
                    version: u64::try_from(version).map_err(q)?,
                    created_at: Timestamp::from_millis(created),
                    updated_at: Timestamp::from_millis(updated),
                })
            },
        )
        .transpose()
    }

    async fn all_categories(&self) -> Result<Vec<CategoryRecord>, QueryError> {
        type Row = (String, String, Option<String>, String, i64, String, i64, i64, i64);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT category_id, business_id, parent_id, name, sort_order, status, version, created_at, updated_at
             FROM categories ORDER BY sort_order, name COLLATE NOCASE",
        )
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter()
            .map(|(id, b, parent, name, sort, status, version, created, updated)| {
                Ok(CategoryRecord {
                    category_id: id.parse().map_err(q)?,
                    business_id: b.parse().map_err(q)?,
                    parent_id: parent.map(|p| p.parse().map_err(q)).transpose()?,
                    name,
                    sort_order: sort,
                    archived: status == "archived",
                    version: u64::try_from(version).map_err(q)?,
                    created_at: Timestamp::from_millis(created),
                    updated_at: Timestamp::from_millis(updated),
                })
            })
            .collect()
    }

    async fn all_tax_rates(&self) -> Result<Vec<TaxRateRecord>, QueryError> {
        type Row = (
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
            String,
            i64,
            i64,
            i64,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT tax_rate_id, business_id, name, rate_ppm, effective_from, effective_to, status, version, created_at, updated_at
             FROM tax_rates ORDER BY name COLLATE NOCASE, effective_from",
        )
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter()
            .map(
                |(id, b, name, ppm, from, to, status, version, created, updated)| {
                    Ok(TaxRateRecord {
                        tax_rate_id: id.parse().map_err(q)?,
                        business_id: b.parse().map_err(q)?,
                        name,
                        rate: pos_domain::RatePpm::new(u32::try_from(ppm).map_err(q)?).map_err(q)?,
                        effective_from: from.map(|d| d.parse().map_err(q)).transpose()?,
                        effective_to: to.map(|d| d.parse().map_err(q)).transpose()?,
                        archived: status == "archived",
                        version: u64::try_from(version).map_err(q)?,
                        created_at: Timestamp::from_millis(created),
                        updated_at: Timestamp::from_millis(updated),
                    })
                },
            )
            .collect()
    }

    async fn all_tax_categories(&self) -> Result<Vec<TaxCategoryRecord>, QueryError> {
        type Row = (
            String,
            String,
            String,
            bool,
            String,
            i64,
            i64,
            i64,
            Option<String>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT c.tax_category_id, c.business_id, c.name, c.is_default, c.status, c.version, c.created_at, c.updated_at,
               (SELECT group_concat(tax_rate_id, ',') FROM tax_category_rates r WHERE r.tax_category_id = c.tax_category_id)
             FROM tax_categories c ORDER BY c.is_default DESC, c.name COLLATE NOCASE",
        )
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter()
            .map(
                |(id, b, name, is_default, status, version, created, updated, rates)| {
                    let mut rate_ids = rates
                        .unwrap_or_default()
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.parse().map_err(q))
                        .collect::<Result<Vec<pos_domain::TaxRateId>, _>>()?;
                    rate_ids.sort();
                    Ok(TaxCategoryRecord {
                        tax_category_id: id.parse().map_err(q)?,
                        business_id: b.parse().map_err(q)?,
                        name,
                        is_default,
                        rate_ids,
                        archived: status == "archived",
                        version: u64::try_from(version).map_err(q)?,
                        created_at: Timestamp::from_millis(created),
                        updated_at: Timestamp::from_millis(updated),
                    })
                },
            )
            .collect()
    }
}
