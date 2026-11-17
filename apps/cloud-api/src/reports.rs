//! `GET /v1/reports/sales` (E14-T05, REP-008 minimal): the consolidated sales report across
//! every register, for the owner's account or for a linked register, with how complete it
//! is per register (PRD §15).

use crate::auth::UserPrincipal;
use crate::devices::{DevicePrincipal, internal, pool};
use crate::extract::ApiQuery;
use crate::{AppState, Problem};
use axum::extract::{FromRequestParts, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use pos_cloud_db::devices::is_member;
use pos_cloud_db::reports::{self as db, Figures, RegisterState};
use pos_cloud_db::{Principal, TenantTx};
use pos_domain::{BusinessId, ErrorCode};
use pos_sync::reports::{
    CURRENT_WITHIN_SECS, Completeness, GroupBy, MAX_RANGE_DAYS, RegisterCompleteness, SalesReport,
    SalesReportRow,
};
use serde::Deserialize;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route("/reports/sales", get(sales))
}

/// Whoever may read a business's reports: one of its linked registers (the business comes
/// from the token) or a member's account (the business is named by `businessId`).
pub enum Caller {
    Device(DevicePrincipal),
    User(UserPrincipal),
}

impl FromRequestParts<AppState> for Caller {
    type Rejection = Problem;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        match DevicePrincipal::from_request_parts(parts, state).await {
            Ok(d) => Ok(Self::Device(d)),
            // A revoked device stays refused; anything else may be an account token.
            Err(e) if e.code == ErrorCode::SyncDeviceRevoked => Err(e),
            Err(e) => UserPrincipal::from_request_parts(parts, state)
                .await
                .map(Self::User)
                .map_err(|_| e),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SalesQuery {
    from: String,
    to: String,
    group_by: GroupBy,
    business_id: Option<Uuid>,
}

fn not_found() -> Problem {
    Problem::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, "Not found", None)
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn row(f: Figures) -> SalesReportRow {
    SalesReportRow {
        net_minor: f.gross - f.refunds,
        key: f.key,
        label: f.label,
        sales_count: f.sales_count,
        gross_minor: f.gross,
        tax_minor: f.tax,
        discount_minor: f.discount,
        returns_count: f.returns_count,
        refunds_minor: f.refunds,
    }
}

/// A register is current when it confirmed its state within five minutes of `now`, and
/// then held nothing the cloud had not accepted. A register that never reported what it
/// holds (an app from before E14-T05) is never current.
pub fn register_completeness(now: DateTime<Utc>, r: RegisterState) -> RegisterCompleteness {
    let pending_known = r.unsent.is_some_and(|n| n > 0);
    let recent = r
        .last_ack_at
        .is_some_and(|t| (now - t).num_seconds() <= CURRENT_WITHIN_SECS);
    RegisterCompleteness {
        register_id: r.register_id,
        code: r.code,
        name: r.name,
        last_ack_at: r.last_ack_at.map(rfc3339),
        pending_known,
        current: recent && r.unsent == Some(0),
    }
}

async fn sales(
    State(state): State<AppState>,
    caller: Caller,
    ApiQuery(q): ApiQuery<SalesQuery>,
) -> Result<Json<SalesReport>, Problem> {
    let date = |s: &str, name: &str| {
        NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| Problem::validation(format!("{name} is not a YYYY-MM-DD date")))
    };
    let (from, to) = (date(&q.from, "from")?, date(&q.to, "to")?);
    if to < from {
        return Err(Problem::validation("to is before from"));
    }
    if (to - from).num_days() >= MAX_RANGE_DAYS {
        return Err(Problem::validation(format!(
            "at most {MAX_RANGE_DAYS} days at a time"
        )));
    }
    let business_id = match &caller {
        Caller::Device(d) => match q.business_id {
            Some(b) if b != *d.business_id.as_uuid() => return Err(not_found()),
            _ => d.business_id,
        },
        Caller::User(_) => BusinessId::from_uuid(
            q.business_id
                .ok_or_else(|| Problem::validation("businessId is required"))?,
        ),
    };
    let mut tx = TenantTx::begin(pool(&state)?, &Principal { business_id })
        .await
        .map_err(internal)?;
    if let Caller::User(u) = &caller
        && !is_member(&mut tx, u.account_id).await.map_err(internal)?
    {
        return Err(not_found());
    }
    let figures = match q.group_by {
        GroupBy::Day => db::by_day(&mut tx, from, to).await,
        GroupBy::Register => db::by_register(&mut tx, from, to).await,
        GroupBy::Tender => db::by_tender(&mut tx, from, to).await,
    }
    .map_err(internal)?;
    let (now, registers) = db::completeness(&mut tx).await.map_err(internal)?;
    let (currency_code, exponent) = db::currency(&mut tx).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(SalesReport {
        group_by: q.group_by,
        from: from.to_string(),
        to: to.to_string(),
        currency_code,
        currency_exponent: u8::try_from(exponent).unwrap_or(2),
        rows: figures.into_iter().map(row).collect(),
        as_of: rfc3339(now),
        completeness: Completeness {
            registers: registers
                .into_iter()
                .map(|r| register_completeness(now, r))
                .collect(),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn state(ago_secs: i64, unsent: Option<i64>, now: DateTime<Utc>) -> RegisterState {
        RegisterState {
            register_id: Uuid::nil(),
            code: "R01".into(),
            name: "Till".into(),
            last_ack_at: Some(now - chrono::Duration::seconds(ago_secs)),
            unsent,
        }
    }

    #[test]
    fn current_means_heard_from_within_five_minutes_holding_nothing() {
        let now = Utc.with_ymd_and_hms(2026, 11, 17, 10, 0, 0).unwrap();
        let c = |ago, unsent| register_completeness(now, state(ago, unsent, now));
        assert!(c(299, Some(0)).current);
        assert!(c(300, Some(0)).current);
        assert!(!c(301, Some(0)).current, "too long ago");
        let pending = c(10, Some(3));
        assert!(!pending.current && pending.pending_known);
        let unknown = c(10, None);
        assert!(
            !unknown.current && !unknown.pending_known,
            "never said what it holds"
        );
        let never = register_completeness(
            now,
            RegisterState {
                last_ack_at: None,
                ..state(0, Some(0), now)
            },
        );
        assert!(!never.current && never.last_ack_at.is_none());
    }
}
