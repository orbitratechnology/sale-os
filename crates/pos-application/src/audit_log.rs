//! The owner's audit viewer (E15-T01, IAM-004): newest first, filtered by action, user and
//! business days, a page at a time. Reads only: the audit log is append-only.

use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, QueryError};
use pos_domain::time::BusinessDate;
use pos_domain::{ErrorCode, Permission, Timestamp, UserId};
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditFilter {
    /// An exact action (`sale.completed`) or a group prefix ending in `.` (`cart.`).
    pub action: Option<String>,
    /// The acting user (or the approver).
    pub user: Option<UserId>,
    pub from: Option<BusinessDate>,
    pub to: Option<BusinessDate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRow {
    pub audit_id: String,
    pub occurred_at: Timestamp,
    pub local_time: String,
    pub action: String,
    pub actor_name: Option<String>,
    pub approver_name: Option<String>,
    pub entity_type: String,
    pub entity_id: String,
    /// A human number for the entity when there is one (receipt, return, document, session).
    pub entity_label: Option<String>,
    /// The metadata as stored (JSON text).
    pub metadata: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditPage {
    pub rows: Vec<AuditRow>,
    /// Pass back as `before` for the next (older) page.
    pub next: Option<String>,
}

/// A time range and filters, already resolved to instants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditQuery {
    pub action: Option<String>,
    pub user: Option<UserId>,
    pub from: Option<Timestamp>,
    /// Exclusive.
    pub until: Option<Timestamp>,
    /// `(occurred_at, audit_id)` of the last row seen.
    pub before: Option<(i64, String)>,
    pub limit: u32,
}

#[async_trait::async_trait]
pub trait AuditQueries: Send + Sync {
    async fn list_audit(&self, q: &AuditQuery) -> Result<AuditPage, QueryError>;
}

pub struct AuditService {
    pub audit: Arc<dyn AuditQueries>,
    /// For the business time zone.
    pub pricing: Arc<dyn CartQueries>,
}

fn invalid(m: &str) -> AppError {
    AppError::Validation(m.into(), ErrorCode::ValidationFailed)
}

impl AuditService {
    pub async fn list(
        &self,
        ctx: &CommandContext,
        filter: &AuditFilter,
        before: Option<&str>,
        limit: Option<u32>,
    ) -> Result<AuditPage, AppError> {
        ctx.authorize(Permission::AuditView)?;
        if let (Some(f), Some(t)) = (filter.from, filter.to)
            && f > t
        {
            return Err(invalid("the start date is after the end date"));
        }
        let tz = pos_domain::time::parse_time_zone(&self.pricing.pricing_settings().await?.timezone)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let day = |d: BusinessDate| {
            pos_domain::time::day_start(d, tz).map_err(|e| AppError::Internal(e.to_string()))
        };
        let next_day =
            |d: BusinessDate| BusinessDate::from_naive(d.as_naive().succ_opt().unwrap_or(d.as_naive()));
        let before = match before {
            None => None,
            Some(s) => {
                let (at, id) = s
                    .split_once('|')
                    .ok_or_else(|| invalid("that page token is not valid"))?;
                Some((
                    at.parse::<i64>()
                        .map_err(|_| invalid("that page token is not valid"))?,
                    id.to_owned(),
                ))
            }
        };
        let action = filter
            .action
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_owned);
        Ok(self
            .audit
            .list_audit(&AuditQuery {
                action,
                user: filter.user,
                from: filter.from.map(day).transpose()?,
                until: filter.to.map(|t| day(next_day(t))).transpose()?,
                before,
                limit: limit.unwrap_or(50).clamp(1, 200),
            })
            .await?)
    }
}
