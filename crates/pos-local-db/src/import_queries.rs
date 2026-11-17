//! Import staging reads and the catalogue index the import matches against (E06-T07).

use crate::SqliteStore;
use pos_application::import::{CatalogIndex, ImportQueries};
use pos_application::queries::QueryError;
use pos_application::records::{ImportJobRecord, ImportRowRecord, ImportRowStatus, ImportStatus};
use pos_domain::{ImportJobId, Timestamp};

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

#[async_trait::async_trait]
impl ImportQueries for SqliteStore {
    async fn import_job(&self, id: ImportJobId) -> Result<Option<ImportJobRecord>, QueryError> {
        type Row = (
            String,
            String,
            Option<String>,
            String,
            String,
            i64,
            i64,
            i64,
            String,
            i64,
            Option<i64>,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT file_name, file_sha256, mapping_json, options_json, status, total_rows, valid_rows, error_rows,
                    created_by, created_at, committed_at
             FROM import_jobs WHERE job_id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        row.map(
            |(name, sha, mapping, options, status, total, valid, errors, by, created, committed)| {
                Ok(ImportJobRecord {
                    job_id: id,
                    file_name: name,
                    file_sha256: sha,
                    mapping: mapping.map(|m| serde_json::from_str(&m)).transpose().map_err(q)?,
                    options: serde_json::from_str(&options).map_err(q)?,
                    status: ImportStatus::from_code(&status)
                        .ok_or_else(|| q(format!("import status {status}")))?,
                    total_rows: u32::try_from(total).map_err(q)?,
                    valid_rows: u32::try_from(valid).map_err(q)?,
                    error_rows: u32::try_from(errors).map_err(q)?,
                    created_by: by.parse().map_err(q)?,
                    created_at: Timestamp::from_millis(created),
                    committed_at: committed.map(Timestamp::from_millis),
                })
            },
        )
        .transpose()
    }

    async fn import_rows(&self, id: ImportJobId) -> Result<Vec<ImportRowRecord>, QueryError> {
        let rows: Vec<(i64, String, Option<String>, String, String)> = sqlx::query_as(
            "SELECT row_no, raw_json, parsed_json, status, errors_json FROM import_rows WHERE job_id = ? ORDER BY row_no",
        )
        .bind(id.to_string())
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter()
            .map(|(line, raw, parsed, status, errors)| {
                Ok(ImportRowRecord {
                    line: u32::try_from(line).map_err(q)?,
                    raw: serde_json::from_str(&raw).map_err(q)?,
                    parsed: parsed.map(|p| serde_json::from_str(&p)).transpose().map_err(q)?,
                    status: ImportRowStatus::from_code(&status)
                        .ok_or_else(|| q(format!("row status {status}")))?,
                    errors: serde_json::from_str(&errors).map_err(q)?,
                })
            })
            .collect()
    }

    async fn catalog_index(&self) -> Result<CatalogIndex, QueryError> {
        let r = self.db().readers();
        let mut index = CatalogIndex::default();
        let products: Vec<(String, String)> =
            sqlx::query_as("SELECT product_id, name FROM products WHERE status = 'active'")
                .fetch_all(r)
                .await
                .map_err(q)?;
        for (id, name) in products {
            let id = id.parse().map_err(q)?;
            index
                .names
                .entry(name.trim().to_lowercase())
                .or_default()
                .push(id);
            index.product_names.insert(id, name);
        }
        let codes: Vec<(String, String)> = sqlx::query_as(
            "SELECT b.value_normalized, v.product_id FROM product_barcodes b
             JOIN product_variants v ON v.variant_id = b.variant_id
             JOIN products p ON p.product_id = v.product_id
             WHERE b.status = 'active' AND v.status = 'active' AND p.status = 'active'",
        )
        .fetch_all(r)
        .await
        .map_err(q)?;
        for (code, id) in codes {
            index.barcodes.insert(code, id.parse().map_err(q)?);
        }
        let skus: Vec<(String, String)> = sqlx::query_as(
            "SELECT v.sku, v.product_id FROM product_variants v JOIN products p ON p.product_id = v.product_id
             WHERE v.sku IS NOT NULL AND v.status = 'active' AND p.status = 'active'",
        )
        .fetch_all(r)
        .await
        .map_err(q)?;
        for (sku, id) in skus {
            index.skus.insert(sku.to_uppercase(), id.parse().map_err(q)?);
        }
        Ok(index)
    }
}
