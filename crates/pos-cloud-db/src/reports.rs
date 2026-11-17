//! The consolidated sales report (E14-T05): rollups by day, register and tender over the
//! facts every register sent, and how complete they are per register.

use crate::TenantTx;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

/// Figures for one group (see `pos_sync::reports::SalesReportRow`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Figures {
    pub key: String,
    pub label: String,
    pub sales_count: i64,
    pub gross: i64,
    pub tax: i64,
    pub discount: i64,
    pub returns_count: i64,
    pub refunds: i64,
}

type RollupRow = (String, String, i64, i64, i64, i64, i64, i64);

fn figures(rows: Vec<RollupRow>) -> Vec<Figures> {
    rows.into_iter()
        .map(
            |(key, label, sales_count, gross, tax, discount, returns_count, refunds)| Figures {
                key,
                label,
                sales_count,
                gross,
                tax,
                discount,
                returns_count,
                refunds,
            },
        )
        .collect()
}

/// Per business date, from the daily rollup.
pub async fn by_day(
    tx: &mut TenantTx<'_>,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Figures>, sqlx::Error> {
    let rows: Vec<RollupRow> = sqlx::query_as(
        "SELECT to_char(business_date, 'YYYY-MM-DD'), to_char(business_date, 'YYYY-MM-DD'),
                sum(sales_count)::bigint, sum(sales_total_minor)::bigint, sum(tax_minor)::bigint,
                sum(discount_minor)::bigint, sum(returns_count)::bigint, sum(refunds_total_minor)::bigint
           FROM daily_sales_rollup
          WHERE business_id = $1 AND business_date BETWEEN $2 AND $3
          GROUP BY business_date
          ORDER BY business_date",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(from)
    .bind(to)
    .fetch_all(tx.conn())
    .await?;
    Ok(figures(rows))
}

/// Per register, from the daily rollup; labelled with the register's code.
pub async fn by_register(
    tx: &mut TenantTx<'_>,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Figures>, sqlx::Error> {
    let rows: Vec<RollupRow> = sqlx::query_as(
        "SELECT d.register_id::text, coalesce(min(r.code), d.register_id::text),
                sum(d.sales_count)::bigint, sum(d.sales_total_minor)::bigint, sum(d.tax_minor)::bigint,
                sum(d.discount_minor)::bigint, sum(d.returns_count)::bigint, sum(d.refunds_total_minor)::bigint
           FROM daily_sales_rollup d
           LEFT JOIN registers r ON r.business_id = d.business_id AND r.register_id = d.register_id
          WHERE d.business_id = $1 AND d.business_date BETWEEN $2 AND $3
          GROUP BY d.register_id
          ORDER BY 2",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(from)
    .bind(to)
    .fetch_all(tx.conn())
    .await?;
    Ok(figures(rows))
}

/// Per tender kind: confirmed tenders of live sales made in the range, and confirmed
/// refunds of returns made in the range, by the kind they were paid back with.
pub async fn by_tender(
    tx: &mut TenantTx<'_>,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Figures>, sqlx::Error> {
    let rows: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
        "WITH taken AS (
           SELECT t.kind, count(*)::bigint AS n, sum(t.amount_minor)::bigint AS amount
             FROM tenders t
             JOIN sales s ON s.business_id = t.business_id AND s.sale_id = t.sale_id
            WHERE t.business_id = $1 AND s.mode = 'live' AND t.status = 'confirmed'
              AND s.business_date BETWEEN $2 AND $3
            GROUP BY t.kind
         ), refunded AS (
           SELECT f.kind, count(*)::bigint AS n, sum(f.amount_minor)::bigint AS amount
             FROM refunds f
             JOIN returns x ON x.business_id = f.business_id AND x.return_id = f.return_id
             JOIN sales s ON s.business_id = x.business_id AND s.sale_id = x.original_sale_id
            WHERE f.business_id = $1 AND s.mode = 'live' AND f.status = 'confirmed'
              AND x.business_date BETWEEN $2 AND $3
            GROUP BY f.kind
         )
         SELECT coalesce(a.kind, b.kind), coalesce(a.n, 0), coalesce(a.amount, 0),
                coalesce(b.n, 0), coalesce(b.amount, 0)
           FROM taken a FULL JOIN refunded b ON a.kind = b.kind
          ORDER BY 1",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(from)
    .bind(to)
    .fetch_all(tx.conn())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(kind, n, amount, returns, refunds)| Figures {
            key: kind.clone(),
            label: kind,
            sales_count: n,
            gross: amount,
            returns_count: returns,
            refunds,
            ..Figures::default()
        })
        .collect())
}

/// Where one register stands with the cloud.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterState {
    pub register_id: Uuid,
    pub code: String,
    pub name: String,
    /// The later of its last accepted push and its last report of what it holds.
    pub last_ack_at: Option<DateTime<Utc>>,
    /// What it last said it holds that the cloud has not accepted (`None`: never said).
    pub unsent: Option<i64>,
}

/// The registers with an active device, and where each stands; `now` is the database's
/// clock (so "current" never depends on two clocks).
pub async fn completeness(tx: &mut TenantTx<'_>) -> Result<(DateTime<Utc>, Vec<RegisterState>), sqlx::Error> {
    type Row = (Uuid, String, String, Option<DateTime<Utc>>, Option<i64>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT r.register_id, r.code, r.name,
                greatest(s.last_push_at, s.unsent_reported_at), s.unsent_reported
           FROM registers r
           LEFT JOIN register_sync_state s ON s.business_id = r.business_id AND s.register_id = r.register_id
          WHERE r.business_id = $1
            AND EXISTS (SELECT 1 FROM devices d
                         WHERE d.business_id = r.business_id AND d.register_id = r.register_id
                           AND d.status = 'active')
          ORDER BY r.code",
    )
    .bind(*tx.business_id().as_uuid())
    .fetch_all(tx.conn())
    .await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()").fetch_one(tx.conn()).await?;
    Ok((
        now,
        rows.into_iter()
            .map(|(register_id, code, name, last_ack_at, unsent)| RegisterState {
                register_id,
                code,
                name,
                last_ack_at,
                unsent,
            })
            .collect(),
    ))
}

/// Records what a register says it still holds (sent with each pull).
pub async fn record_unsent(tx: &mut TenantTx<'_>, register_id: Uuid, unsent: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO register_sync_state (business_id, register_id, unsent_reported, unsent_reported_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (business_id, register_id) DO UPDATE
            SET unsent_reported = EXCLUDED.unsent_reported, unsent_reported_at = EXCLUDED.unsent_reported_at",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(register_id)
    .bind(unsent.max(0))
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// The business's currency (code, exponent).
pub async fn currency(tx: &mut TenantTx<'_>) -> Result<(String, i16), sqlx::Error> {
    sqlx::query_as("SELECT currency_code::text, currency_exponent FROM businesses WHERE business_id = $1")
        .bind(*tx.business_id().as_uuid())
        .fetch_one(tx.conn())
        .await
}
