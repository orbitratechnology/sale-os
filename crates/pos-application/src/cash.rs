//! Cash sessions (E15-T02, DER-001): open the drawer with a float, close it with a count.
//!
//! - One open session per register (a unique index); sales and returns join it inside
//!   their own transaction.
//! - In live mode, with `cash.require_session_for_sale` (default true), a sale, or a return
//!   paying out cash, needs an open session: `CASH_SESSION_REQUIRED`.
//! - Closing is final. The store recomputes the totals inside the closing transaction and
//!   refuses if they moved, so the recorded expected cash is exactly what the sales say.
//! - Blind count: a cashier closes with the counted amount only; expected cash, variance,
//!   the totals and the Z summary need `CashSessionViewVariance`.

use crate::audit::{self, AuditEntry};
use crate::checkout::receipt_context;
use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, CashQueries, ReceiptQueries, SetupQueries};
use crate::receipts::{ReceiptService, RenderedReceipt};
use crate::records::{CashSessionRecord, RegisterMode};
use crate::write::{NumberSlot, WriteError, cash_session_aggregate};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::cash::{ClosedSession, OpenedSession, SessionTotals, cash_amount, variance};
use pos_domain::{CashSessionId, ErrorCode, Money, Permission, ReceiptId, role_has};
use std::sync::Arc;

const SESSION_NUMBER: NumberSlot = NumberSlot("cash_session");

pub struct CashService {
    pub store: Arc<dyn LocalStore>,
    pub cash: Arc<dyn CashQueries>,
    pub receipts: Arc<dyn ReceiptQueries>,
    pub setup: Arc<dyn SetupQueries>,
    /// For the business time zone.
    pub pricing: Arc<dyn CartQueries>,
}

/// The drawer as the till sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CashState {
    pub session: Option<CashSessionRecord>,
    /// Whether sales need an open session right now (live mode and the setting).
    pub required: bool,
    /// Running totals and expected cash: only for roles that may see the variance.
    pub totals: Option<SessionTotals>,
    pub expected: Option<Money>,
}

/// What closing returns. A blind count (cashier) sees only what was counted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedCash {
    pub session: CashSessionRecord,
    pub counted: Money,
    pub expected: Option<Money>,
    pub variance: Option<Money>,
    pub totals: Option<SessionTotals>,
}

fn no_session() -> AppError {
    AppError::Validation(
        "no cash session is open on this till".into(),
        ErrorCode::CashSessionRequired,
    )
}

fn sees_variance(ctx: &CommandContext) -> Result<bool, AppError> {
    Ok(role_has(ctx.actor()?.role, Permission::CashSessionViewVariance))
}

impl CashService {
    async fn required(&self) -> Result<bool, AppError> {
        let live = matches!(
            self.setup.device_identity().await?.map(|d| d.mode),
            Some(RegisterMode::Live)
        );
        Ok(live && self.setup.cash_session_required().await?)
    }

    pub async fn current(&self, ctx: &CommandContext) -> Result<CashState, AppError> {
        ctx.authorize(Permission::CashSessionOpen)?;
        let session = self.cash.open_cash_session(ctx.register.register_id).await?;
        let (totals, expected) = match &session {
            Some(s) if sees_variance(ctx)? => {
                let totals = self.cash.cash_session_totals(s.id).await?;
                let expected = totals
                    .expected_cash(s.opening_float)
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                (Some(totals), Some(expected))
            }
            _ => (None, None),
        };
        Ok(CashState {
            session,
            required: self.required().await?,
            totals,
            expected,
        })
    }

    pub async fn open(&self, ctx: &CommandContext, float: Money) -> Result<CashSessionRecord, AppError> {
        ctx.authorize(Permission::CashSessionOpen)?;
        let actor = ctx.actor()?.clone();
        let float = cash_amount(float)
            .map_err(|e| AppError::Validation(e.to_string(), ErrorCode::ValidationFailed))?;
        if self
            .cash
            .open_cash_session(ctx.register.register_id)
            .await?
            .is_some()
        {
            return Err(AppError::Write(WriteError::CashSessionAlreadyOpen));
        }
        let timezone = self.pricing.pricing_settings().await?.timezone;
        let tz =
            pos_domain::time::parse_time_zone(&timezone).map_err(|e| AppError::Internal(e.to_string()))?;
        let ids = ctx.ids.as_ref();
        let session = OpenedSession {
            id: CashSessionId::generate(ids),
            opened_by: actor.user_id,
            opened_at: ctx.now(),
            opening_float: float,
            business_date: pos_domain::time::business_date(ctx.now(), tz)
                .map_err(|e| AppError::Internal(e.to_string()))?,
        };
        let aggregate = cash_session_aggregate(session.id);
        let payload = serde_json::json!({
            "session": &session,
            "registerId": ctx.register.register_id,
            "locationId": ctx.register.location_id,
        });
        let set = WriteSet::builder(ctx.write_context())
            .open_cash_session(session.clone(), SESSION_NUMBER)
            .outbox(OutboxDraft::new(
                ids,
                "cash_session.opened",
                1,
                aggregate,
                None,
                &payload,
            )?)
            .audit(
                AuditEntry::new(ids, audit::CASH_SESSION_OPENED, "cash_session", session.id)
                    .with_metadata(serde_json::json!({ "openingFloatMinor": float.minor().to_string() })),
            )
            .build()?;
        self.store.apply(set).await?;
        self.cash
            .cash_session(session.id)
            .await?
            .ok_or_else(|| AppError::Internal("the opened session is not readable".into()))
    }

    /// Closes the open session with the counted cash. Final.
    pub async fn close(
        &self,
        ctx: &CommandContext,
        counted: Money,
        note: Option<String>,
    ) -> Result<ClosedCash, AppError> {
        ctx.authorize(Permission::CashSessionClose)?;
        let actor = ctx.actor()?.clone();
        let counted = cash_amount(counted)
            .map_err(|e| AppError::Validation(e.to_string(), ErrorCode::ValidationFailed))?;
        let note = note.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        let session = self
            .cash
            .open_cash_session(ctx.register.register_id)
            .await?
            .ok_or_else(no_session)?;
        let info = self
            .receipts
            .receipt_info(ctx.register.register_id, actor.user_id)
            .await?;
        let sandbox = !matches!(
            self.setup.device_identity().await?.map(|d| d.mode),
            Some(RegisterMode::Live)
        );
        // A sale may land between reading the totals and closing: the store refuses, and we
        // read again. Three tries is plenty on one till.
        let mut tries = 0;
        let closed = loop {
            tries += 1;
            let totals = self.cash.cash_session_totals(session.id).await?;
            let expected = totals
                .expected_cash(session.opening_float)
                .map_err(|e| AppError::Internal(e.to_string()))?;
            let closed = ClosedSession {
                id: session.id,
                closed_by: actor.user_id,
                closed_at: ctx.now(),
                opening_float: session.opening_float,
                counted,
                expected,
                variance: variance(counted, expected).map_err(|e| AppError::Internal(e.to_string()))?,
                totals,
                note: note.clone(),
            };
            let ids = ctx.ids.as_ref();
            let z = pos_receipts::z_summary(
                &closed,
                &receipt_context(&info),
                &pos_receipts::ZContext {
                    session_number: session.number.clone(),
                    opened_local: session.opened_local.clone(),
                    opened_by: session.opened_by_name.clone(),
                    closed_local: pos_domain::time::local_date_time(
                        ctx.now(),
                        &self.pricing.pricing_settings().await?.timezone,
                    )
                    .unwrap_or_default(),
                    currency_exponent: session.currency_exponent,
                    sandbox,
                },
            );
            let payload = serde_json::json!({
                "session": &closed,
                "sessionNumber": &session.number,
                "registerId": ctx.register.register_id,
            });
            let set = WriteSet::builder(ctx.write_context())
                .close_cash_session(closed.clone())
                .insert_receipt(
                    ReceiptId::generate(ids),
                    ("cash_session", session.id.to_string()),
                    pos_receipts::CONTENT_VERSION,
                    serde_json::to_value(&z).map_err(|e| AppError::Internal(e.to_string()))?,
                    None,
                )
                .outbox(OutboxDraft::new(
                    ids,
                    "cash_session.closed",
                    1,
                    cash_session_aggregate(session.id),
                    None,
                    &payload,
                )?)
                .audit(
                    AuditEntry::new(ids, audit::CASH_SESSION_CLOSED, "cash_session", session.id)
                        .with_metadata(serde_json::json!({
                            "countedMinor": counted.minor().to_string(),
                            "expectedMinor": closed.expected.minor().to_string(),
                            "varianceMinor": closed.variance.minor().to_string(),
                        })),
                )
                .build()?;
            match self.store.apply(set).await {
                Ok(_) => break closed,
                Err(WriteError::CashSessionChanged) if tries < 3 => continue,
                Err(e) => return Err(e.into()),
            }
        };
        let session = self
            .cash
            .cash_session(session.id)
            .await?
            .ok_or_else(|| AppError::Internal("the closed session is not readable".into()))?;
        let full = sees_variance(ctx)?;
        Ok(ClosedCash {
            session,
            counted,
            expected: full.then_some(closed.expected),
            variance: full.then_some(closed.variance),
            totals: full.then_some(closed.totals),
        })
    }

    /// The stored Z summary of a closed session (`None`: the last one on this till).
    pub async fn z_summary(
        &self,
        ctx: &CommandContext,
        session: Option<CashSessionId>,
        width_mm: Option<u32>,
    ) -> Result<RenderedReceipt, AppError> {
        let (_, json) = self.z_receipt(ctx, session).await?;
        ReceiptService {
            receipts: self.receipts.clone(),
        }
        .rendered(ctx, &json, width_mm)
        .await
    }

    /// The Z summary's stored receipt, for printing. Needs `CashSessionViewVariance`.
    pub async fn z_receipt(
        &self,
        ctx: &CommandContext,
        session: Option<CashSessionId>,
    ) -> Result<(ReceiptId, String), AppError> {
        ctx.authorize(Permission::CashSessionViewVariance)?;
        let id = match session {
            Some(id) => id,
            None => self
                .cash
                .last_closed_cash_session(ctx.register.register_id)
                .await?
                .ok_or_else(|| {
                    AppError::Validation(
                        "no closed session on this till yet".into(),
                        ErrorCode::ValidationFailed,
                    )
                })?,
        };
        self.cash.cash_session_receipt(id).await?.ok_or_else(|| {
            AppError::Validation(
                "that session has no Z summary".into(),
                ErrorCode::ValidationFailed,
            )
        })
    }
}
