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
            WriteOp::PutStaff {
                record: r,
                expected_version,
            } => {
                st.step()?;
                check_version(
                    &mut tx,
                    "staff_users",
                    "user_id",
                    &r.user_id.to_string(),
                    *expected_version,
                    r.aggregate(),
                )
                .await?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO staff_users (user_id, business_id, display_name, role, pin_hash, pin_updated_at,
                       must_change_pin, status, version, sync_status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?11)
                     ON CONFLICT (user_id) DO UPDATE SET display_name = ?3, role = ?4, pin_hash = ?5,
                       pin_updated_at = ?6, must_change_pin = ?7, status = ?8, version = ?9, sync_status = 'pending',
                       updated_at = ?11",
                )
                .bind(r.user_id.to_string())
                .bind(r.business_id.to_string())
                .bind(&r.display_name)
                .bind(r.role.as_str())
                .bind(&r.pin_hash)
                .bind(r.pin_updated_at.as_millis())
                .bind(r.must_change_pin)
                .bind(if r.active { "active" } else { "deactivated" })
                .bind(as_i64(r.version)?)
                .bind(r.created_at.as_millis())
                .bind(r.updated_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::PutTaxRate {
                record: r,
                expected_version,
            } => {
                st.step()?;
                check_version(
                    &mut tx,
                    "tax_rates",
                    "tax_rate_id",
                    &r.tax_rate_id.to_string(),
                    *expected_version,
                    r.aggregate(),
                )
                .await?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO tax_rates (tax_rate_id, business_id, name, rate_ppm, effective_from, effective_to, status,
                       version, sync_status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?10)
                     ON CONFLICT (tax_rate_id) DO UPDATE SET name = ?3, rate_ppm = ?4, effective_from = ?5,
                       effective_to = ?6, status = ?7, version = ?8, sync_status = 'pending', updated_at = ?10",
                )
                .bind(r.tax_rate_id.to_string())
                .bind(r.business_id.to_string())
                .bind(&r.name)
                .bind(i64::from(r.rate.ppm()))
                .bind(r.effective_from.map(|d| d.to_string()))
                .bind(r.effective_to.map(|d| d.to_string()))
                .bind(if r.archived { "archived" } else { "active" })
                .bind(as_i64(r.version)?)
                .bind(r.created_at.as_millis())
                .bind(r.updated_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::PutCategory {
                record: r,
                expected_version,
            } => {
                st.step()?;
                check_version(
                    &mut tx,
                    "categories",
                    "category_id",
                    &r.category_id.to_string(),
                    *expected_version,
                    r.aggregate(),
                )
                .await?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO categories (category_id, business_id, parent_id, name, sort_order, status, version,
                       sync_status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9)
                     ON CONFLICT (category_id) DO UPDATE SET parent_id = ?3, name = ?4, sort_order = ?5, status = ?6,
                       version = ?7, sync_status = 'pending', updated_at = ?9",
                )
                .bind(r.category_id.to_string())
                .bind(r.business_id.to_string())
                .bind(r.parent_id.map(|p| p.to_string()))
                .bind(&r.name)
                .bind(r.sort_order)
                .bind(if r.archived { "archived" } else { "active" })
                .bind(as_i64(r.version)?)
                .bind(r.created_at.as_millis())
                .bind(r.updated_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::PutTaxCategory {
                record: r,
                expected_version,
            } => {
                st.step()?;
                check_version(
                    &mut tx,
                    "tax_categories",
                    "tax_category_id",
                    &r.tax_category_id.to_string(),
                    *expected_version,
                    r.aggregate(),
                )
                .await?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO tax_categories (tax_category_id, business_id, name, is_default, status, version,
                       sync_status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8)
                     ON CONFLICT (tax_category_id) DO UPDATE SET name = ?3, is_default = ?4, status = ?5, version = ?6,
                       sync_status = 'pending', updated_at = ?8",
                )
                .bind(r.tax_category_id.to_string())
                .bind(r.business_id.to_string())
                .bind(&r.name)
                .bind(r.is_default)
                .bind(if r.archived { "archived" } else { "active" })
                .bind(as_i64(r.version)?)
                .bind(r.created_at.as_millis())
                .bind(r.updated_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                // Rate membership is part of the aggregate: replace it wholesale.
                st.step()?;
                sqlx::query("DELETE FROM tax_category_rates WHERE tax_category_id = ?")
                    .bind(r.tax_category_id.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                for rate in &r.rate_ids {
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO tax_category_rates (tax_category_id, tax_rate_id) VALUES (?, ?)",
                    )
                    .bind(r.tax_category_id.to_string())
                    .bind(rate.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                }
            }
            WriteOp::SetDeviceIdentity(d) => {
                st.step()?;
                // Plain INSERT on the singleton row: a second setup can never overwrite it.
                sqlx::query(
                    "INSERT INTO device_identity (id, installation_id, business_id, register_id, mode, created_at)
                     VALUES (1, ?, ?, ?, ?, ?)",
                )
                .bind(&d.installation_id)
                .bind(d.business_id.to_string())
                .bind(d.register_id.to_string())
                .bind(d.mode.as_str())
                .bind(ctx.occurred_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::PutProduct {
                product: p,
                business_id,
                expected_version,
            } => {
                let pid = p.id.to_string();
                let bid = business_id.to_string();
                st.step()?;
                check_version(
                    &mut tx,
                    "products",
                    "product_id",
                    &pid,
                    *expected_version,
                    pos_application::write::product_aggregate(p),
                )
                .await?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO products (product_id, business_id, name, description, category_id, product_type, unit_code,
                       tax_category_id, quick_select, status, version, sync_status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending', ?12, ?12)
                     ON CONFLICT (product_id) DO UPDATE SET name = ?3, description = ?4, category_id = ?5,
                       product_type = ?6, unit_code = ?7, tax_category_id = ?8, quick_select = ?9, status = ?10,
                       version = ?11, sync_status = 'pending', updated_at = ?12",
                )
                .bind(&pid)
                .bind(&bid)
                .bind(&p.name)
                .bind(&p.description)
                .bind(p.category_id.map(|c| c.to_string()))
                .bind(p.product_type.as_str())
                .bind(p.unit.code())
                .bind(p.tax_category_id.to_string())
                .bind(p.quick_select)
                .bind(if p.archived { "archived" } else { "active" })
                .bind(as_i64(p.version)?)
                .bind(ctx.occurred_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                // Free every barcode of the product first, so moving a code between its
                // variants never trips the unique index; current codes are re-activated below.
                st.step()?;
                sqlx::query(
                    "UPDATE product_barcodes SET status = 'archived'
                     WHERE variant_id IN (SELECT variant_id FROM product_variants WHERE product_id = ?)",
                )
                .bind(&pid)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                st.step()?;
                sqlx::query("DELETE FROM product_search WHERE variant_id IN (SELECT variant_id FROM product_variants WHERE product_id = ?)")
                    .bind(&pid)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                // Defaults move between variants: clear them first (one default per product).
                st.step()?;
                sqlx::query("UPDATE product_variants SET is_default = 0 WHERE product_id = ?")
                    .bind(&pid)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                let category: Option<String> = match p.category_id {
                    Some(c) => sqlx::query_scalar("SELECT name FROM categories WHERE category_id = ?")
                        .bind(c.to_string())
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(map_sqlx)?,
                    None => None,
                };
                // Default variant last, so the unique "one default" index never sees two.
                let mut order: Vec<&pos_domain::catalog::Variant> = p.variants.iter().collect();
                order.sort_by_key(|v| v.is_default);
                for (pos, v) in order.into_iter().enumerate() {
                    let variant_active = !v.archived && !p.archived;
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO product_variants (variant_id, business_id, product_id, is_default, sku, options_json,
                           label, price_minor, cost_minor, track_stock, low_stock_threshold_milli, scan_qty_milli,
                           sort_order, status, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)
                         ON CONFLICT (variant_id) DO UPDATE SET is_default = ?4, sku = ?5, options_json = ?6, label = ?7,
                           price_minor = ?8, cost_minor = ?9, track_stock = ?10, low_stock_threshold_milli = ?11,
                           scan_qty_milli = ?12, sort_order = ?13, status = ?14, updated_at = ?15",
                    )
                    .bind(v.id.to_string())
                    .bind(&bid)
                    .bind(&pid)
                    .bind(v.is_default && !v.archived)
                    .bind(&v.sku)
                    .bind(serde_json::to_string(&v.options).map_err(|e| WriteError::Io(e.to_string()))?)
                    .bind(v.label())
                    .bind(v.price.minor())
                    .bind(v.cost.map(|c| c.minor()))
                    .bind(v.track_stock)
                    .bind(v.low_stock_threshold.map(|q| q.milli()))
                    .bind(v.scan_qty.milli())
                    .bind(i64::try_from(pos).unwrap_or(i64::MAX))
                    .bind(if variant_active { "active" } else { "archived" })
                    .bind(ctx.occurred_at.as_millis())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                    for b in &v.barcodes {
                        st.step()?;
                        sqlx::query(
                            "INSERT INTO product_barcodes (barcode_id, business_id, variant_id, value_normalized, value_raw,
                               symbology, qualifier, pack_qty_milli, status, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                             ON CONFLICT (barcode_id) DO UPDATE SET variant_id = ?3, value_normalized = ?4,
                               value_raw = ?5, symbology = ?6, qualifier = ?7, pack_qty_milli = ?8, status = ?9",
                        )
                        .bind(b.id.to_string())
                        .bind(&bid)
                        .bind(v.id.to_string())
                        .bind(&b.value)
                        .bind(&b.raw)
                        .bind(b.symbology.as_str())
                        .bind(b.qualifier.as_str())
                        .bind(b.pack_qty.milli())
                        .bind(if variant_active { "active" } else { "archived" })
                        .bind(ctx.occurred_at.as_millis())
                        .execute(&mut *tx)
                        .await
                        .map_err(map_sqlx)?;
                    }
                    if variant_active {
                        st.step()?;
                        let codes: Vec<&str> = v.barcodes.iter().map(|b| b.value.as_str()).collect();
                        sqlx::query(
                            "INSERT INTO product_search (variant_id, name, label, sku, barcodes, category) VALUES (?, ?, ?, ?, ?, ?)",
                        )
                        .bind(v.id.to_string())
                        .bind(&p.name)
                        .bind(v.label())
                        .bind(v.sku.clone().unwrap_or_default())
                        .bind(codes.join(" "))
                        .bind(category.clone().unwrap_or_default())
                        .execute(&mut *tx)
                        .await
                        .map_err(map_sqlx)?;
                    }
                }
            }
            WriteOp::PutCart {
                cart,
                opened_by,
                expected_revision,
            } => {
                use pos_domain::cart::{AddVia, CartStatus};
                let status = match cart.status {
                    CartStatus::Open => "open",
                    CartStatus::Parked => "parked",
                    CartStatus::Voided => "voided",
                    CartStatus::Completed => "completed",
                };
                let json = |v: &Option<pos_domain::pricing::Discount>| -> Result<Option<String>, WriteError> {
                    v.as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|e| WriteError::Io(e.to_string()))
                };
                let now = ctx.occurred_at.as_millis();
                let closed_at = (cart.status != CartStatus::Open).then_some(now);
                let stale = WriteError::CartRevisionMismatch {
                    expected: *expected_revision,
                };
                st.step()?;
                match expected_revision {
                    // Never saved: insert. Another open cart for this register (a concurrent
                    // first add) trips the unique index: the caller's view is stale.
                    None => {
                        sqlx::query(
                            "INSERT INTO carts (cart_id, business_id, register_id, status, opened_by, order_discount_json,
                               revision, void_reason, opened_at, updated_at, closed_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10)",
                        )
                        .bind(cart.id.to_string())
                        .bind(ctx.business_id.to_string())
                        .bind(ctx.register_id.to_string())
                        .bind(status)
                        .bind(opened_by.to_string())
                        .bind(json(&cart.order_discount)?)
                        .bind(as_i64(cart.revision)?)
                        .bind(&cart.void_reason)
                        .bind(now)
                        .bind(closed_at)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| match map_sqlx(e) {
                            WriteError::UniqueViolation { .. } => stale.clone(),
                            other => other,
                        })?;
                    }
                    // Compare-and-set on the revision the change was based on.
                    Some(base) => {
                        let done = sqlx::query(
                            "UPDATE carts SET status = ?1, order_discount_json = ?2, revision = ?3, void_reason = ?4,
                               updated_at = ?5, closed_at = ?6
                             WHERE cart_id = ?7 AND register_id = ?8 AND revision = ?9 AND status = 'open'",
                        )
                        .bind(status)
                        .bind(json(&cart.order_discount)?)
                        .bind(as_i64(cart.revision)?)
                        .bind(&cart.void_reason)
                        .bind(now)
                        .bind(closed_at)
                        .bind(cart.id.to_string())
                        .bind(ctx.register_id.to_string())
                        .bind(as_i64(*base)?)
                        .execute(&mut *tx)
                        .await
                        .map_err(map_sqlx)?;
                        if done.rows_affected() != 1 {
                            return Err(stale);
                        }
                    }
                }
                st.step()?;
                sqlx::query("DELETE FROM cart_lines WHERE cart_id = ?")
                    .bind(cart.id.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                for (pos, l) in cart.lines.iter().enumerate() {
                    let (via, scan_id) = match &l.added_via {
                        AddVia::Scan { event_id } => ("scan", Some(event_id.to_string())),
                        AddVia::Search => ("search", None),
                        AddVia::QuickSelect => ("quick_select", None),
                        AddVia::ManualCode => ("manual_code", None),
                    };
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO cart_lines (cart_line_id, cart_id, position, variant_id, quantity_milli,
                           unit_price_override_minor, line_discount_json, added_via, last_scan_event_id, created_at, updated_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(l.id.to_string())
                    .bind(cart.id.to_string())
                    .bind(i64::try_from(pos).unwrap_or(i64::MAX))
                    .bind(l.variant.variant_id.to_string())
                    .bind(l.quantity.milli())
                    .bind(l.price_override.map(|m| m.minor()))
                    .bind(json(&l.line_discount)?)
                    .bind(via)
                    .bind(scan_id)
                    .bind(now)
                    .bind(now)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                }
            }
            WriteOp::RecordScanEvent { scan_event_id } => {
                st.step()?;
                sqlx::query(
                    "INSERT INTO scan_events_seen (scan_event_id, register_id, received_at) VALUES (?, ?, ?)",
                )
                .bind(scan_event_id.to_string())
                .bind(ctx.register_id.to_string())
                .bind(ctx.occurred_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(|e| match map_sqlx(e) {
                    WriteError::UniqueViolation { .. } => WriteError::ScanAlreadySeen,
                    other => other,
                })?;
                // Keep a window of the last 500 per register (rowid order is insertion order).
                st.step()?;
                sqlx::query(
                    "DELETE FROM scan_events_seen WHERE register_id = ?1 AND rowid NOT IN
                       (SELECT rowid FROM scan_events_seen WHERE register_id = ?1 ORDER BY rowid DESC LIMIT 500)",
                )
                .bind(ctx.register_id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::InsertSale {
                sale,
                receipt_slot,
                require_cash_session,
            } => {
                let seq = *applied.numbers.get(receipt_slot).ok_or_else(|| {
                    WriteError::Io("receipt number must be allocated before the sale".into())
                })?;
                st.step()?;
                let session = crate::cash::open_session(&mut tx, &ctx.register_id.to_string())
                    .await
                    .map_err(map_sqlx)?;
                if session.is_none() && *require_cash_session {
                    return Err(WriteError::CashSessionRequired);
                }
                st.step()?;
                let (code, prefix): (String, String) =
                    sqlx::query_as("SELECT code, receipt_prefix FROM registers WHERE register_id = ?")
                        .bind(ctx.register_id.to_string())
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(map_sqlx)?;
                let receipt = pos_domain::receipt_number(&prefix, &code, seq);
                st.step()?;
                sqlx::query(
                    "INSERT INTO sales (sale_id, business_id, location_id, register_id, cart_id, cash_session_id, receipt_seq,
                       receipt_number, status, mode, currency_code, currency_exponent, prices_include_tax, subtotal_minor,
                       line_discount_total_minor, order_discount_total_minor, tax_total_minor, rounding_minor, total_minor,
                       tendered_total_minor, change_minor, order_discount_json, tax_breakdown_json, cashier_user_id,
                       approver_user_id, occurred_at, business_date, timezone, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'completed', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(sale.id.to_string())
                .bind(ctx.business_id.to_string())
                .bind(ctx.location_id.to_string())
                .bind(ctx.register_id.to_string())
                .bind(sale.cart_id.to_string())
                .bind(&session)
                .bind(as_i64(seq)?)
                .bind(&receipt)
                .bind(sale.mode.as_str())
                .bind(sale.currency.code.to_string())
                .bind(i64::from(sale.currency.exponent))
                .bind(sale.prices_include_tax)
                .bind(sale.subtotal.minor())
                .bind(sale.line_discount_total.minor())
                .bind(sale.order_discount_total.minor())
                .bind(sale.tax_total.minor())
                .bind(sale.rounding.minor())
                .bind(sale.total.minor())
                .bind(sale.tendered_total.minor())
                .bind(sale.change.minor())
                .bind(sale.order_discount.as_ref().map(json).transpose()?)
                .bind(json(&sale.tax_breakdown)?)
                .bind(sale.cashier.to_string())
                .bind(sale.approver.map(|a| a.to_string()))
                .bind(sale.occurred_at.as_millis())
                .bind(sale.business_date.to_string())
                .bind(&sale.timezone)
                .bind(ctx.occurred_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                for l in &sale.lines {
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO sale_lines (sale_line_id, sale_id, business_id, line_no, product_id, variant_id,
                           name_snapshot, variant_label_snapshot, sku_snapshot, barcode_snapshot, unit_code, product_type,
                           quantity_milli, unit_price_minor, price_overridden, gross_minor, line_discount_minor,
                           order_discount_alloc_minor, tax_minor, total_minor, discount_json, tax_lines_json, unit_cost_minor)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(l.id.to_string())
                    .bind(sale.id.to_string())
                    .bind(ctx.business_id.to_string())
                    .bind(i64::from(l.line_no))
                    .bind(l.product_id.to_string())
                    .bind(l.variant_id.to_string())
                    .bind(&l.name)
                    .bind(&l.variant_label)
                    .bind(&l.sku)
                    .bind(l.unit.code())
                    .bind(l.product_type.as_str())
                    .bind(l.quantity.milli())
                    .bind(l.unit_price.minor())
                    .bind(l.price_overridden)
                    .bind(l.gross.minor())
                    .bind(l.line_discount.minor())
                    .bind(l.order_discount_alloc.minor())
                    .bind(l.tax.minor())
                    .bind(l.total.minor())
                    .bind(l.discount.as_ref().map(json).transpose()?)
                    .bind(json(&l.tax_lines)?)
                    .bind(l.unit_cost.map(|c| c.minor()))
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                }
                for t in &sale.tenders {
                    let x = &t.tender;
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO tenders (tender_id, sale_id, business_id, seq, tender_type, amount_minor, received_minor,
                           change_minor, status, confirmation_source, provider_code, external_reference, recorded_by, occurred_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?)",
                    )
                    .bind(t.id.to_string())
                    .bind(sale.id.to_string())
                    .bind(ctx.business_id.to_string())
                    .bind(i64::from(t.seq))
                    .bind(x.kind.as_str())
                    .bind(x.amount.minor())
                    .bind(x.received.map(|m| m.minor()))
                    .bind(x.change.minor())
                    .bind(x.status.as_str())
                    .bind(x.confirmation.as_str())
                    .bind(&x.external_reference)
                    .bind(sale.cashier.to_string())
                    .bind(sale.occurred_at.as_millis())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO tender_status_events (tender_event_id, tender_id, from_status, to_status, source,
                           actor_user_id, occurred_at) VALUES (?, ?, NULL, ?, ?, ?, ?)",
                    )
                    .bind(format!("{}-0", t.id))
                    .bind(t.id.to_string())
                    .bind(x.status.as_str())
                    .bind(x.confirmation.as_str())
                    .bind(sale.cashier.to_string())
                    .bind(sale.occurred_at.as_millis())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                }
                applied.receipt_numbers.insert(*receipt_slot, receipt);
            }
            WriteOp::AppendMovements { movements, .. } => {
                for m in movements {
                    let (source_type, source_id, source_line) = m.source.columns();
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO stock_movements (movement_id, business_id, location_id, variant_id, bucket, movement_type,
                           quantity_delta_milli, unit_cost_minor, source_type, source_id, source_line_id, register_id,
                           actor_user_id, occurred_at, business_date, created_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(m.id.to_string())
                    .bind(ctx.business_id.to_string())
                    .bind(m.location_id.to_string())
                    .bind(m.variant_id.to_string())
                    .bind(m.bucket.as_str())
                    .bind(m.movement_type.as_str())
                    .bind(m.delta.milli())
                    .bind(m.unit_cost.map(|c| c.minor()))
                    .bind(source_type)
                    .bind(source_id)
                    .bind(source_line)
                    .bind(ctx.register_id.to_string())
                    .bind(m.actor.to_string())
                    .bind(m.occurred_at.as_millis())
                    .bind(m.business_date.to_string())
                    .bind(ctx.occurred_at.as_millis())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                    // The projection moves in the same transaction (INV-004).
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO stock_balances (business_id, location_id, variant_id, bucket, on_hand_milli,
                           last_movement_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                         ON CONFLICT (location_id, variant_id, bucket) DO UPDATE SET
                           on_hand_milli = on_hand_milli + ?5,
                           last_movement_at = max(last_movement_at, ?6), updated_at = ?7",
                    )
                    .bind(ctx.business_id.to_string())
                    .bind(m.location_id.to_string())
                    .bind(m.variant_id.to_string())
                    .bind(m.bucket.as_str())
                    .bind(m.delta.milli())
                    .bind(m.occurred_at.as_millis())
                    .bind(ctx.occurred_at.as_millis())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                }
            }
            WriteOp::InsertInventoryDocument { doc, number_slot } => {
                let seq = *applied.numbers.get(number_slot).ok_or_else(|| {
                    WriteError::Io("the document number must be allocated before the document".into())
                })?;
                st.step()?;
                let code: String = sqlx::query_scalar("SELECT code FROM registers WHERE register_id = ?")
                    .bind(ctx.register_id.to_string())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                let number = format!("{code}-IN-{seq:06}");
                st.step()?;
                sqlx::query(
                    "INSERT INTO inventory_documents (document_id, business_id, location_id, register_id, document_no,
                       doc_type, reason_code, supplier_name, reference, note, actor_user_id, occurred_at, business_date,
                       created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(doc.id.to_string())
                .bind(ctx.business_id.to_string())
                .bind(ctx.location_id.to_string())
                .bind(ctx.register_id.to_string())
                .bind(&number)
                .bind(doc.plan.doc_type.as_str())
                .bind(doc.plan.reason.map(|r| r.as_str()))
                .bind(&doc.plan.supplier_name)
                .bind(&doc.plan.reference)
                .bind(&doc.plan.note)
                .bind(doc.actor.to_string())
                .bind(doc.occurred_at.as_millis())
                .bind(doc.business_date.to_string())
                .bind(ctx.occurred_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                applied.receipt_numbers.insert(*number_slot, number);
            }
            WriteOp::SetRegisterMode { from, to } => {
                st.step()?;
                let done = sqlx::query("UPDATE device_identity SET mode = ? WHERE id = 1 AND mode = ?")
                    .bind(to.as_str())
                    .bind(from.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                if done.rows_affected() != 1 {
                    return Err(WriteError::ModeChanged);
                }
            }
            WriteOp::RebuildStockBalances => {
                st.step()?;
                sqlx::query("DELETE FROM stock_balances")
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO stock_balances (business_id, location_id, variant_id, bucket, on_hand_milli, last_movement_at, updated_at)
                     SELECT business_id, location_id, variant_id, bucket, sum(quantity_delta_milli), max(occurred_at), ?
                     FROM stock_movements GROUP BY business_id, location_id, variant_id, bucket",
                )
                .bind(ctx.occurred_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::OpenCashSession { session, number_slot } => {
                st.step()?;
                let (code, next): (String, i64) = sqlx::query_as(
                    "SELECT r.code, coalesce((SELECT max(session_seq) FROM cash_sessions c WHERE c.register_id = r.register_id), 0) + 1
                     FROM registers r WHERE r.register_id = ?",
                )
                .bind(ctx.register_id.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                let number = format!("{code}-Z-{next:04}");
                st.step()?;
                sqlx::query(
                    "INSERT INTO cash_sessions (cash_session_id, business_id, register_id, session_seq, session_number,
                       opened_by, opened_at, opening_float_minor, business_date, status)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'open')",
                )
                .bind(session.id.to_string())
                .bind(ctx.business_id.to_string())
                .bind(ctx.register_id.to_string())
                .bind(next)
                .bind(&number)
                .bind(session.opened_by.to_string())
                .bind(session.opened_at.as_millis())
                .bind(session.opening_float.minor())
                .bind(session.business_date.to_string())
                .execute(&mut *tx)
                .await
                .map_err(|e| match map_sqlx(e) {
                    WriteError::UniqueViolation { constraint } if constraint.contains("register_id") => {
                        WriteError::CashSessionAlreadyOpen
                    }
                    other => other,
                })?;
                applied.receipt_numbers.insert(*number_slot, number);
            }
            WriteOp::CloseCashSession { closed } => {
                st.step()?;
                let id = closed.id.to_string();
                let float: Option<i64> = sqlx::query_scalar(
                    "SELECT opening_float_minor FROM cash_sessions
                     WHERE cash_session_id = ? AND register_id = ? AND status = 'open'",
                )
                .bind(&id)
                .bind(ctx.register_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                let Some(float) = float else {
                    return Err(WriteError::CashSessionNotOpen);
                };
                st.step()?;
                let totals = crate::cash::session_totals(&mut tx, &id)
                    .await
                    .map_err(map_sqlx)?;
                let expected = totals
                    .expected_cash(pos_domain::Money::from_minor(float))
                    .map_err(|e| WriteError::Io(e.to_string()))?;
                if totals != closed.totals
                    || expected != closed.expected
                    || float != closed.opening_float.minor()
                {
                    return Err(WriteError::CashSessionChanged);
                }
                st.step()?;
                sqlx::query(
                    "UPDATE cash_sessions SET status = 'closed', closed_by = ?, closed_at = ?, expected_cash_minor = ?,
                       counted_cash_minor = ?, variance_minor = ?, totals_json = ?, note = ?
                     WHERE cash_session_id = ?",
                )
                .bind(closed.closed_by.to_string())
                .bind(closed.closed_at.as_millis())
                .bind(closed.expected.minor())
                .bind(closed.counted.minor())
                .bind(closed.variance.minor())
                .bind(json(&closed.totals)?)
                .bind(&closed.note)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::InsertReturn {
                ret,
                number_slot,
                require_cash_session,
            } => {
                let seq = *applied.numbers.get(number_slot).ok_or_else(|| {
                    WriteError::Io("the return number must be allocated before the return".into())
                })?;
                st.step()?;
                let session = crate::cash::open_session(&mut tx, &ctx.register_id.to_string())
                    .await
                    .map_err(map_sqlx)?;
                if session.is_none() && *require_cash_session {
                    return Err(WriteError::CashSessionRequired);
                }
                // RET-004 inside BEGIN IMMEDIATE: nothing else can return these lines meanwhile.
                // A sale line split across dispositions is checked for its parts together.
                let mut wanted: Vec<(pos_domain::SaleLineId, i64)> = Vec::new();
                for l in &ret.lines {
                    match wanted.iter_mut().find(|(id, _)| *id == l.line.sale_line_id) {
                        Some((_, q)) => *q += l.line.quantity.milli(),
                        None => wanted.push((l.line.sale_line_id, l.line.quantity.milli())),
                    }
                }
                for (line, quantity) in wanted {
                    st.step()?;
                    let available: Option<i64> = sqlx::query_scalar(
                        "SELECT sl.quantity_milli - coalesce((SELECT sum(rl.quantity_milli) FROM return_lines rl
                           WHERE rl.original_sale_line_id = sl.sale_line_id), 0)
                         FROM sale_lines sl WHERE sl.sale_line_id = ? AND sl.sale_id = ?",
                    )
                    .bind(line.to_string())
                    .bind(ret.original_sale.to_string())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                    let available = available.unwrap_or(0);
                    if quantity > available {
                        return Err(WriteError::ReturnQtyExceeds {
                            line,
                            available_milli: available,
                        });
                    }
                }
                st.step()?;
                let code: String = sqlx::query_scalar("SELECT code FROM registers WHERE register_id = ?")
                    .bind(ctx.register_id.to_string())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                let number = format!("{code}-R-{seq:06}");
                st.step()?;
                sqlx::query(
                    "INSERT INTO returns (return_id, business_id, location_id, register_id, original_sale_id, return_seq,
                       return_number, reason_code, note, subtotal_minor, tax_minor, total_minor, actor_user_id,
                       approver_user_id, occurred_at, business_date, created_at, cash_session_id)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(ret.id.to_string())
                .bind(ctx.business_id.to_string())
                .bind(ctx.location_id.to_string())
                .bind(ctx.register_id.to_string())
                .bind(ret.original_sale.to_string())
                .bind(as_i64(seq)?)
                .bind(&number)
                .bind(ret.reason.as_str())
                .bind(&ret.note)
                .bind(ret.subtotal.minor())
                .bind(ret.tax.minor())
                .bind(ret.total.minor())
                .bind(ret.actor.to_string())
                .bind(ret.approver.map(|a| a.to_string()))
                .bind(ret.occurred_at.as_millis())
                .bind(ret.business_date.to_string())
                .bind(ctx.occurred_at.as_millis())
                .bind(&session)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                for l in &ret.lines {
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO return_lines (return_line_id, return_id, original_sale_line_id, quantity_milli,
                           refund_minor, tax_minor, disposition) VALUES (?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(l.id.to_string())
                    .bind(ret.id.to_string())
                    .bind(l.line.sale_line_id.to_string())
                    .bind(l.line.quantity.milli())
                    .bind(l.line.refund.minor())
                    .bind(l.line.tax.minor())
                    .bind(l.line.disposition.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                }
                for r in &ret.refunds {
                    st.step()?;
                    sqlx::query(
                        "INSERT INTO refunds (refund_id, return_id, business_id, seq, tender_type, amount_minor, status,
                           confirmation_source, external_reference, recorded_by, occurred_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(r.id.to_string())
                    .bind(ret.id.to_string())
                    .bind(ctx.business_id.to_string())
                    .bind(i64::from(r.seq))
                    .bind(r.tender.kind.as_str())
                    .bind(r.tender.amount.minor())
                    .bind(r.tender.status.as_str())
                    .bind(r.tender.confirmation.as_str())
                    .bind(&r.tender.external_reference)
                    .bind(ret.actor.to_string())
                    .bind(ret.occurred_at.as_millis())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
                }
                applied.receipt_numbers.insert(*number_slot, number);
            }
            WriteOp::PutPrintJob { job, detail } => {
                st.step()?;
                sqlx::query(
                    "INSERT INTO print_jobs (job_id, receipt_id, adapter, target, copy_kind, status, attempt_count, last_error_code,
                       requested_by, created_at, updated_at)
                     VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT (job_id) DO UPDATE SET status = ?5, attempt_count = ?6, last_error_code = ?7, updated_at = ?10",
                )
                .bind(job.job_id.to_string())
                .bind(job.receipt_id.to_string())
                .bind(job.adapter)
                .bind(if job.reprint { "reprint" } else { "original" })
                .bind(job.status.as_str())
                .bind(i64::from(job.attempt_count))
                .bind(&job.last_error_code)
                .bind(job.requested_by.to_string())
                .bind(job.created_at.as_millis())
                .bind(job.updated_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                st.step()?;
                sqlx::query(
                    "INSERT INTO print_job_events (job_id, seq, status, detail_json, occurred_at)
                     VALUES (?1, (SELECT coalesce(max(seq), 0) + 1 FROM print_job_events WHERE job_id = ?1), ?2, ?3, ?4)",
                )
                .bind(job.job_id.to_string())
                .bind(job.status.as_str())
                .bind(detail.to_string())
                .bind(job.updated_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::InsertReceipt {
                receipt_id,
                source_type,
                source_id,
                content_version,
                document,
                receipt_slot,
            } => {
                let mut doc = document.clone();
                if let Some(slot) = receipt_slot {
                    let number = applied.receipt_numbers.get(slot).ok_or_else(|| {
                        WriteError::Io("a receipt's number must come from a sale in the same set".into())
                    })?;
                    let obj = doc
                        .as_object_mut()
                        .ok_or_else(|| WriteError::Io("a receipt document must be an object".into()))?;
                    obj.insert("barcode".into(), number.clone().into());
                    if let Some(meta) = obj.get_mut("meta").and_then(|m| m.as_object_mut()) {
                        meta.insert("receiptNumber".into(), number.clone().into());
                    }
                }
                st.step()?;
                sqlx::query(
                    "INSERT INTO receipts (receipt_id, business_id, source_type, source_id, content_version, document_json, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(receipt_id.to_string())
                .bind(ctx.business_id.to_string())
                .bind(*source_type)
                .bind(source_id)
                .bind(i64::from(*content_version))
                .bind(doc.to_string())
                .bind(ctx.occurred_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::CloseCart {
                cart_id,
                sale_id,
                expected_revision,
            } => {
                st.step()?;
                let done = sqlx::query(
                    "UPDATE carts SET status = 'completed', sale_id = ?1, closed_at = ?2, updated_at = ?2, revision = revision + 1
                     WHERE cart_id = ?3 AND register_id = ?4 AND revision = ?5 AND status = 'open'",
                )
                .bind(sale_id.to_string())
                .bind(ctx.occurred_at.as_millis())
                .bind(cart_id.to_string())
                .bind(ctx.register_id.to_string())
                .bind(as_i64(*expected_revision)?)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
                if done.rows_affected() != 1 {
                    return Err(WriteError::CartRevisionMismatch {
                        expected: Some(*expected_revision),
                    });
                }
            }
            WriteOp::PutLoginThrottle(t) => {
                st.step()?;
                sqlx::query(
                    "INSERT INTO staff_login_throttle (user_id, failed_attempts, locked_until, last_failed_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (user_id) DO UPDATE SET failed_attempts = ?2, locked_until = ?3, last_failed_at = ?4",
                )
                .bind(t.user_id.to_string())
                .bind(i64::from(t.failed_attempts))
                .bind(t.locked_until.map(|x| x.as_millis()))
                .bind(t.last_failed_at.map(|x| x.as_millis()))
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::StartSession { session_id, user_id } => {
                st.step()?;
                sqlx::query(
                    "INSERT INTO staff_sessions (session_id, user_id, register_id, started_at) VALUES (?, ?, ?, ?)",
                )
                .bind(session_id.to_string())
                .bind(user_id.to_string())
                .bind(ctx.register_id.to_string())
                .bind(ctx.occurred_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            WriteOp::EndSession { session_id, reason } => {
                st.step()?;
                sqlx::query(
                    "UPDATE staff_sessions SET ended_at = ?, end_reason = ? WHERE session_id = ? AND ended_at IS NULL",
                )
                .bind(ctx.occurred_at.as_millis())
                .bind(reason.as_str())
                .bind(session_id.to_string())
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
        .bind(payload_with_numbers(e, &applied)?.to_string())
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

/// The event payload with the receipt number filled in, when the draft asked for it.
fn payload_with_numbers(
    e: &pos_application::OutboxDraft,
    applied: &AppliedWriteSet,
) -> Result<serde_json::Value, WriteError> {
    let Some(slot) = e.receipt_slot else {
        return Ok(e.payload.clone());
    };
    let missing = || {
        WriteError::Io(format!(
            "event {} needs receipt slot {slot:?}, which no sale used",
            e.event_type
        ))
    };
    let seq = *applied.numbers.get(&slot).ok_or_else(missing)?;
    let number = applied.receipt_numbers.get(&slot).ok_or_else(missing)?;
    let mut p = e.payload.clone();
    let obj = p
        .as_object_mut()
        .ok_or_else(|| WriteError::Io("a payload with a receipt number must be an object".into()))?;
    let (seq_key, number_key) = e.number_keys;
    obj.insert(seq_key.into(), seq.into());
    obj.insert(number_key.into(), number.clone().into());
    Ok(p)
}

/// A value stored as JSON text.
fn json<T: serde::Serialize + ?Sized>(v: &T) -> Result<String, WriteError> {
    serde_json::to_string(v).map_err(|e| WriteError::Io(e.to_string()))
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
