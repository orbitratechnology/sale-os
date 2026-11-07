//! Cash session reads (E15-T02).

use crate::SqliteStore;
use pos_application::queries::{CashQueries, QueryError};
use pos_application::records::CashSessionRecord;
use pos_domain::cash::SessionTotals;
use pos_domain::{CashSessionId, Money, ReceiptId, RegisterId, Timestamp};
use sqlx::Row;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

const COLUMNS: &str = "c.cash_session_id, c.session_number, c.status, coalesce(o.display_name, ''), c.opened_at,
   c.opening_float_minor, c.business_date, k.display_name, c.closed_at, c.expected_cash_minor, c.counted_cash_minor,
   c.variance_minor, c.totals_json, c.note, b.timezone, b.currency_exponent
 FROM cash_sessions c
 JOIN businesses b ON b.business_id = c.business_id
 LEFT JOIN staff_users o ON o.user_id = c.opened_by
 LEFT JOIN staff_users k ON k.user_id = c.closed_by";

fn record(r: &sqlx::sqlite::SqliteRow) -> Result<CashSessionRecord, QueryError> {
    let zone: String = r.try_get(14).map_err(q)?;
    let local =
        |at: i64| pos_domain::time::local_date_time(Timestamp::from_millis(at), &zone).unwrap_or_default();
    let opened_at: i64 = r.try_get(4).map_err(q)?;
    let money = |i: usize| -> Result<Option<Money>, QueryError> {
        Ok(r.try_get::<Option<i64>, _>(i).map_err(q)?.map(Money::from_minor))
    };
    Ok(CashSessionRecord {
        id: r.try_get::<String, _>(0).map_err(q)?.parse().map_err(q)?,
        number: r.try_get(1).map_err(q)?,
        open: r.try_get::<String, _>(2).map_err(q)? == "open",
        opened_by_name: r.try_get(3).map_err(q)?,
        opened_at: Timestamp::from_millis(opened_at),
        opened_local: local(opened_at),
        opening_float: Money::from_minor(r.try_get(5).map_err(q)?),
        business_date: r.try_get(6).map_err(q)?,
        closed_by_name: r.try_get(7).map_err(q)?,
        closed_local: r.try_get::<Option<i64>, _>(8).map_err(q)?.map(local),
        expected: money(9)?,
        counted: money(10)?,
        variance: money(11)?,
        totals: r
            .try_get::<Option<String>, _>(12)
            .map_err(q)?
            .map(|j| serde_json::from_str::<SessionTotals>(&j))
            .transpose()
            .map_err(q)?,
        note: r.try_get(13).map_err(q)?,
        currency_exponent: u8::try_from(r.try_get::<i64, _>(15).map_err(q)?).map_err(q)?,
    })
}

#[async_trait::async_trait]
impl CashQueries for SqliteStore {
    async fn open_cash_session(&self, register: RegisterId) -> Result<Option<CashSessionRecord>, QueryError> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} WHERE c.register_id = ? AND c.status = 'open'"
        ))
        .bind(register.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?
        .as_ref()
        .map(record)
        .transpose()
    }

    async fn cash_session(&self, id: CashSessionId) -> Result<Option<CashSessionRecord>, QueryError> {
        sqlx::query(&format!("SELECT {COLUMNS} WHERE c.cash_session_id = ?"))
            .bind(id.to_string())
            .fetch_optional(self.db().readers())
            .await
            .map_err(q)?
            .as_ref()
            .map(record)
            .transpose()
    }

    async fn cash_session_totals(&self, id: CashSessionId) -> Result<SessionTotals, QueryError> {
        let mut c = self.db().readers().acquire().await.map_err(q)?;
        crate::cash::session_totals(&mut c, &id.to_string())
            .await
            .map_err(q)
    }

    async fn last_closed_cash_session(
        &self,
        register: RegisterId,
    ) -> Result<Option<CashSessionId>, QueryError> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT cash_session_id FROM cash_sessions WHERE register_id = ? AND status = 'closed'
             ORDER BY session_seq DESC LIMIT 1",
        )
        .bind(register.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        id.map(|i| i.parse().map_err(q)).transpose()
    }

    async fn cash_session_receipt(
        &self,
        id: CashSessionId,
    ) -> Result<Option<(ReceiptId, String)>, QueryError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT receipt_id, document_json FROM receipts WHERE source_type = 'cash_session' AND source_id = ?
             ORDER BY content_version DESC LIMIT 1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        row.map(|(r, d)| Ok((r.parse().map_err(q)?, d))).transpose()
    }
}
