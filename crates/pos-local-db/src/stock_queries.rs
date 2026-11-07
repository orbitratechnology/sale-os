//! Stock reads for selling (E11-T04): the negative-stock policy and on-hand balances.

use crate::SqliteStore;
use pos_application::queries::{QueryError, StockQueries};
use pos_domain::inventory::NegativeStockPolicy;
use pos_domain::{LocationId, Quantity, VariantId};
use std::collections::BTreeMap;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

#[async_trait::async_trait]
impl StockQueries for SqliteStore {
    async fn negative_stock_policy(&self) -> Result<NegativeStockPolicy, QueryError> {
        let v: Option<String> = sqlx::query_scalar(
            "SELECT value_json FROM business_settings WHERE key = 'inventory.allow_negative' LIMIT 1",
        )
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        Ok(match v.as_deref().map(str::trim) {
            Some("false") => NegativeStockPolicy::Block,
            // Missing or true: the default (PRD INV-008) sells and flags.
            _ => NegativeStockPolicy::AllowAndFlag,
        })
    }

    async fn on_hand(
        &self,
        location: LocationId,
        variants: &[VariantId],
    ) -> Result<BTreeMap<VariantId, Quantity>, QueryError> {
        let mut out = BTreeMap::new();
        if variants.is_empty() {
            return Ok(out);
        }
        let mut b = sqlx::QueryBuilder::new(
            "SELECT variant_id, on_hand_milli FROM stock_balances WHERE bucket = 'sellable' AND location_id = ",
        );
        b.push_bind(location.to_string()).push(" AND variant_id IN (");
        let mut list = b.separated(", ");
        for v in variants {
            list.push_bind(v.to_string());
        }
        b.push(")");
        let rows: Vec<(String, i64)> = b
            .build_query_as()
            .fetch_all(self.db().readers())
            .await
            .map_err(q)?;
        for (v, milli) in rows {
            out.insert(v.parse().map_err(q)?, Quantity::from_milli(milli));
        }
        Ok(out)
    }
}
