//! STAGE 0 SPIKE (E00-T06): IPC commands for the vertical slice
//! "set up -> scan -> complete cash sale -> virtual print". Thin adapters over
//! `pos_application::spike` and `pos_local_db::spike`. Delete with the spike (E09-T08).

use crate::{AppServices, IpcError};
use pos_application::LocalStore;
use pos_application::audit::{self, AuditEntry};
use pos_application::records::{BusinessRecord, LocationRecord, RegisterRecord};
use pos_application::spike::{self, SpikeProduct};
use pos_application::{OutboxDraft, WriteContext, WriteSet};
use pos_device_virtual::printer::VirtualPrinter;
use pos_domain::{
    BusinessId, Currency, ErrorCode, HasErrorCode, LocationId, Money, NumberFormat, ProductId, Quantity,
    RegisterId,
};
use pos_local_db::spike as db_spike;
use pos_local_db::{LocalDb, SqliteStore};
use pos_receipts::escpos::{Align, Cut, EscPos};
use serde::Serialize;
use tauri::State;

pub const DEMO_BARCODE: &str = "5012345678900";

fn gbp() -> Currency {
    Currency::from_iso("GBP").expect("GBP is in the ISO table")
}

fn fmt(m: Money) -> String {
    gbp().format(m, NumberFormat::EN_US)
}

#[derive(Debug)]
struct DemoError(ErrorCode, String);

impl std::fmt::Display for DemoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

impl HasErrorCode for DemoError {
    fn error_code(&self) -> ErrorCode {
        self.0
    }
}

fn err<E: HasErrorCode + std::fmt::Display>(e: E) -> IpcError {
    IpcError::from_error(&e)
}

fn parts(services: &AppServices) -> Result<(&LocalDb, &SqliteStore), IpcError> {
    let db = services.db.as_ref().map_err(Clone::clone)?;
    let store = services
        .store
        .as_ref()
        .ok_or_else(|| IpcError::internal("store unavailable"))?;
    Ok((db, store))
}

async fn context(
    services: &AppServices,
    db: &LocalDb,
    store: &SqliteStore,
) -> Result<WriteContext, IpcError> {
    let now = services.clock.now();
    if let Some((business_id, location_id, register_id)) = db_spike::workspace(db).await.map_err(err)? {
        return Ok(WriteContext {
            business_id,
            location_id,
            register_id,
            actor: None,
            occurred_at: now,
        });
    }
    // First run of the demo: create the workspace through the real write path (ADR-009:
    // business, location and register events go to the outbox from day zero).
    let ids = services.ids.as_ref();
    let ctx = WriteContext {
        business_id: BusinessId::generate(ids),
        location_id: LocationId::generate(ids),
        register_id: RegisterId::generate(ids),
        actor: None,
        occurred_at: now,
    };
    let business = BusinessRecord {
        business_id: ctx.business_id,
        name: "Sale OS Demo Shop".into(),
        legal_name: None,
        tax_registration_no: None,
        address: serde_json::json!({}),
        phone: None,
        email: None,
        logo_path: None,
        country_code: "GB".into(),
        timezone: "Europe/London".into(),
        currency: gbp(),
        prices_include_tax: true,
        version: 1,
        created_at: now,
        updated_at: now,
    };
    let location = LocationRecord {
        location_id: ctx.location_id,
        business_id: ctx.business_id,
        name: "Main".into(),
        address: serde_json::json!({}),
        archived: false,
        version: 1,
        created_at: now,
        updated_at: now,
    };
    let register = RegisterRecord {
        register_id: ctx.register_id,
        business_id: ctx.business_id,
        location_id: ctx.location_id,
        name: "Till 1".into(),
        code: "R01".into(),
        receipt_prefix: String::new(),
        retired: false,
        version: 1,
        created_at: now,
        updated_at: now,
    };
    let set = WriteSet::builder(ctx.clone())
        .outbox(OutboxDraft::business_updated(ids, &business).map_err(err)?)
        .outbox(OutboxDraft::location_upserted(ids, &location).map_err(err)?)
        .outbox(OutboxDraft::register_upserted(ids, &register).map_err(err)?)
        .put_business(business, None)
        .put_location(location, None)
        .put_register(register, None)
        .audit(AuditEntry::new(
            ids,
            audit::BUSINESS_CREATED,
            "business",
            ctx.business_id,
        ))
        .build()
        .map_err(err)?;
    store.apply(set).await.map_err(err)?;
    pos_observability::set_context("register_id", ctx.register_id);
    Ok(ctx)
}

#[derive(Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DemoProduct {
    pub name: String,
    pub barcode: String,
    pub price_formatted: String,
}

/// Creates the demo workspace and product if missing.
#[tauri::command]
#[specta::specta]
pub async fn dev_demo_setup(services: State<'_, AppServices>) -> Result<DemoProduct, IpcError> {
    let (db, store) = parts(&services)?;
    db_spike::ensure_spike_tables(db).await.map_err(err)?;
    let ctx = context(&services, db, store).await?;
    let product = match db_spike::product_by_barcode(db, DEMO_BARCODE)
        .await
        .map_err(err)?
    {
        Some(p) => p,
        None => {
            let p = SpikeProduct {
                product_id: ProductId::generate(services.ids.as_ref()),
                name: "Demo Cola 330ml".into(),
                barcode: DEMO_BARCODE.into(),
                price: Money::from_minor(120),
            };
            store
                .apply(spike::create_product(&ctx, services.ids.as_ref(), p.clone()).map_err(err)?)
                .await
                .map_err(err)?;
            p
        }
    };
    Ok(DemoProduct {
        name: product.name,
        barcode: product.barcode,
        price_formatted: fmt(product.price),
    })
}

#[derive(Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DemoCartLine {
    pub name: String,
    pub quantity: String,
    pub line_total_formatted: String,
}

#[derive(Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DemoCart {
    pub lines: Vec<DemoCartLine>,
    pub total_minor: String,
    pub total_formatted: String,
}

async fn cart_view(db: &LocalDb) -> Result<DemoCart, IpcError> {
    let lines = db_spike::cart(db).await.map_err(err)?;
    let mut out = Vec::new();
    let mut total = Money::ZERO;
    for l in &lines {
        let units = l.quantity.milli() / 1000;
        let line_total = Money::from_minor(l.product.price.minor().saturating_mul(units));
        total = total.checked_add(line_total).map_err(err)?;
        out.push(DemoCartLine {
            name: l.product.name.clone(),
            quantity: l.quantity.to_string(),
            line_total_formatted: fmt(line_total),
        });
    }
    Ok(DemoCart {
        lines: out,
        total_minor: total.minor().to_string(),
        total_formatted: fmt(total),
    })
}

/// Current demo cart.
#[tauri::command]
#[specta::specta]
pub async fn dev_demo_cart(services: State<'_, AppServices>) -> Result<DemoCart, IpcError> {
    let (db, _) = parts(&services)?;
    db_spike::ensure_spike_tables(db).await.map_err(err)?;
    cart_view(db).await
}

/// Adds the scanned product to the cart. `scan_event_id` is accepted for the E07-T02
/// contract; duplicate-delivery handling arrives with that task.
#[tauri::command]
#[specta::specta]
pub async fn dev_demo_scan(
    services: State<'_, AppServices>,
    raw: String,
    scan_event_id: String,
) -> Result<DemoCart, IpcError> {
    let (db, store) = parts(&services)?;
    let code = raw.trim();
    tracing::info!(%scan_event_id, code, "demo scan");
    let product = db_spike::product_by_barcode(db, code)
        .await
        .map_err(err)?
        .ok_or_else(|| {
            err(DemoError(
                ErrorCode::ScanNotFound,
                format!("no product with barcode {code:?}"),
            ))
        })?;
    let ctx = context(&services, db, store).await?;
    store
        .apply(
            spike::add_to_cart(&ctx, services.ids.as_ref(), product.product_id, Quantity::ONE)
                .map_err(err)?,
        )
        .await
        .map_err(err)?;
    cart_view(db).await
}

#[derive(Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DemoSaleResult {
    pub receipt_number: String,
    pub total_formatted: String,
    pub change_formatted: String,
    pub outbox_sequence: u32,
    /// Path of the virtual printer's ESC/POS capture, or why printing failed (a print
    /// failure never undoes a committed sale).
    pub print: String,
}

/// Completes a cash sale atomically, then prints to the virtual printer.
#[tauri::command]
#[specta::specta]
pub async fn dev_demo_complete_sale(
    services: State<'_, AppServices>,
    cash_received_minor: String,
) -> Result<DemoSaleResult, IpcError> {
    let (db, store) = parts(&services)?;
    let cash = cash_received_minor
        .trim()
        .parse::<i64>()
        .map(Money::from_minor)
        .map_err(|_| {
            err(DemoError(
                ErrorCode::ValidationFailed,
                "cash received must be minor units".into(),
            ))
        })?;
    let ctx = context(&services, db, store).await?;
    let lines = db_spike::cart(db).await.map_err(err)?;
    let (set, sale) = spike::complete_cash_sale(&ctx, services.ids.as_ref(), &lines, cash).map_err(err)?;
    let applied = store.apply(set).await.map_err(err)?;
    let receipt_number = db_spike::receipt_number(db, &sale.sale_id.to_string())
        .await
        .map_err(err)?
        .ok_or_else(|| IpcError::internal("committed sale not found"))?;
    let outbox_sequence = applied.outbox.first().map(|(_, s)| *s).unwrap_or_default();

    // Printing happens after the commit: a printer problem can never roll back a sale.
    let mut p = EscPos::new();
    p.align(Align::Center)
        .bold(true)
        .line("SALE OS DEMO SHOP")
        .bold(false)
        .line(&receipt_number)
        .align(Align::Left);
    for l in &sale.lines {
        p.line(&format!("{:<22}{:>10}", l.name, fmt(l.total)));
    }
    p.bold(true)
        .line(&format!("{:<22}{:>10}", "TOTAL", fmt(sale.total)))
        .bold(false)
        .line(&format!("{:<22}{:>10}", "CASH", fmt(sale.tender.received)))
        .line(&format!("{:<22}{:>10}", "CHANGE", fmt(sale.tender.change)))
        .feed(3)
        .cut(Cut::Partial);
    let printer = VirtualPrinter::new(services.data_dir.join("virtual-printer"));
    let print = match printer.capture(&receipt_number, p.bytes()) {
        Ok(job) => job.bin.display().to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "virtual print failed after commit");
            format!("print failed: {e}")
        }
    };
    Ok(DemoSaleResult {
        receipt_number,
        total_formatted: fmt(sale.total),
        change_formatted: fmt(sale.tender.change),
        outbox_sequence: u32::try_from(outbox_sequence).unwrap_or(u32::MAX),
        print,
    })
}
