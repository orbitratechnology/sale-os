//! `LocalStore` for SQLite: the one place business writes happen (ADR-008).

use crate::LocalDb;
use pos_application::records::AggregateRef;
use pos_application::{
    AppliedWriteSet, ChangeNotice, LocalStore, WriteContext, WriteError, WriteOp, WriteSet,
};
use sqlx::{Sqlite, SqlitePool, Transaction};
use tokio::sync::broadcast;

/// The SQLite write path. Cheap to clone; clones share the writer connection.
#[derive(Clone)]
pub struct SqliteStore {
    db: LocalDb,
    notices: broadcast::Sender<ChangeNotice>,
    fail_at: Option<usize>,
}

impl SqliteStore {
    pub fn new(db: LocalDb) -> Self {
        let (notices, _) = broadcast::channel(256);
        Self {
            db,
            notices,
            fail_at: None,
        }
    }

    pub fn db(&self) -> &LocalDb {
        &self.db
    }

    /// Change notices published after each successful commit.
    pub fn subscribe(&self) -> broadcast::Receiver<ChangeNotice> {
        self.notices.subscribe()
    }

    /// Test hook: fail just before the `n`-th statement (0-based) of every apply, to prove
    /// that a failure at any point leaves nothing behind (E02-T03).
    #[doc(hidden)]
    pub fn with_fault_before_statement(mut self, n: usize) -> Self {
        self.fail_at = Some(n);
        self
    }
}

#[async_trait::async_trait]
impl LocalStore for SqliteStore {
    async fn apply(&self, write_set: WriteSet) -> Result<AppliedWriteSet, WriteError> {
        let notice = ChangeNotice {
            aggregates: write_set.outbox().iter().map(|e| e.aggregate.clone()).collect(),
            outbox_appended: !write_set.outbox().is_empty(),
        };
        let pool = self.db.writer().clone();
        let fail_at = self.fail_at;
        // Run detached: if the caller is cancelled (window closed, request dropped), the
        // transaction still commits or rolls back as a whole instead of being abandoned.
        let applied = tokio::spawn(async move { apply_in_transaction(&pool, &write_set, fail_at).await })
            .await
            .map_err(|e| WriteError::Io(format!("write task failed: {e}")))??;
        let _ = self.notices.send(notice);
        Ok(applied)
    }
}

struct Statements {
    next: usize,
    fail_at: Option<usize>,
}

impl Statements {
    fn step(&mut self) -> Result<(), WriteError> {
        let n = self.next;
        self.next += 1;
        if self.fail_at == Some(n) {
            return Err(WriteError::FaultInjected { statement: n });
        }
        Ok(())
    }
}

async fn apply_in_transaction(
    pool: &SqlitePool,
    ws: &WriteSet,
    fail_at: Option<usize>,
) -> Result<AppliedWriteSet, WriteError> {
    // IMMEDIATE takes the write lock up front: no lock upgrades, no deadlocks (ADR-007).
    // Dropping `tx` without commit rolls back.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let mut st = Statements { next: 0, fail_at };
    let ctx = ws.context();
    let mut applied = AppliedWriteSet::default();

    for op in ws.ops() {
        match op {
            WriteOp::AllocateNumber { counter, slot } => {
                st.step()?;
                let n = allocate(&mut tx, ctx, counter.column_name()).await?;
                applied.numbers.insert(*slot, n);
            }
            WriteOp::PutBusiness {
                record: r,
                expected_version,
            } => {
                st.step()?;
                check_version(
                    &mut tx,
                    "businesses",
                    "business_id",
                    &r.business_id.to_string(),
                    *expected_version,
                    r.aggregate(),
                )
                .await?;
                st.step()?;
                let address = r.address.to_string();
                // The version check above decides create vs update; one upsert covers both and
                // never touches created_at on update.
                sqlx::query(
                    "INSERT INTO businesses (business_id, name, legal_name, tax_registration_no, address_json, phone, email,
                       logo_path, country_code, timezone, currency_code, currency_exponent, prices_include_tax, version,
                       sync_status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 'pending', ?15, ?16)
                     ON CONFLICT (business_id) DO UPDATE SET name = ?2, legal_name = ?3, tax_registration_no = ?4,
                       address_json = ?5, phone = ?6, email = ?7, logo_path = ?8, country_code = ?9, timezone = ?10,
                       currency_code = ?11, currency_exponent = ?12, prices_include_tax = ?13, version = ?14,
                       sync_status = 'pending', updated_at = ?16",
                )
                .bind(r.business_id.to_string())
                    .bind(&r.name)
                    .bind(&r.legal_name)
                    .bind(&r.tax_registration_no)
                    .bind(address)
                    .bind(&r.phone)
                    .bind(&r.email)
                    .bind(&r.logo_path)
                    .bind(&r.country_code)
                    .bind(&r.timezone)
                    .bind(r.currency.code.to_string())
                    .bind(i64::from(r.currency.exponent))
                    .bind(r.prices_include_tax)
                    .bind(as_i64(r.version)?)
                    .bind(r.created_at.as_millis())
                    .bind(r.updated_at.as_millis())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
            }
            WriteOp::PutLocation {
                record: r,
                expected_version,
            } => {
                st.step()?;
                check_version(
                    &mut tx,
                    "locations",
                    "location_id",
                    &r.location_id.to_string(),
                    *expected_version,
                    r.aggregate(),
                )
                .await?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO locations (location_id, business_id, name, address_json, status, version, sync_status,
                       created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8)
                     ON CONFLICT (location_id) DO UPDATE SET name = ?3, address_json = ?4, status = ?5, version = ?6,
                       sync_status = 'pending', updated_at = ?8",
                )
                .bind(r.location_id.to_string())
                    .bind(r.business_id.to_string())
                    .bind(&r.name)
                    .bind(r.address.to_string())
                    .bind(if r.archived { "archived" } else { "active" })
                    .bind(as_i64(r.version)?)
                    .bind(r.created_at.as_millis())
                    .bind(r.updated_at.as_millis())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
            }
            WriteOp::PutRegister {
                record: r,
                expected_version,
            } => {
                st.step()?;
                check_version(
                    &mut tx,
                    "registers",
                    "register_id",
                    &r.register_id.to_string(),
                    *expected_version,
                    r.aggregate(),
                )
                .await?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO registers (register_id, business_id, location_id, name, code, receipt_prefix, status, version,
                       sync_status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?10)
                     ON CONFLICT (register_id) DO UPDATE SET location_id = ?3, name = ?4, code = ?5, receipt_prefix = ?6,
                       status = ?7, version = ?8, sync_status = 'pending', updated_at = ?10",
                )
                .bind(r.register_id.to_string())
                    .bind(r.business_id.to_string())
                    .bind(r.location_id.to_string())
                    .bind(&r.name)
                    .bind(&r.code)
                    .bind(&r.receipt_prefix)
                    .bind(if r.retired { "retired" } else { "active" })
                    .bind(as_i64(r.version)?)
                    .bind(r.created_at.as_millis())
                    .bind(r.updated_at.as_millis())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
            }
            WriteOp::PutSetting {
                record: r,
                expected_version,
            } => {
                st.step()?;
                let version: Option<i64> = sqlx::query_scalar(
                    "SELECT version FROM business_settings WHERE business_id = ? AND key = ?",
                )
                .bind(r.business_id.to_string())
                .bind(&r.key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                ensure_version(version, *expected_version, r.aggregate())?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO business_settings (business_id, key, value_json, version, sync_status, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 'pending', ?5)
                     ON CONFLICT (business_id, key) DO UPDATE SET value_json = ?3, version = ?4, sync_status = 'pending',
                       updated_at = ?5",
                )
                .bind(r.business_id.to_string())
                .bind(&r.key)
                .bind(r.value.to_string())
                .bind(as_i64(r.version)?)
                .bind(r.updated_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
        }
    }

    for e in ws.outbox() {
        st.step()?;
        let seq = allocate(&mut tx, ctx, "outbox_seq").await?;
        st.step()?;
        sqlx::query(
            "INSERT INTO outbox_events (event_id, business_id, location_id, register_id, local_sequence, event_type,
               schema_version, aggregate_type, aggregate_id, base_version, occurred_at, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?11)",
        )
        .bind(e.event_id.to_string())
        .bind(ctx.business_id.to_string())
        .bind(ctx.location_id.to_string())
        .bind(ctx.register_id.to_string())
        .bind(as_i64(seq)?)
        .bind(e.event_type)
        .bind(i64::from(e.schema_version))
        .bind(e.aggregate.aggregate_type)
        .bind(&e.aggregate.aggregate_id)
        .bind(e.base_version.map(as_i64).transpose()?)
        .bind(ctx.occurred_at.as_millis())
        .bind(e.payload.to_string())
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        applied.outbox.push((e.event_id, seq));
    }

    for a in ws.audit() {
        st.step()?;
        sqlx::query(
            "INSERT INTO audit_events (audit_id, business_id, register_id, actor_user_id, approver_user_id, action,
               entity_type, entity_id, occurred_at, metadata_json, outbox_event_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(a.audit_id.to_string())
        .bind(ctx.business_id.to_string())
        .bind(ctx.register_id.to_string())
        .bind(ctx.actor.map(|u| u.to_string()))
        .bind(a.approver.map(|u| u.to_string()))
        .bind(a.action.as_str())
        .bind(a.entity_type)
        .bind(&a.entity_id)
        .bind(ctx.occurred_at.as_millis())
        .bind(a.metadata.to_string())
        .bind(a.outbox_event_id.map(|e| e.to_string()))
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
    }

    if cfg!(debug_assertions) && !ws.outbox().is_empty() {
        check_outbox_invariant(&mut tx, ctx).await?;
    }

    st.step()?;
    tx.commit().await.map_err(map_sqlx)?;
    Ok(applied)
}

/// Allocates the next value of a register-local counter inside the transaction.
async fn allocate(
    tx: &mut Transaction<'_, Sqlite>,
    ctx: &WriteContext,
    name: &str,
) -> Result<u64, WriteError> {
    let v: i64 = sqlx::query_scalar(
        "INSERT INTO register_counters (register_id, name, value) VALUES (?, ?, 1)
         ON CONFLICT (register_id, name) DO UPDATE SET value = value + 1
         RETURNING value",
    )
    .bind(ctx.register_id.to_string())
    .bind(name)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    u64::try_from(v).map_err(|_| WriteError::Corrupt(format!("negative counter {name}")))
}

async fn check_version(
    tx: &mut Transaction<'_, Sqlite>,
    table: &'static str,
    id_column: &'static str,
    id: &str,
    expected: Option<u64>,
    aggregate: AggregateRef,
) -> Result<(), WriteError> {
    let sql = format!("SELECT version FROM {table} WHERE {id_column} = ?");
    let version: Option<i64> = sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    ensure_version(version, expected, aggregate)
}

fn ensure_version(
    actual: Option<i64>,
    expected: Option<u64>,
    aggregate: AggregateRef,
) -> Result<(), WriteError> {
    let actual = actual.map(|v| u64::try_from(v).unwrap_or(0));
    if actual != expected {
        return Err(WriteError::VersionConflict {
            aggregate,
            expected,
            actual,
        });
    }
    Ok(())
}

async fn check_outbox_invariant(
    tx: &mut Transaction<'_, Sqlite>,
    ctx: &WriteContext,
) -> Result<(), WriteError> {
    let (counter, max): (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT (SELECT value FROM register_counters WHERE register_id = ?1 AND name = 'outbox_seq'),
                (SELECT MAX(local_sequence) FROM outbox_events WHERE register_id = ?1)",
    )
    .bind(ctx.register_id.to_string())
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if counter != max {
        return Err(WriteError::Corrupt(format!(
            "outbox_seq counter {counter:?} != max local_sequence {max:?}"
        )));
    }
    Ok(())
}

fn as_i64(v: u64) -> Result<i64, WriteError> {
    i64::try_from(v).map_err(|_| WriteError::Io(format!("value out of range: {v}")))
}

fn map_sqlx(e: sqlx::Error) -> WriteError {
    if let sqlx::Error::Database(db) = &e
        && let Some(code) = db.code().and_then(|c| c.parse::<i32>().ok())
    {
        match code {
            // SQLITE_CONSTRAINT_UNIQUE / _PRIMARYKEY
            2067 | 1555 => {
                return WriteError::UniqueViolation {
                    constraint: db.message().to_owned(),
                };
            }
            _ => {}
        }
        match code & 0xff {
            5 | 6 => return WriteError::Busy,
            11 | 26 => return WriteError::Corrupt(db.message().to_owned()),
            _ => {}
        }
    }
    WriteError::Io(e.to_string())
}
