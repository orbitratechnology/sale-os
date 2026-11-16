//! Link persistence (E17-T08): what the wizard needs to know, its resumable progress, and a
//! join's staged bootstrap and the one transaction that replaces this register's business
//! with the joined one.

use crate::SqliteStore;
use crate::sync_store::{Here, apply_master};
use pos_application::link::{LinkFacts, LinkProgress, LinkStore, NewCloudBusiness};
use pos_application::queries::QueryError;
use pos_application::records::RegisterRecord;
use pos_application::sync::InboundReport;
use pos_application::{AppliedWriteSet, OutboxDraft, WriteContext, WriteOp};
use pos_domain::{BusinessId, LocationId, RegisterId, Timestamp, UuidV7Generator};
use pos_sync::pull::{BootstrapEntity, BootstrapPage};
use sqlx::Row;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

/// Tables a join leaves alone: the schema's own, seed data, and sync bookkeeping.
const KEEP: &[&str] = &["_sqlx_migrations", "units", "sync_state", "bootstrap_staging"];

#[async_trait::async_trait]
impl LinkStore for SqliteStore {
    async fn link_facts(&self) -> Result<LinkFacts, QueryError> {
        let r = self.db().readers();
        let row = sqlx::query(
            "SELECT b.business_id, b.name, b.country_code, b.timezone, b.currency_code, b.currency_exponent,
                    b.prices_include_tax, l.location_id, l.name, g.register_id, g.name, g.code, d.installation_id
               FROM device_identity d
               JOIN businesses b ON b.business_id = d.business_id
               JOIN registers g ON g.register_id = d.register_id
               JOIN locations l ON l.location_id = g.location_id
              WHERE d.id = 1",
        )
        .fetch_one(r)
        .await
        .map_err(q)?;
        let id = |i: usize| -> Result<uuid::Uuid, QueryError> {
            row.try_get::<String, _>(i).map_err(q)?.parse().map_err(q)
        };
        let state: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT link_state, link_progress FROM sync_state WHERE id = 1")
                .fetch_optional(r)
                .await
                .map_err(q)?;
        let (link_state, progress) = state.unwrap_or_else(|| ("unlinked".into(), None));
        let live_sales: i64 = sqlx::query_scalar("SELECT count(*) FROM sales WHERE mode = 'live'")
            .fetch_one(r)
            .await
            .map_err(q)?;
        let products: i64 = sqlx::query_scalar("SELECT count(*) FROM products")
            .fetch_one(r)
            .await
            .map_err(q)?;
        Ok(LinkFacts {
            link_state,
            business: NewCloudBusiness {
                business_id: id(0)?,
                name: row.try_get(1).map_err(q)?,
                country_code: row.try_get(2).map_err(q)?,
                timezone: row.try_get(3).map_err(q)?,
                currency_code: row.try_get(4).map_err(q)?,
                currency_exponent: u8::try_from(row.try_get::<i64, _>(5).map_err(q)?).map_err(q)?,
                prices_include_tax: row.try_get(6).map_err(q)?,
            },
            location_id: id(7)?,
            location_name: row.try_get(8).map_err(q)?,
            register_id: id(9)?,
            register_name: row.try_get(10).map_err(q)?,
            register_code: row.try_get(11).map_err(q)?,
            installation_id: id(12)?,
            live_sales,
            products,
            progress: progress
                .map(|p| serde_json::from_str(&p))
                .transpose()
                .map_err(q)?,
        })
    }

    async fn save_progress(&self, progress: &LinkProgress) -> Result<(), QueryError> {
        sqlx::query(
            "INSERT INTO sync_state (id, link_state, link_progress) VALUES (1, 'linking', ?1)
             ON CONFLICT (id) DO UPDATE SET link_state = 'linking', link_progress = ?1",
        )
        .bind(serde_json::to_string(progress).map_err(q)?)
        .execute(self.db().writer())
        .await
        .map(|_| ())
        .map_err(q)
    }

    async fn finish_create(&self, device_id: uuid::Uuid) -> Result<(), QueryError> {
        sqlx::query(
            "INSERT INTO sync_state (id, link_state, device_id) VALUES (1, 'linked', ?1)
             ON CONFLICT (id) DO UPDATE SET link_state = 'linked', device_id = ?1, link_progress = NULL,
               last_error_code = NULL",
        )
        .bind(device_id.to_string())
        .execute(self.db().writer())
        .await
        .map(|_| ())
        .map_err(q)
    }

    async fn stage_bootstrap(&self, page: &BootstrapPage) -> Result<(), QueryError> {
        let mut tx = self
            .db()
            .writer()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(q)?;
        for e in &page.entities {
            sqlx::query("INSERT INTO bootstrap_staging (entity_json) VALUES (?)")
                .bind(serde_json::to_string(e).map_err(q)?)
                .execute(&mut *tx)
                .await
                .map_err(q)?;
        }
        sqlx::query("UPDATE sync_state SET bootstrap_page = ? WHERE id = 1")
            .bind(&page.next_page)
            .execute(&mut *tx)
            .await
            .map_err(q)?;
        tx.commit().await.map_err(q)
    }

    async fn staged_page(&self) -> Result<Option<String>, QueryError> {
        sqlx::query_scalar("SELECT bootstrap_page FROM sync_state WHERE id = 1")
            .fetch_optional(self.db().readers())
            .await
            .map(Option::flatten)
            .map_err(q)
    }

    async fn finish_join(&self, p: &LinkProgress, at: Timestamp) -> Result<InboundReport, QueryError> {
        let (Some(device_id), Some(code), Some(location), Some(cursor)) = (
            p.device_id,
            p.register_code.clone(),
            p.location_id,
            p.staged_cursor.clone(),
        ) else {
            return Err(QueryError("the join is not ready to apply".into()));
        };
        let mut tx = self
            .db()
            .writer()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(q)?;
        let staged: Vec<String> =
            sqlx::query_scalar("SELECT entity_json FROM bootstrap_staging ORDER BY position")
                .fetch_all(&mut *tx)
                .await
                .map_err(q)?;
        let (installation, mode): (String, String) =
            sqlx::query_as("SELECT installation_id, mode FROM device_identity WHERE id = 1")
                .fetch_one(&mut *tx)
                .await
                .map_err(q)?;
        // References are checked at commit: the wipe and the new rows meet in between.
        sqlx::query("PRAGMA defer_foreign_keys = ON")
            .execute(&mut *tx)
            .await
            .map_err(q)?;
        wipe(&mut tx).await?;

        let business = BusinessId::from_uuid(p.business_id);
        let ctx = WriteContext {
            business_id: business,
            location_id: LocationId::from_uuid(location),
            register_id: RegisterId::from_uuid(p.register_id),
            actor: None,
            occurred_at: at,
        };
        let here = Here {
            register: p.register_id,
            ctx: ctx.clone(),
            ack: 0,
        };
        let mut report = InboundReport::default();
        for json in staged {
            let e: BootstrapEntity = serde_json::from_str(&json).map_err(q)?;
            let o = apply_master(
                &mut tx,
                &here,
                &e.aggregate_type,
                &e.aggregate_id,
                &e.event_type,
                e.schema_version,
                e.version,
                &e.snapshot,
            )
            .await?;
            report.add(o);
        }
        // This register, as the cloud placed it; it announces itself with its first event.
        let register = RegisterRecord {
            register_id: RegisterId::from_uuid(p.register_id),
            business_id: business,
            location_id: LocationId::from_uuid(location),
            name: p.register_name.clone(),
            code,
            receipt_prefix: String::new(),
            retired: false,
            version: 1,
            created_at: at,
            updated_at: at,
        };
        let draft = OutboxDraft::register_upserted(&UuidV7Generator, &register).map_err(q)?;
        let mut applied = AppliedWriteSet::default();
        let mut st = crate::store::Statements::unlimited();
        crate::store::run_ops(
            &mut tx,
            &ctx,
            &[WriteOp::PutRegister {
                record: register,
                expected_version: None,
            }],
            &mut st,
            &mut applied,
        )
        .await
        .map_err(q)?;
        crate::store::write_outbox(&mut tx, &ctx, &[draft], &mut st, &mut applied)
            .await
            .map_err(q)?;
        sqlx::query(
            "INSERT INTO device_identity (id, installation_id, business_id, register_id, mode, created_at)
             VALUES (1, ?, ?, ?, ?, ?)",
        )
        .bind(installation)
        .bind(p.business_id.to_string())
        .bind(p.register_id.to_string())
        .bind(mode)
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(q)?;
        sqlx::query(
            "UPDATE sync_state SET link_state = 'linked', device_id = ?, last_ack_sequence = 0, last_pull_cursor = ?,
               last_pull_ok_at = ?, last_push_ok_at = NULL, bootstrap_page = NULL, link_progress = NULL,
               last_error_code = NULL WHERE id = 1",
        )
        .bind(device_id.to_string())
        .bind(cursor)
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(q)?;
        sqlx::query("DELETE FROM bootstrap_staging")
            .execute(&mut *tx)
            .await
            .map_err(q)?;
        tx.commit().await.map_err(q)?;
        Ok(report)
    }

    async fn mark_revoked(&self) -> Result<(), QueryError> {
        sqlx::query(
            "INSERT INTO sync_state (id, link_state, last_error_code) VALUES (1, 'revoked', 'SYNC_DEVICE_REVOKED')
             ON CONFLICT (id) DO UPDATE SET link_state = 'revoked', last_error_code = 'SYNC_DEVICE_REVOKED'",
        )
        .execute(self.db().writer())
        .await
        .map(|_| ())
        .map_err(q)
    }
}

/// Empties every business table. Their append-only triggers are lifted for the statement
/// and put back as they were, inside the same transaction.
async fn wipe(tx: &mut sqlx::SqliteConnection) -> Result<(), QueryError> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type IN ('table') AND name NOT LIKE 'sqlite_%'
           AND name NOT LIKE 'product_search_%'",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(q)?;
    let tables: Vec<String> = tables
        .into_iter()
        .filter(|t| !KEEP.contains(&t.as_str()))
        .collect();
    let triggers: Vec<(String, String)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND sql IS NOT NULL")
            .fetch_all(&mut *tx)
            .await
            .map_err(q)?;
    for (name, _) in &triggers {
        sqlx::query(&format!("DROP TRIGGER \"{}\"", name.replace('"', "\"\"")))
            .execute(&mut *tx)
            .await
            .map_err(q)?;
    }
    for t in &tables {
        sqlx::query(&format!("DELETE FROM \"{}\"", t.replace('"', "\"\"")))
            .execute(&mut *tx)
            .await
            .map_err(q)?;
    }
    for (_, sql) in &triggers {
        sqlx::query(sql).execute(&mut *tx).await.map_err(q)?;
    }
    Ok(())
}
