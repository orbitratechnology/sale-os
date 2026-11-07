//! The audit viewer's reads (E15-T01).

use crate::SqliteStore;
use pos_application::audit_log::{AuditPage, AuditQueries, AuditQuery, AuditRow};
use pos_application::queries::QueryError;
use pos_domain::Timestamp;
use sqlx::sqlite::SqliteArguments;
use sqlx::{Arguments, Row};

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

#[async_trait::async_trait]
impl AuditQueries for SqliteStore {
    async fn list_audit(&self, f: &AuditQuery) -> Result<AuditPage, QueryError> {
        let mut sql = String::from(
            "SELECT a.audit_id, a.occurred_at, a.action, u.display_name, p.display_name, a.entity_type, a.entity_id,
               CASE a.entity_type
                 WHEN 'sale' THEN (SELECT receipt_number FROM sales WHERE sale_id = a.entity_id)
                 WHEN 'return' THEN (SELECT return_number FROM returns WHERE return_id = a.entity_id)
                 WHEN 'inventory_document' THEN (SELECT document_no FROM inventory_documents WHERE document_id = a.entity_id)
                 WHEN 'cash_session' THEN (SELECT session_number FROM cash_sessions WHERE cash_session_id = a.entity_id)
                 WHEN 'staff_user' THEN (SELECT display_name FROM staff_users WHERE user_id = a.entity_id)
                 WHEN 'product' THEN (SELECT name FROM products WHERE product_id = a.entity_id)
               END,
               a.metadata_json, b.timezone
             FROM audit_events a
             JOIN businesses b ON b.business_id = a.business_id
             LEFT JOIN staff_users u ON u.user_id = a.actor_user_id
             LEFT JOIN staff_users p ON p.user_id = a.approver_user_id
             WHERE 1 = 1",
        );
        let mut args = SqliteArguments::default();
        if let Some(action) = &f.action {
            if let Some(prefix) = action.strip_suffix('.') {
                sql.push_str(" AND a.action LIKE ? ESCAPE '\\'");
                args.add(format!("{}.%", prefix.replace('%', "\\%").replace('_', "\\_")))
                    .map_err(q)?;
            } else {
                sql.push_str(" AND a.action = ?");
                args.add(action.clone()).map_err(q)?;
            }
        }
        if let Some(u) = f.user {
            sql.push_str(" AND (a.actor_user_id = ? OR a.approver_user_id = ?)");
            args.add(u.to_string()).map_err(q)?;
            args.add(u.to_string()).map_err(q)?;
        }
        if let Some(from) = f.from {
            sql.push_str(" AND a.occurred_at >= ?");
            args.add(from.as_millis()).map_err(q)?;
        }
        if let Some(until) = f.until {
            sql.push_str(" AND a.occurred_at < ?");
            args.add(until.as_millis()).map_err(q)?;
        }
        if let Some((at, id)) = &f.before {
            sql.push_str(" AND (a.occurred_at, a.audit_id) < (?, ?)");
            args.add(*at).map_err(q)?;
            args.add(id.clone()).map_err(q)?;
        }
        sql.push_str(" ORDER BY a.occurred_at DESC, a.audit_id DESC LIMIT ?");
        args.add(i64::from(f.limit) + 1).map_err(q)?;
        let rows = sqlx::query_with(&sql, args)
            .fetch_all(self.db().readers())
            .await
            .map_err(q)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let at: i64 = r.try_get(1).map_err(q)?;
            let zone: String = r.try_get(9).map_err(q)?;
            out.push(AuditRow {
                audit_id: r.try_get(0).map_err(q)?,
                occurred_at: Timestamp::from_millis(at),
                local_time: pos_domain::time::local_date_time(Timestamp::from_millis(at), &zone)
                    .unwrap_or_default(),
                action: r.try_get(2).map_err(q)?,
                actor_name: r.try_get(3).map_err(q)?,
                approver_name: r.try_get(4).map_err(q)?,
                entity_type: r.try_get(5).map_err(q)?,
                entity_id: r.try_get(6).map_err(q)?,
                entity_label: r.try_get(7).map_err(q)?,
                metadata: r.try_get(8).map_err(q)?,
            });
        }
        let more = out.len() > f.limit as usize;
        out.truncate(f.limit as usize);
        let next = if more {
            out.last()
                .map(|r| format!("{}|{}", r.occurred_at.as_millis(), r.audit_id))
        } else {
            None
        };
        Ok(AuditPage { rows: out, next })
    }
}
