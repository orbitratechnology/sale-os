//! Receipt reads (E13-T02/T03).

use crate::SqliteStore;
use pos_application::queries::{QueryError, ReceiptInfo, ReceiptQueries};
use pos_domain::{ReceiptId, RegisterId, SaleId, UserId};

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

/// A JSON array of strings from a setting value; anything else is no lines.
fn lines(v: Option<String>) -> Vec<String> {
    v.and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default()
}

#[async_trait::async_trait]
impl ReceiptQueries for SqliteStore {
    async fn receipt_info(&self, register: RegisterId, cashier: UserId) -> Result<ReceiptInfo, QueryError> {
        let r = self.db().readers();
        let (business, address, phone, tax_no, register_name, code): (
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT b.name, b.address_json, b.phone, b.tax_registration_no, r.name, r.code
                 FROM registers r JOIN businesses b ON b.business_id = r.business_id WHERE r.register_id = ?",
        )
        .bind(register.to_string())
        .fetch_one(r)
        .await
        .map_err(q)?;
        let cashier_name: Option<String> =
            sqlx::query_scalar("SELECT display_name FROM staff_users WHERE user_id = ?")
                .bind(cashier.to_string())
                .fetch_optional(r)
                .await
                .map_err(q)?;
        let setting = |key: &'static str| async move {
            sqlx::query_scalar::<_, String>("SELECT value_json FROM business_settings WHERE key = ? LIMIT 1")
                .bind(key)
                .fetch_optional(r)
                .await
                .map_err(q)
        };
        let address: serde_json::Value = serde_json::from_str(&address).unwrap_or_default();
        let address_lines = address
            .get("lines")
            .and_then(|l| l.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let width = setting("receipt.paper_width_mm")
            .await?
            .and_then(|v| v.trim().parse::<u32>().ok());
        Ok(ReceiptInfo {
            business_name: business,
            address_lines,
            phone,
            tax_registration_no: tax_no,
            header_lines: lines(setting("receipt.header_lines").await?),
            footer_lines: lines(setting("receipt.footer_lines").await?),
            register_name,
            register_code: code,
            cashier_name: cashier_name.unwrap_or_default(),
            paper_width_mm: if width == Some(58) { 58 } else { 80 },
        })
    }

    async fn sale_receipt(&self, sale: SaleId) -> Result<Option<(ReceiptId, String)>, QueryError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT receipt_id, document_json FROM receipts WHERE source_type = 'sale' AND source_id = ?
             ORDER BY content_version DESC LIMIT 1",
        )
        .bind(sale.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        row.map(|(id, doc)| Ok((id.parse().map_err(q)?, doc))).transpose()
    }

    async fn last_sale(&self, register: RegisterId) -> Result<Option<SaleId>, QueryError> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT sale_id FROM sales WHERE register_id = ? ORDER BY receipt_seq DESC LIMIT 1",
        )
        .bind(register.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        id.map(|i| i.parse().map_err(q)).transpose()
    }
}
