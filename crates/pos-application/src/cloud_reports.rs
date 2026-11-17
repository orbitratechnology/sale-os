//! Consolidated reports across every register (E14-T05, REP-008 minimal, PRD §15, §11.6).
//!
//! The figures come from the cloud (`GET /v1/reports/sales`). Each one fetched is kept on
//! this register; when the cloud cannot be reached, the last one kept for the same range and
//! grouping is shown, marked stale. Sales never wait on any of this.

use crate::context::{AppError, CommandContext};
use crate::queries::QueryError;
use crate::reports::check_range;
use crate::sync::{SyncStore, SyncTransport, TransportError, link_error};
use pos_domain::{BusinessDate, Clock, ErrorCode, Permission, Timestamp};
use pos_sync::reports::{GroupBy, SalesReport};
use std::sync::Arc;

/// The reports this register fetched last, one per range and grouping.
#[async_trait::async_trait]
pub trait CloudReportCache: Send + Sync {
    async fn cached_report(&self, key: &str) -> Result<Option<(String, Timestamp)>, QueryError>;
    async fn keep_report(&self, key: &str, json: &str, at: Timestamp) -> Result<(), QueryError>;
}

/// How far the figures can be trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// Every register confirmed its state within 5 minutes, holding nothing unsent.
    FullySynchronized,
    /// `unsynced` registers may hold data the figures lack; `last_synced` (RFC 3339) is the
    /// oldest confirmation among them, if each has confirmed at least once.
    Partial {
        unsynced: usize,
        last_synced: Option<String>,
    },
    /// The cloud could not be reached: these are the figures kept from `fetched_at`.
    Stale { reason: ErrorCode },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CloudSalesReport {
    pub report: SalesReport,
    pub fetched_at: Timestamp,
    pub freshness: Freshness,
}

pub fn freshness(report: &SalesReport) -> Freshness {
    let c = &report.completeness;
    if c.fully_synchronized() {
        Freshness::FullySynchronized
    } else {
        Freshness::Partial {
            unsynced: c.unsynced(),
            last_synced: c.last_synced().map(str::to_owned),
        }
    }
}

pub fn cache_key(from: BusinessDate, to: BusinessDate, group_by: GroupBy) -> String {
    let by = match group_by {
        GroupBy::Day => "day",
        GroupBy::Register => "register",
        GroupBy::Tender => "tender",
    };
    format!("sales:{by}:{from}:{to}")
}

pub struct CloudReportService {
    pub transport: Arc<dyn SyncTransport>,
    pub sync: Arc<dyn SyncStore>,
    pub cache: Arc<dyn CloudReportCache>,
    pub clock: Arc<dyn Clock>,
}

impl CloudReportService {
    pub async fn sales(
        &self,
        ctx: &CommandContext,
        from: BusinessDate,
        to: BusinessDate,
        group_by: GroupBy,
    ) -> Result<CloudSalesReport, AppError> {
        ctx.authorize(Permission::ReportsView)?;
        check_range(from, to)?;
        if !self.sync.push_state().await?.linked {
            return Err(AppError::Validation(
                "this register is not linked to the cloud".into(),
                ErrorCode::SyncNotLinked,
            ));
        }
        let key = cache_key(from, to, group_by);
        match self
            .transport
            .sales_report(&from.to_string(), &to.to_string(), group_by)
            .await
        {
            Ok(report) => {
                let at = self.clock.now();
                let json = serde_json::to_string(&report).map_err(|e| AppError::Internal(e.to_string()))?;
                self.cache.keep_report(&key, &json, at).await?;
                Ok(CloudSalesReport {
                    freshness: freshness(&report),
                    report,
                    fetched_at: at,
                })
            }
            Err(e) => self
                .kept(&key, &e)
                .await?
                .ok_or_else(|| AppError::from(link_error(&e))),
        }
    }

    async fn kept(&self, key: &str, e: &TransportError) -> Result<Option<CloudSalesReport>, AppError> {
        let Some((json, fetched_at)) = self.cache.cached_report(key).await? else {
            return Ok(None);
        };
        let Ok(report) = serde_json::from_str::<SalesReport>(&json) else {
            // Kept by an older app in another shape: as good as none.
            return Ok(None);
        };
        Ok(Some(CloudSalesReport {
            report,
            fetched_at,
            freshness: Freshness::Stale {
                reason: link_error(e).code(),
            },
        }))
    }
}
