//! Payment attempt reads (E10-T02).

use crate::SqliteStore;
use pos_application::payments::PaymentQueries;
use pos_application::queries::QueryError;
use pos_domain::payment::{
    Approval, AttemptStatus, CardApproval, PaymentAttempt, PaymentKind, StoredAttempt,
};
use pos_domain::{CartId, CurrencyCode, Money, PaymentAttemptId, RegisterId, Timestamp};
use sqlx::Row;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

const COLUMNS: &str = "attempt_id, cart_id, kind, provider, amount_minor, currency_code, status, approval_source,
   provider_ref, auth_code, card_brand, masked_pan, status_reason, tender_id, created_by, created_at, updated_at
   FROM payment_attempts";

fn attempt(r: &sqlx::sqlite::SqliteRow) -> Result<PaymentAttempt, QueryError> {
    let text = |i: usize| -> Result<String, QueryError> { r.try_get::<String, _>(i).map_err(q) };
    let opt =
        |i: usize| -> Result<Option<String>, QueryError> { r.try_get::<Option<String>, _>(i).map_err(q) };
    let provider_ref = opt(8)?;
    let approval = match opt(7)?.as_deref() {
        None => None,
        Some("provider") => Some(Approval::Provider(
            CardApproval::from_provider(
                provider_ref.as_deref().unwrap_or_default(),
                opt(9)?.as_deref(),
                opt(10)?.as_deref(),
                opt(11)?.as_deref(),
            )
            .map_err(q)?,
        )),
        Some("operator") => Some(Approval::Operator {
            reference: provider_ref,
        }),
        Some(other) => return Err(QueryError(format!("unknown approval source {other:?}"))),
    };
    PaymentAttempt::restore(StoredAttempt {
        id: text(0)?.parse().map_err(q)?,
        cart_id: text(1)?.parse().map_err(q)?,
        kind: PaymentKind::parse(&text(2)?).ok_or_else(|| q("unknown payment kind"))?,
        provider: text(3)?,
        amount: Money::from_minor(r.try_get(4).map_err(q)?),
        currency: CurrencyCode::new(&text(5)?).map_err(q)?,
        status: AttemptStatus::parse(&text(6)?).ok_or_else(|| q("unknown payment status"))?,
        approval,
        status_reason: opt(12)?,
        tender_id: opt(13)?.map(|t| t.parse()).transpose().map_err(q)?,
        created_by: text(14)?.parse().map_err(q)?,
        created_at: Timestamp::from_millis(r.try_get(15).map_err(q)?),
        updated_at: Timestamp::from_millis(r.try_get(16).map_err(q)?),
    })
    .map_err(q)
}

#[async_trait::async_trait]
impl PaymentQueries for SqliteStore {
    async fn payment_attempt(&self, id: PaymentAttemptId) -> Result<Option<PaymentAttempt>, QueryError> {
        sqlx::query(&format!("SELECT {COLUMNS} WHERE attempt_id = ?"))
            .bind(id.to_string())
            .fetch_optional(self.db().readers())
            .await
            .map_err(q)?
            .as_ref()
            .map(attempt)
            .transpose()
    }

    async fn cart_payment_attempts(&self, cart: CartId) -> Result<Vec<PaymentAttempt>, QueryError> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} WHERE cart_id = ? ORDER BY created_at, attempt_id"
        ))
        .bind(cart.to_string())
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?
        .iter()
        .map(attempt)
        .collect()
    }

    async fn open_payment_attempts(&self, register: RegisterId) -> Result<Vec<PaymentAttempt>, QueryError> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} WHERE register_id = ? AND status IN ('pending','reconciliation_required')
             ORDER BY created_at, attempt_id"
        ))
        .bind(register.to_string())
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?
        .iter()
        .map(attempt)
        .collect()
    }
}
