//! Integrated card payments at the till (E22-T02, PAY-009/010): start an attempt on the
//! configured terminal, follow it, cancel it, and settle uncertain ones. The sale itself is
//! completed by `checkout_complete` with the approved attempts.
//!
//! Which terminal: the enabled `payment_terminal` device of this till. The sandbox terminal
//! works only while the till is in sandbox mode, so practice payments can never be taken
//! for real ones.

use crate::app_commands::err;
use crate::{AppServices, IpcError};
use pos_application::payments::ProviderPaymentFlow;
use pos_domain::payment::{AttemptStatus, PaymentAttempt, Resolution};
use pos_domain::{ApprovalId, CartId, ErrorCode, Money, PaymentAttemptId};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PaymentAttemptDto {
    pub attempt_id: PaymentAttemptId,
    pub cart_id: CartId,
    pub provider: String,
    pub amount_minor: String,
    /// `pending`, `approved`, `declined`, `cancelled`, `reversed`, `reconciliation_required`.
    pub status: String,
    /// Why it is in this state (decline reason, "the terminal timed out"…).
    pub reason: Option<String>,
    pub provider_ref: Option<String>,
    pub auth_code: Option<String>,
    pub card_brand: Option<String>,
    /// Last four digits at most (PAY-005).
    pub masked_pan: Option<String>,
    /// Approved and not yet part of a sale: pass it to `checkout_complete`.
    pub usable: bool,
}

impl PaymentAttemptDto {
    pub fn from_attempt(a: &PaymentAttempt) -> Self {
        use pos_domain::payment::Approval;
        let (card, operator_ref) = match a.approval() {
            Some(Approval::Provider(c)) => (Some(c), None),
            Some(Approval::Operator { reference }) => (None, reference.clone()),
            None => (None, None),
        };
        Self {
            attempt_id: a.id(),
            cart_id: a.cart_id(),
            provider: a.provider().into(),
            amount_minor: a.amount().minor().to_string(),
            status: a.status().as_str().into(),
            reason: a.status_reason().map(str::to_owned),
            provider_ref: card.map(|p| p.provider_ref().to_owned()).or(operator_ref),
            auth_code: card.and_then(|p| p.auth_code().map(str::to_owned)),
            card_brand: card.and_then(|p| p.card_brand().map(str::to_owned)),
            masked_pan: card.and_then(|p| p.masked_pan().map(|m| m.as_str().to_owned())),
            usable: a.status() == AttemptStatus::Approved && a.tender_id().is_none(),
        }
    }
}

/// An approved terminal payment to complete the sale with.
#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TerminalTenderDto {
    pub attempt_id: PaymentAttemptId,
    pub amount_minor: String,
}

/// How the operator settles an attempt whose outcome is unknown, after looking at the
/// terminal: `approved` (with the slip's reference), `declined` or `cancelled`.
#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionDto {
    pub outcome: String,
    pub reference: Option<String>,
}

fn minor(s: &str) -> Result<Money, IpcError> {
    s.parse::<i64>()
        .ok()
        .filter(|m| *m > 0)
        .map(Money::from_minor)
        .ok_or_else(|| {
            IpcError::coded(
                ErrorCode::ValidationFailed,
                "amount must be a positive number of minor units",
            )
        })
}

pub(crate) async fn flow(services: &AppServices) -> Result<Arc<ProviderPaymentFlow>, IpcError> {
    services.payment_flow().await.ok_or_else(|| {
        IpcError::coded(
            ErrorCode::DeviceUnavailable,
            "no card terminal is set up on this till (Settings → Devices)",
        )
    })
}

/// The tenders as the payment flow takes them.
pub(crate) fn provider_tenders(
    t: &[TerminalTenderDto],
) -> Result<Vec<pos_application::payments::ProviderTender>, IpcError> {
    t.iter()
        .map(|t| {
            Ok(pos_application::payments::ProviderTender {
                attempt_id: t.attempt_id,
                amount: minor(&t.amount_minor)?,
            })
        })
        .collect()
}

/// Sends `amountMinor` of the open cart to the terminal.
#[tauri::command]
#[specta::specta]
pub async fn payment_start(
    services: State<'_, AppServices>,
    cart_id: CartId,
    amount_minor: String,
) -> Result<PaymentAttemptDto, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::SaleComplete).map_err(err)?;
    let flow = flow(&services).await?;
    if flow.port.provider() == "sandbox" && !services.in_sandbox().await? {
        return Err(IpcError::coded(
            ErrorCode::DeviceUnavailable,
            "the sandbox terminal takes practice payments only; set up a real terminal for live sales",
        ));
    }
    let a = flow
        .start(&ctx, cart_id, minor(&amount_minor)?)
        .await
        .map_err(err)?;
    Ok(PaymentAttemptDto::from_attempt(&a))
}

/// Asks the terminal again (the UI calls this while the customer pays).
#[tauri::command]
#[specta::specta]
pub async fn payment_poll(
    services: State<'_, AppServices>,
    attempt_id: PaymentAttemptId,
) -> Result<PaymentAttemptDto, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::SaleComplete).map_err(err)?;
    let a = flow(&services).await?.poll(&ctx, attempt_id).await.map_err(err)?;
    Ok(PaymentAttemptDto::from_attempt(&a))
}

#[tauri::command]
#[specta::specta]
pub async fn payment_cancel(
    services: State<'_, AppServices>,
    attempt_id: PaymentAttemptId,
) -> Result<PaymentAttemptDto, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::SaleComplete).map_err(err)?;
    let a = flow(&services)
        .await?
        .cancel(&ctx, attempt_id)
        .await
        .map_err(err)?;
    Ok(PaymentAttemptDto::from_attempt(&a))
}

/// Settles an attempt whose outcome is unknown, after checking the terminal. A manager's
/// decision: cashiers need an approval.
#[tauri::command]
#[specta::specta]
pub async fn payment_resolve(
    services: State<'_, AppServices>,
    attempt_id: PaymentAttemptId,
    resolution: ResolutionDto,
    reason: String,
    approval_token_id: Option<ApprovalId>,
) -> Result<PaymentAttemptDto, IpcError> {
    let r = match resolution.outcome.as_str() {
        "approved" => Resolution::Approved {
            reference: resolution.reference,
        },
        "declined" => Resolution::Declined,
        "cancelled" => Resolution::Cancelled,
        other => {
            return Err(IpcError::coded(
                ErrorCode::ValidationFailed,
                format!("unknown outcome {other:?}"),
            ));
        }
    };
    let (ctx, token) = services.context_with_approval(approval_token_id).await?;
    if let Err(e) = ctx.authorize_with_approval(pos_domain::Permission::CartVoid, attempt_id, None) {
        let result = Err(err(e));
        services.settle(token, &result);
        return result;
    }
    let result = match flow(&services).await {
        Ok(f) => f
            .resolve(&ctx, attempt_id, r, &reason)
            .await
            .map(|a| PaymentAttemptDto::from_attempt(&a))
            .map_err(err),
        Err(e) => Err(e),
    };
    services.settle(token, &result);
    result
}

/// Gives back an approved payment whose sale was never completed (cart voided).
#[tauri::command]
#[specta::specta]
pub async fn payment_reverse(
    services: State<'_, AppServices>,
    attempt_id: PaymentAttemptId,
    reason: String,
    approval_token_id: Option<ApprovalId>,
) -> Result<PaymentAttemptDto, IpcError> {
    let (ctx, token) = services.context_with_approval(approval_token_id).await?;
    if let Err(e) = ctx.authorize_with_approval(pos_domain::Permission::CartVoid, attempt_id, None) {
        let result = Err(err(e));
        services.settle(token, &result);
        return result;
    }
    let result = match flow(&services).await {
        Ok(f) => f
            .reverse(&ctx, attempt_id, &reason)
            .await
            .map(|a| PaymentAttemptDto::from_attempt(&a))
            .map_err(err),
        Err(e) => Err(e),
    };
    services.settle(token, &result);
    result
}

/// The open cart's card attempts, oldest first (to resume after a restart).
#[tauri::command]
#[specta::specta]
pub async fn payment_attempts(
    services: State<'_, AppServices>,
    cart_id: CartId,
) -> Result<Vec<PaymentAttemptDto>, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::SaleComplete).map_err(err)?;
    let list = flow(&services)
        .await?
        .attempts(&ctx, cart_id)
        .await
        .map_err(err)?;
    Ok(list.iter().map(PaymentAttemptDto::from_attempt).collect())
}
