//! Cash session totals (E15-T02), read the same way by the closing transaction and by the
//! "current session" query, so what the operator saw is what gets recorded.

use pos_domain::Money;
use pos_domain::cash::{SessionTotals, TenderTotal};
use pos_domain::settlement::TenderKind;
use sqlx::SqliteConnection;

fn by_kind(rows: Vec<(String, i64, i64)>) -> Result<Vec<TenderTotal>, sqlx::Error> {
    let mut out = rows
        .into_iter()
        .map(|(kind, count, amount)| {
            Ok(TenderTotal {
                kind: TenderKind::from_code(&kind)
                    .ok_or_else(|| sqlx::Error::Decode(format!("tender {kind:?}").into()))?,
                count: u32::try_from(count).map_err(|e| sqlx::Error::Decode(e.into()))?,
                amount: Money::from_minor(amount),
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    out.sort_by_key(|t| t.kind.as_str());
    Ok(out)
}

/// The totals of everything recorded in session `id` so far.
pub(crate) async fn session_totals(c: &mut SqliteConnection, id: &str) -> Result<SessionTotals, sqlx::Error> {
    let (sales_count, sales_total, tax_total, discount_total, rounding_total): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(sum(total_minor), 0), coalesce(sum(tax_total_minor), 0),
           coalesce(sum(line_discount_total_minor + order_discount_total_minor), 0), coalesce(sum(rounding_minor), 0)
         FROM sales WHERE cash_session_id = ?",
    )
    .bind(id)
    .fetch_one(&mut *c)
    .await?;
    let tenders: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT t.tender_type, count(*), sum(t.amount_minor) FROM tenders t JOIN sales s ON s.sale_id = t.sale_id
         WHERE s.cash_session_id = ? AND t.status = 'confirmed' GROUP BY t.tender_type",
    )
    .bind(id)
    .fetch_all(&mut *c)
    .await?;
    let (returns_count, refunds_total): (i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(sum(total_minor), 0) FROM returns WHERE cash_session_id = ?",
    )
    .bind(id)
    .fetch_one(&mut *c)
    .await?;
    let refunds: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT f.tender_type, count(*), sum(f.amount_minor) FROM refunds f JOIN returns r ON r.return_id = f.return_id
         WHERE r.cash_session_id = ? GROUP BY f.tender_type",
    )
    .bind(id)
    .fetch_all(&mut *c)
    .await?;
    let n = |v: i64| u32::try_from(v).map_err(|e| sqlx::Error::Decode(e.into()));
    Ok(SessionTotals {
        sales_count: n(sales_count)?,
        sales_total: Money::from_minor(sales_total),
        tax_total: Money::from_minor(tax_total),
        discount_total: Money::from_minor(discount_total),
        rounding_total: Money::from_minor(rounding_total),
        tenders: by_kind(tenders)?,
        returns_count: n(returns_count)?,
        refunds_total: Money::from_minor(refunds_total),
        refunds: by_kind(refunds)?,
    })
}

/// The register's open session, if any.
pub(crate) async fn open_session(
    c: &mut SqliteConnection,
    register: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT cash_session_id FROM cash_sessions WHERE register_id = ? AND status = 'open'")
        .bind(register)
        .fetch_optional(&mut *c)
        .await
}
