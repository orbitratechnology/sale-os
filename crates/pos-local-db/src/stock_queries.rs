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
        self.bucket_on_hand(location, variants, "sellable").await
    }

    async fn stock_drift(&self) -> Result<Vec<pos_application::records::StockDrift>, QueryError> {
        let rows: Vec<(String, String, String, i64, i64)> = sqlx::query_as(
            "WITH m AS (SELECT location_id, variant_id, bucket, sum(quantity_delta_milli) AS q
                        FROM stock_movements GROUP BY location_id, variant_id, bucket)
             SELECT coalesce(m.location_id, b.location_id), coalesce(m.variant_id, b.variant_id),
                    coalesce(m.bucket, b.bucket), coalesce(b.on_hand_milli, 0), coalesce(m.q, 0)
             FROM m FULL OUTER JOIN stock_balances b
               ON b.location_id = m.location_id AND b.variant_id = m.variant_id AND b.bucket = m.bucket
             WHERE coalesce(b.on_hand_milli, 0) <> coalesce(m.q, 0)",
        )
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter()
            .map(|(l, v, bucket, recorded, sum)| {
                Ok(pos_application::records::StockDrift {
                    location_id: l.parse().map_err(q)?,
                    variant_id: v.parse().map_err(q)?,
                    bucket,
                    recorded: Quantity::from_milli(recorded),
                    from_movements: Quantity::from_milli(sum),
                })
            })
            .collect()
    }

    async fn damaged_on_hand(
        &self,
        location: LocationId,
        variants: &[VariantId],
    ) -> Result<BTreeMap<VariantId, Quantity>, QueryError> {
        self.bucket_on_hand(location, variants, "damaged").await
    }
}

impl SqliteStore {
    async fn bucket_on_hand(
        &self,
        location: LocationId,
        variants: &[VariantId],
        bucket: &'static str,
    ) -> Result<BTreeMap<VariantId, Quantity>, QueryError> {
        let mut out = BTreeMap::new();
        if variants.is_empty() {
            return Ok(out);
        }
        let mut b =
            sqlx::QueryBuilder::new("SELECT variant_id, on_hand_milli FROM stock_balances WHERE bucket = ");
        b.push_bind(bucket).push(" AND location_id = ");
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

/// What the startup stock check found (E11-T06).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupStockCheck {
    /// `quick_check` passed and every balance equals its movements.
    Healthy,
    /// This many balances differ from their movements (rebuild from diagnostics).
    Drift(usize),
    /// `quick_check` reported a problem: the stock check was not run.
    IntegrityProblem(String),
    /// The budget ran out before the check finished; it is left for later.
    Deferred,
}

impl SqliteStore {
    /// At startup: `PRAGMA quick_check`, then the stock verify, within `budget`.
    pub async fn startup_stock_check(
        &self,
        budget: std::time::Duration,
    ) -> Result<StartupStockCheck, QueryError> {
        let check = async {
            let quick: String = sqlx::query_scalar("PRAGMA quick_check")
                .fetch_one(self.db().readers())
                .await
                .map_err(q)?;
            if quick != "ok" {
                return Ok(StartupStockCheck::IntegrityProblem(quick));
            }
            let drift = self.stock_drift().await?;
            Ok(if drift.is_empty() {
                StartupStockCheck::Healthy
            } else {
                StartupStockCheck::Drift(drift.len())
            })
        };
        match tokio::time::timeout(budget, check).await {
            Ok(result) => result,
            Err(_) => Ok(StartupStockCheck::Deferred),
        }
    }
}
