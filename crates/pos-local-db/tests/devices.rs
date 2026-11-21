//! E20-T05 / E20-T11: devices attached to a till. Saving a printer swaps it in for the next
//! print, bad configurations are refused by field and never stored, the drawer opens only
//! for cash and on an audited, approved "no sale". The customer display is saved, audited
//! and tested the same way, and follows a sale. So is the scale (E20-T06).

mod common;

use pos_application::context::AppError;
use pos_application::customer_display::CustomerDisplay;
use pos_application::devices::{
    DeviceConfigProblem, DeviceDriver, DeviceInput, DeviceService, DrawerKick, PrinterHandle,
    SwitchablePrinter,
};
use pos_application::records::DeviceKind;
use pos_application::scale::Scale;
use pos_device_ports::{
    CustomerDisplayPort, DeviceError, DeviceHealth, DisplayFrame, DisplayLine, DrawerPin, PrintResult,
    ScalePort, ScaleReading, Weight,
};
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
    displays: Mutex<HashMap<String, Arc<RecordingDisplay>>>,
    scales: Mutex<HashMap<String, Arc<FixedScale>>>,
}

/// A scale that always reads what it was last given (`unplugged`: an error).
struct FixedScale {
    reading: Mutex<Result<ScaleReading, DeviceError>>,
    health: tokio::sync::watch::Sender<DeviceHealth>,
}

#[async_trait::async_trait]
impl ScalePort for FixedScale {
    fn describe(&self) -> String {
        "COM4 @ 9600, Toledo 8217 (kg)".into()
    }
    async fn read(&self) -> Result<ScaleReading, DeviceError> {
        let r = self.reading.lock().unwrap().clone();
        self.health.send_replace(match &r {
            Ok(_) => DeviceHealth::Ready,
            Err(e) => DeviceHealth::Offline(e.to_string()),
        });
        r
    }
    fn health(&self) -> tokio::sync::watch::Receiver<DeviceHealth> {
        self.health.subscribe()
    }
}

/// A customer display that records every frame it is asked to show.
struct RecordingDisplay {
    frames: Mutex<Vec<DisplayFrame>>,
    health: tokio::sync::watch::Sender<DeviceHealth>,
    unplugged: std::sync::atomic::AtomicBool,
}

impl RecordingDisplay {
    fn last(&self) -> Option<DisplayFrame> {
        self.frames.lock().unwrap().last().cloned()
    }
}

#[async_trait::async_trait]
impl CustomerDisplayPort for RecordingDisplay {
    fn describe(&self) -> String {
        "COM7 @ 9600, ESC/POS".into()
    }
    fn show(&self, frame: DisplayFrame) {
        self.frames.lock().unwrap().push(frame);
    }
    async fn test(&self, frame: DisplayFrame) -> Result<(), DeviceError> {
        if self.unplugged.load(std::sync::atomic::Ordering::SeqCst) {
            self.health
                .send_replace(DeviceHealth::Offline("COM7 not found".into()));
            return Err(DeviceError::Disconnected("COM7 not found".into()));
        }
        self.show(frame);
        self.health.send_replace(DeviceHealth::Ready);
        Ok(())
    }
    fn health(&self) -> tokio::sync::watch::Receiver<DeviceHealth> {
        self.health.subscribe()
    }
}

impl FakeDriver {
    fn scale(&self, name: &str) -> Arc<FixedScale> {
        self.scales
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_insert_with(|| {
                Arc::new(FixedScale {
                    reading: Mutex::new(Ok(ScaleReading::Stable(Weight::from_milligrams(1_234_000)))),
                    health: tokio::sync::watch::channel(DeviceHealth::Unknown).0,
                })
            })
            .clone()
    }

    fn display(&self, name: &str) -> Arc<RecordingDisplay> {
        self.displays
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_insert_with(|| {
                Arc::new(RecordingDisplay {
                    frames: Mutex::new(Vec::new()),
                    health: tokio::sync::watch::channel(DeviceHealth::Unknown).0,
                    unplugged: std::sync::atomic::AtomicBool::new(false),
                })
            })
            .clone()
    }

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
    fn validate(&self, kind: DeviceKind, config: &Value) -> Result<Value, DeviceConfigProblem> {
        if kind == DeviceKind::Scale {
            return match config.get("scale").and_then(Value::as_str) {
                Some(n) if !n.trim().is_empty() => Ok(json!({ "scale": n.trim() })),
                _ => Err(DeviceConfigProblem {
                    field: "serial.port".into(),
                    message: "choose a port".into(),
                }),
            };
        }
        if kind == DeviceKind::CustomerDisplay {
            return match config.get("display").and_then(Value::as_str) {
                Some(n) if !n.trim().is_empty() => Ok(json!({ "display": n.trim() })),
                _ => Err(DeviceConfigProblem {
                    field: "serial.port".into(),
                    message: "choose a port".into(),
                }),
            };
        }
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

    async fn open_scale(&self, config: &Value) -> Result<Arc<dyn ScalePort>, DeviceError> {
        match config["scale"].as_str().unwrap_or_default() {
            "gone" => Err(DeviceError::Disconnected("COM4 is not present".into())),
            name => Ok(self.scale(name)),
        }
    }

    async fn open_display(&self, config: &Value) -> Result<Arc<dyn CustomerDisplayPort>, DeviceError> {
        match config["display"].as_str().unwrap_or_default() {
            "gone" => Err(DeviceError::Disconnected("COM8 is not present".into())),
            name => Ok(self.display(name)),
        }
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
        displays: Mutex::new(HashMap::new()),
        scales: Mutex::new(HashMap::new()),
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
        display: Arc::new(CustomerDisplay::new(std::time::Duration::from_millis(50))),
        scale: Arc::new(Scale::default()),
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
        display: Arc::new(CustomerDisplay::default()),
        scale: Arc::new(Scale::default()),
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

fn display_input(name: &str) -> DeviceInput {
    DeviceInput {
        device_id: None,
        kind: DeviceKind::CustomerDisplay,
        role: "main".into(),
        name: "Pole display".into(),
        config: json!({ "display": name }),
        enabled: true,
        base_version: None,
    }
}

fn pair(l: &str, r: &str) -> DisplayLine {
    DisplayLine::Pair(l.into(), r.into())
}

#[tokio::test]
async fn a_customer_display_is_saved_audited_and_tested_like_the_other_devices() {
    let t = till().await;
    let owner = &t.shop.owner;
    t.service.display.set_shop("Kamal Stores");
    // A cashier may not set one up.
    let refused = t
        .service
        .save(&t.shop.as_role(Role::Cashier), display_input("pole"))
        .await
        .unwrap_err();
    assert_eq!(refused.error_code(), ErrorCode::PermissionDenied);
    let bad = t.service.save(owner, display_input(" ")).await.unwrap_err();
    assert!(matches!(bad, AppError::DeviceConfig { ref field, .. } if field == "serial.port"));

    let saved = t.service.save(owner, display_input("pole")).await.unwrap();
    assert_eq!(saved.kind, DeviceKind::CustomerDisplay);
    let saves = audit(&t, "device.saved").await;
    assert!(saves.last().unwrap().2.contains("customer_display"));
    let pole = t.driver.display("pole");
    // Put to work at once, showing the welcome line.
    assert_eq!(
        pole.last(),
        Some(DisplayFrame::new(
            DisplayLine::Centered("WELCOME".into()),
            DisplayLine::Centered("Kamal Stores".into())
        ))
    );
    let view = t.service.display_view(owner).unwrap();
    assert!(view.configured);
    assert_eq!(view.target.as_deref(), Some("COM7 @ 9600, ESC/POS"));

    // Test display: shown and audited; a cashier may not.
    assert_eq!(
        t.service.test_display(owner).await.unwrap(),
        Ok("COM7 @ 9600, ESC/POS".to_owned())
    );
    assert_eq!(
        pole.last().unwrap().lower,
        DisplayLine::Text("12345678901234567890".into())
    );
    assert_eq!(
        t.service
            .test_display(&t.shop.as_role(Role::Cashier))
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::PermissionDenied
    );
    // The welcome line comes back after the idle time.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        pole.last().unwrap().upper,
        DisplayLine::Centered("WELCOME".into())
    );

    // Unplugged: the test says so (an answer, not an error) and the status shows it.
    pole.unplugged.store(true, std::sync::atomic::Ordering::SeqCst);
    let failed = t.service.test_display(owner).await.unwrap();
    assert!(failed.unwrap_err().contains("COM7 not found"));
    assert!(matches!(
        t.service.display_view(owner).unwrap().health,
        DeviceHealth::Offline(_)
    ));
    let tests = audit(&t, "device.test_displayed").await;
    assert_eq!(tests.len(), 2);
    assert!(tests[0].2.contains("\"outcome\":\"shown\""), "{}", tests[0].2);
    assert!(tests[1].2.contains("failed"), "{}", tests[1].2);

    // Removed: nothing to show any more.
    t.service.remove(owner, saved.device_id).await.unwrap();
    assert!(!t.service.display_view(owner).unwrap().configured);
    assert!(t.service.test_display(owner).await.is_err());
}

#[tokio::test]
async fn the_customer_display_follows_a_sale() {
    use pos_application::cart::CartService;
    use pos_application::catalog::CatalogService;
    use pos_application::checkout::{CheckoutService, CompleteSale};
    use pos_application::scan::ScanSource;
    use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
    use pos_domain::settlement::{TenderInput, TenderKind};
    use pos_domain::{Money, ScanEventId, UuidV7Generator};

    let t = till().await;
    let owner = &t.shop.owner;
    t.service.save(owner, display_input("pole")).await.unwrap();
    let pole = t.driver.display("pole");
    let s = t.shop.store.clone();
    let carts = Arc::new(CartService::new(
        Arc::new(s.clone()),
        Arc::new(s.clone()),
        Arc::new(s.clone()),
    ));
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    let mut v = VariantInput::simple(Money::from_minor(450));
    v.sku = Some("CAFE".into());
    catalog
        .create(
            owner,
            ProductInput {
                name: "Café crème, large cup".into(),
                description: None,
                category_id: None,
                product_type: ProductType::Stock,
                unit: Unit::Ea,
                tax_category_id: t.shop.tax_category,
                quick_select: false,
                variants: vec![v],
            },
        )
        .await
        .unwrap();
    let display = &t.service.display;
    let scan = || async {
        let id = ScanEventId::generate(&UuidV7Generator);
        match carts.scan(owner, "CAFE", id, ScanSource::Manual).await.unwrap() {
            pos_application::scan::ScanOutcome::Added { view, change, .. } => {
                let line = match change {
                    pos_domain::cart::CartChange::LineAdded(l)
                    | pos_domain::cart::CartChange::QuantityIncreased { line: l, .. } => l,
                    other => panic!("{other:?}"),
                };
                (view, line)
            }
            other => panic!("{other:?}"),
        }
    };
    let (view, line) = scan().await;
    display.show_cart(&view, Some(line));
    assert_eq!(
        pole.last(),
        Some(DisplayFrame::new(
            pair("Café crème, large cup", "4.50"),
            pair("TOTAL", "4.50")
        ))
    );
    let (view, line) = scan().await;
    display.show_cart(&view, Some(line));
    assert_eq!(
        pole.last(),
        Some(DisplayFrame::new(
            pair("Café crème, large cup", "9.00"),
            pair("TOTAL", "9.00")
        ))
    );
    display.show_total_due(&view);
    assert_eq!(
        pole.last(),
        Some(DisplayFrame::new(
            DisplayLine::Text("TOTAL DUE".into()),
            pair("", "9.00")
        ))
    );

    let checkout = CheckoutService {
        carts: carts.clone(),
        store: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        stock: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
    };
    let done = checkout
        .complete_sale(
            owner,
            CompleteSale {
                cart_id: view.cart.id,
                revision: view.cart.revision,
                tenders: vec![TenderInput {
                    kind: TenderKind::Cash,
                    amount: Money::from_minor(2000),
                    external_reference: None,
                }],
            },
        )
        .await
        .unwrap();
    display.show_sale_done(&done.sale);
    assert_eq!(
        pole.last(),
        Some(DisplayFrame::new(pair("PAID", "20.00"), pair("CHANGE", "11.00")))
    );
    // Idle: the welcome line returns by itself.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        pole.last().unwrap().upper,
        DisplayLine::Centered("WELCOME".into())
    );
    // An empty cart shows the welcome line too.
    let empty = carts.current(owner).await.unwrap();
    let before = pole.frames.lock().unwrap().len();
    display.show_cart(&empty, None);
    assert_eq!(pole.frames.lock().unwrap().len(), before + 1);
    assert_eq!(
        pole.last().unwrap().upper,
        DisplayLine::Centered("WELCOME".into())
    );
}

#[tokio::test]
async fn start_up_activates_the_stored_display_and_survives_a_missing_one() {
    let t = till().await;
    let owner = &t.shop.owner;
    let saved = t.service.save(owner, display_input("pole")).await.unwrap();
    let fresh = CustomerDisplay::default();
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
        display: Arc::new(fresh),
        scale: Arc::new(Scale::default()),
    };
    restarted
        .activate_display(owner.register.register_id)
        .await
        .unwrap();
    assert!(restarted.display.port().is_some());

    sqlx::query("UPDATE devices SET config_json = '{\"display\":\"gone\"}' WHERE device_id = ?")
        .bind(saved.device_id.to_string())
        .execute(s.db().writer())
        .await
        .unwrap();
    let err = restarted
        .activate_display(owner.register.register_id)
        .await
        .unwrap_err();
    assert!(err.contains("COM8"), "{err}");
    assert!(restarted.display.port().is_none());
}

fn scale_input(name: &str) -> DeviceInput {
    DeviceInput {
        device_id: None,
        kind: DeviceKind::Scale,
        role: "main".into(),
        name: "Counter scale".into(),
        config: json!({ "scale": name }),
        enabled: true,
        base_version: None,
    }
}

#[tokio::test]
async fn a_scale_is_saved_audited_tested_and_activated_after_a_restart() {
    use pos_application::devices::ScaleTest;
    let t = till().await;
    let owner = &t.shop.owner;
    assert!(!t.service.scale_view(owner).unwrap().configured);
    assert!(t.service.test_scale(owner).await.is_err(), "no scale yet");
    let refused = t
        .service
        .save(&t.shop.as_role(Role::Cashier), scale_input("counter"))
        .await
        .unwrap_err();
    assert_eq!(refused.error_code(), ErrorCode::PermissionDenied);
    let bad = t.service.save(owner, scale_input(" ")).await.unwrap_err();
    assert!(matches!(bad, AppError::DeviceConfig { ref field, .. } if field == "serial.port"));

    let saved = t.service.save(owner, scale_input("counter")).await.unwrap();
    assert_eq!(saved.kind, DeviceKind::Scale);
    assert_eq!(saved.name, "Counter scale");
    assert!(
        audit(&t, "device.saved")
            .await
            .last()
            .unwrap()
            .2
            .contains("\"scale\"")
    );
    // Put to work at once: the cart service weighs with it from now on.
    assert!(t.service.scale.is_configured());
    let view = t.service.scale_view(&t.shop.as_role(Role::Cashier)).unwrap();
    assert_eq!(view.target.as_deref(), Some("COM4 @ 9600, Toledo 8217 (kg)"));
    assert_eq!(view.health, DeviceHealth::Unknown);

    // Test scale: the reading, audited; a cashier may not.
    assert_eq!(
        t.service.test_scale(owner).await.unwrap(),
        ScaleTest::Read(ScaleReading::Stable(Weight::from_milligrams(1_234_000)))
    );
    assert_eq!(t.service.scale_view(owner).unwrap().health, DeviceHealth::Ready);
    assert_eq!(
        t.service
            .test_scale(&t.shop.as_role(Role::Cashier))
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::PermissionDenied
    );
    // Unplugged: an answer, not an error, and the status says so.
    *t.driver.scale("counter").reading.lock().unwrap() =
        Err(DeviceError::Disconnected("COM4 not found".into()));
    let ScaleTest::Failed(why) = t.service.test_scale(owner).await.unwrap() else {
        panic!("not failed");
    };
    assert!(why.contains("COM4 not found"));
    assert!(matches!(
        t.service.scale_view(owner).unwrap().health,
        DeviceHealth::Offline(_)
    ));
    let tests = audit(&t, "device.test_weighed").await;
    assert_eq!(tests.len(), 2);
    assert!(tests[0].2.contains("stable 1234"), "{}", tests[0].2);
    assert!(tests[1].2.contains("failed"), "{}", tests[1].2);

    // After a restart the stored scale is used again; a vanished one is reported, not fatal.
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
        display: Arc::new(CustomerDisplay::default()),
        scale: Arc::new(Scale::default()),
    };
    restarted
        .activate_scale(owner.register.register_id)
        .await
        .unwrap();
    assert!(restarted.scale.is_configured());
    let gone = DeviceInput {
        device_id: Some(saved.device_id),
        base_version: Some(saved.version),
        ..scale_input("counter")
    };
    let saved = t.service.save(owner, gone).await.unwrap();
    sqlx::query("UPDATE devices SET config_json = '{\"scale\":\"gone\"}' WHERE device_id = ?")
        .bind(saved.device_id.to_string())
        .execute(t.shop.store.db().writer())
        .await
        .unwrap();
    let err = restarted
        .activate_scale(owner.register.register_id)
        .await
        .unwrap_err();
    assert!(err.contains("COM4 is not present"), "{err}");
    assert!(!restarted.scale.is_configured());

    // Removed: no scale, audited.
    t.service.remove(owner, saved.device_id).await.unwrap();
    assert!(!t.service.scale.is_configured());
    assert_eq!(audit(&t, "device.removed").await.len(), 1);
}
