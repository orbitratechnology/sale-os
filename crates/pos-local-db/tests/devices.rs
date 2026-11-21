//! E20-T05 / E20-T11: devices attached to a till. Saving a printer swaps it in for the next
//! print, bad configurations are refused by field and never stored, the drawer opens only
//! for cash and on an audited, approved "no sale".

mod common;

use pos_application::context::AppError;
use pos_application::devices::{
    DeviceConfigProblem, DeviceDriver, DeviceInput, DeviceService, DrawerKick, PrinterHandle,
    SwitchablePrinter,
};
use pos_application::records::DeviceKind;
use pos_device_ports::{DeviceError, DrawerPin, PrintResult};
use pos_device_virtual::printer::VirtualPrinter;
use pos_domain::{ErrorCode, HasErrorCode, Role};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A driver that "connects" to named virtual printers: `{"printer": "<name>", "pin": 2|5}`.
/// `{"printer": "broken"}` validates but cannot be opened (a vanished port).
struct FakeDriver {
    dir: std::path::PathBuf,
    printers: Mutex<HashMap<String, Arc<VirtualPrinter>>>,
}

impl FakeDriver {
    fn printer(&self, name: &str) -> Arc<VirtualPrinter> {
        self.printers
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_insert_with(|| Arc::new(VirtualPrinter::new(self.dir.join(name))))
            .clone()
    }
}

#[async_trait::async_trait]
impl DeviceDriver for FakeDriver {
    fn validate(&self, _kind: DeviceKind, config: &Value) -> Result<Value, DeviceConfigProblem> {
        match config.get("printer").and_then(Value::as_str) {
            Some(n) if !n.trim().is_empty() => {
                Ok(json!({ "printer": n.trim(), "pin": config.get("pin").cloned().unwrap_or(json!(2)) }))
            }
            _ => Err(DeviceConfigProblem {
                field: "printer".into(),
                message: "choose a printer".into(),
            }),
        }
    }

    async fn open_printer(&self, config: &Value) -> Result<PrinterHandle, DeviceError> {
        let name = config["printer"].as_str().unwrap_or_default();
        if name == "broken" {
            return Err(DeviceError::Disconnected("COM9 is not present".into()));
        }
        let p = self.printer(name);
        Ok(PrinterHandle {
            printer: p.clone(),
            drawer: Some(p),
            drawer_pin: if config["pin"] == json!(5) {
                DrawerPin::Pin5
            } else {
                DrawerPin::Pin2
            },
        })
    }
}

struct Till {
    shop: common::Shop,
    driver: Arc<FakeDriver>,
    fallback: Arc<VirtualPrinter>,
    service: DeviceService,
    _dir: tempfile::TempDir,
}

async fn till() -> Till {
    let shop = common::shop().await;
    let dir = tempfile::tempdir().unwrap();
    let fallback = Arc::new(VirtualPrinter::new(dir.path().join("fallback")));
    let driver = Arc::new(FakeDriver {
        dir: dir.path().to_owned(),
        printers: Mutex::new(HashMap::new()),
    });
    let s = shop.store.clone();
    let service = DeviceService {
        store: Arc::new(s.clone()),
        devices: Arc::new(s),
        driver: driver.clone(),
        printer: Arc::new(SwitchablePrinter::new(PrinterHandle {
            printer: fallback.clone(),
            drawer: None,
            drawer_pin: DrawerPin::Pin2,
        })),
    };
    Till {
        shop,
        driver,
        fallback,
        service,
        _dir: dir,
    }
}

fn printer_input(name: &str) -> DeviceInput {
    DeviceInput {
        device_id: None,
        kind: DeviceKind::ReceiptPrinter,
        role: "main".into(),
        name: "Front counter".into(),
        config: json!({ "printer": name }),
        enabled: true,
        base_version: None,
    }
}

async fn audit(t: &Till, action: &str) -> Vec<(String, Option<String>, String)> {
    sqlx::query_as(
        "SELECT entity_id, approver_user_id, metadata_json FROM audit_events WHERE action = ? ORDER BY rowid",
    )
    .bind(action)
    .fetch_all(t.shop.store.db().readers())
    .await
    .unwrap()
}

#[tokio::test]
async fn a_saved_printer_is_used_by_the_next_job_without_a_restart() {
    let t = till().await;
    let owner = &t.shop.owner;
    assert!(!t.service.printer.is_configured());
    let saved = t.service.save(owner, printer_input("epson")).await.unwrap();
    assert_eq!(saved.version, 1);
    assert!(t.service.printer.is_configured());

    use pos_device_ports::ReceiptOutputPort;
    assert_eq!(t.service.printer.submit("job", b"x").await, PrintResult::Printed);
    assert_eq!(t.driver.printer("epson").submissions(), 1);
    assert_eq!(t.fallback.submissions(), 0);

    // Changing it to another printer swaps again; a stale version is refused.
    let mut change = printer_input("star");
    change.device_id = Some(saved.device_id);
    change.base_version = Some(1);
    t.service.save(owner, change.clone()).await.unwrap();
    t.service.printer.submit("job", b"x").await;
    assert_eq!(t.driver.printer("star").submissions(), 1);
    let stale = t.service.save(owner, change).await.unwrap_err();
    assert_eq!(stale.error_code(), ErrorCode::ConflictVersion);

    // Removing it falls back to the virtual printer.
    t.service.remove(owner, saved.device_id).await.unwrap();
    assert!(!t.service.printer.is_configured());
    t.service.printer.submit("job", b"x").await;
    assert_eq!(t.fallback.submissions(), 1);
    assert_eq!(audit(&t, "device.saved").await.len(), 2);
    assert_eq!(audit(&t, "device.removed").await.len(), 1);
}

#[tokio::test]
async fn bad_or_unusable_configurations_are_refused_by_field_and_never_stored() {
    let t = till().await;
    let owner = &t.shop.owner;
    let e = t.service.save(owner, printer_input(" ")).await.unwrap_err();
    assert!(
        matches!(&e, AppError::DeviceConfig { field, .. } if field == "printer"),
        "{e:?}"
    );
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    let e = t.service.save(owner, printer_input("broken")).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::DeviceUnavailable);
    assert!(e.to_string().contains("COM9"), "{e}");
    assert!(t.service.list(owner).await.unwrap().is_empty());
    assert!(!t.service.printer.is_configured());

    // One main printer per till.
    t.service.save(owner, printer_input("a")).await.unwrap();
    let e = t.service.save(owner, printer_input("b")).await.unwrap_err();
    assert!(
        matches!(&e, AppError::DeviceConfig { field, .. } if field == "role"),
        "{e:?}"
    );
}

#[tokio::test]
async fn managers_configure_devices_and_cashiers_cannot() {
    let t = till().await;
    let cashier = t.shop.as_role(Role::Cashier);
    let e = t.service.save(&cashier, printer_input("a")).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::PermissionDenied);
    assert_eq!(
        t.service.list(&cashier).await.unwrap_err().error_code(),
        ErrorCode::PermissionDenied
    );
    let manager = t.shop.as_role(Role::Manager);
    t.service.save(&manager, printer_input("a")).await.unwrap();
    // Anyone signed in may see whether the printer is ready.
    let view = t.service.printer_view(&cashier).await.unwrap();
    assert!(view.configured && view.drawer);
}

#[tokio::test]
async fn no_sale_needs_a_reason_and_a_managers_approval_for_a_cashier() {
    let t = till().await;
    let owner = &t.shop.owner;
    // No drawer yet: a clear device error.
    let e = t
        .service
        .open_drawer(owner, "change for the float")
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::DeviceUnavailable);

    let mut input = printer_input("epson");
    input.config = json!({ "printer": "epson", "pin": 5 });
    t.service.save(owner, input).await.unwrap();
    let e = t.service.open_drawer(owner, "  ").await.unwrap_err();
    assert!(matches!(&e, AppError::DeviceConfig { field, .. } if field == "reason"));

    let cashier = t.shop.as_role(Role::Cashier);
    let e = t.service.open_drawer(&cashier, "change").await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ApprovalRequired);
    assert!(t.driver.printer("epson").drawer_opens().is_empty());

    t.service
        .open_drawer(owner, "change for the float")
        .await
        .unwrap();
    assert_eq!(t.driver.printer("epson").drawer_opens(), vec![DrawerPin::Pin5]);
    let rows = audit(&t, "cash_drawer.opened").await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].2.contains("change for the float") && rows[0].2.contains("no_sale"));

    // A failed pulse is reported and not audited as opened.
    t.driver
        .printer("epson")
        .fail_next_drawer(DeviceError::Disconnected("printer off".into()));
    let e = t.service.open_drawer(owner, "again").await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::DeviceUnavailable);
    assert_eq!(audit(&t, "cash_drawer.opened").await.len(), 1);
}

#[tokio::test]
async fn the_drawer_opens_after_cash_sales_only() {
    let t = till().await;
    let owner = &t.shop.owner;
    let sale = pos_domain::SaleId::generate(&pos_domain::UuidV7Generator);
    assert_eq!(
        t.service.after_sale(owner, sale, true, true).await,
        DrawerKick::NoDrawer
    );
    t.service.save(owner, printer_input("epson")).await.unwrap();
    assert_eq!(
        t.service.after_sale(owner, sale, false, true).await,
        DrawerKick::NotNeeded
    );
    assert_eq!(
        t.service.after_sale(owner, sale, true, false).await,
        DrawerKick::NotNeeded
    );
    assert_eq!(
        t.service.after_sale(owner, sale, true, true).await,
        DrawerKick::Opened
    );
    assert_eq!(t.driver.printer("epson").drawer_opens().len(), 1);
    let rows = audit(&t, "cash_drawer.opened").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, sale.to_string());
    t.driver.printer("epson").fail_next_drawer(DeviceError::Timeout);
    assert!(matches!(
        t.service.after_sale(owner, sale, true, true).await,
        DrawerKick::Failed(_)
    ));
}

#[tokio::test]
async fn start_up_activates_the_stored_printer_and_survives_a_broken_one() {
    let t = till().await;
    let owner = &t.shop.owner;
    let saved = t.service.save(owner, printer_input("epson")).await.unwrap();
    // A fresh service, as after a restart.
    let s = t.shop.store.clone();
    let restarted = DeviceService {
        store: Arc::new(s.clone()),
        devices: Arc::new(s.clone()),
        driver: t.driver.clone(),
        printer: Arc::new(SwitchablePrinter::new(PrinterHandle {
            printer: t.fallback.clone(),
            drawer: None,
            drawer_pin: DrawerPin::Pin2,
        })),
    };
    restarted.activate(owner.register.register_id).await.unwrap();
    assert!(restarted.printer.is_configured());

    // The stored configuration no longer opens (port gone): fallback, reported, not fatal.
    sqlx::query("UPDATE devices SET config_json = '{\"printer\":\"broken\"}' WHERE device_id = ?")
        .bind(saved.device_id.to_string())
        .execute(s.db().writer())
        .await
        .unwrap();
    let err = restarted.activate(owner.register.register_id).await.unwrap_err();
    assert!(err.contains("COM9"), "{err}");
    assert!(!restarted.printer.is_configured());
}

#[tokio::test]
async fn print_jobs_record_which_printer_took_them() {
    let t = till().await;
    t.service
        .save(&t.shop.owner, printer_input("epson"))
        .await
        .unwrap();
    use pos_device_ports::ReceiptOutputPort;
    assert_eq!(t.service.printer.adapter(), "virtual");
    assert_eq!(t.service.printer.target(), None);
}
