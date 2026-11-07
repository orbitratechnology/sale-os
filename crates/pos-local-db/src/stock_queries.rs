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

    async fn stock_history(
        &self,
        variant: VariantId,
        limit: u32,
    ) -> Result<Vec<pos_application::records::StockHistoryRow>, QueryError> {
        type Row = (
            String,
            i64,
            String,
            String,
            i64,
            String,
            String,
            Option<String>,
            String,
            String,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT m.movement_id, m.occurred_at, m.movement_type, m.bucket, m.quantity_delta_milli, m.source_type,
               m.source_id,
               CASE m.source_type
                 WHEN 'sale' THEN (SELECT receipt_number FROM sales WHERE sale_id = m.source_id)
                 WHEN 'return' THEN (SELECT return_number FROM returns WHERE return_id = m.source_id)
                 ELSE (SELECT document_no FROM inventory_documents WHERE document_id = m.source_id) END,
               coalesce(u.display_name, ''), b.timezone
             FROM stock_movements m
             JOIN businesses b ON b.business_id = m.business_id
             LEFT JOIN staff_users u ON u.user_id = m.actor_user_id
             WHERE m.variant_id = ?
             ORDER BY m.occurred_at DESC, m.rowid DESC LIMIT ?",
        )
        .bind(variant.to_string())
        .bind(i64::from(limit))
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter()
            .map(
                |(id, at, kind, bucket, delta, source_type, source_id, number, actor, zone)| {
                    Ok(pos_application::records::StockHistoryRow {
                        movement_id: id.parse().map_err(q)?,
                        occurred_at: pos_domain::Timestamp::from_millis(at),
                        local_time: pos_domain::time::local_date_time(
                            pos_domain::Timestamp::from_millis(at),
                            &zone,
                        )
                        .unwrap_or_default(),
                        movement_type: kind,
                        bucket,
                        delta: Quantity::from_milli(delta),
                        source_type,
                        source_id,
                        source_number: number.unwrap_or_default(),
                        actor_name: actor,
                    })
                },
            )
            .collect()
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
