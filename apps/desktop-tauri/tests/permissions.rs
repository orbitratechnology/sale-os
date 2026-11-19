//! E05-T03 / AT-11: every IPC command, called directly (bypassing the UI) as each role, is
//! allowed or refused exactly as the role matrix says (docs/backlog/07 §1.2).
//!
//! The table below must list every registered command. Adding a command without deciding
//! what it requires fails `every_command_is_classified`.

use desktop_tauri_lib::{AppServices, COMMANDS};
use pos_application::auth::{Argon2PinHasher, PinHasher};
use pos_domain::{Permission, Role, role_has};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tauri::Manager;
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{WebviewWindow, WebviewWindowBuilder};

/// What a command requires.
#[derive(Clone, Copy, Debug)]
enum Need {
    /// Callable without a session (bootstrap, sign-in, diagnostics).
    Public,
    /// Only before setup; refused afterwards for everyone.
    SetupOnly,
    /// Any signed-in user.
    Session,
    /// A signed-in user whose role holds the permission.
    Perm(Permission),
}

const UNKNOWN_ID: &str = "0190a5b0-0000-7000-8000-000000000000";

/// Every command, what it needs, and well-formed arguments. Arguments use unknown IDs and
/// stale revisions, so an *allowed* call fails harmlessly after authorization (not found,
/// revision mismatch) and changes nothing.
fn cases() -> Vec<(&'static str, Need, Value)> {
    use Need::*;
    use Permission::*;
    let product = json!({"product": {
        "name": "X", "description": null, "categoryId": null, "productType": "stock", "unit": "ea",
        "taxCategoryId": UNKNOWN_ID, "quickSelect": false,
        "variants": [{"variantId": null, "sku": null, "options": {}, "priceMinor": "100", "costMinor": null,
            "trackStock": true, "lowStockThresholdMilli": null, "scanQtyMilli": null, "barcodes": []}]
    }});
    let mut update = product.clone();
    update["productId"] = json!(UNKNOWN_ID);
    update["baseVersion"] = json!(1);
    vec![
        ("app_info", Public, json!({})),
        (
            "dev_money_add",
            Public,
            json!({"aMinor": "1", "bMinor": "2", "currency": "GBP"}),
        ),
        ("app_bootstrap_state", Public, json!({})),
        (
            "setup_create_workspace",
            SetupOnly,
            json!({"input": workspace_input("Again", "9999")}),
        ),
        ("auth_list_users", Public, json!({})),
        ("auth_login", Public, json!({"userId": UNKNOWN_ID, "pin": "0000"})),
        // Exercised explicitly at the end of each role's run.
        ("auth_logout", Session, Value::Null),
        // Ends the session too; not invoked mid-run (null args), checked in the IPC test below.
        ("auth_lock", Session, Value::Null),
        ("sync_retry_now", Session, Value::Null),
        ("sync_status", Session, Value::Null),
        (
            "sync_link_start",
            Perm(DeviceLink),
            json!({"email": "owner@example.com", "password": "not a real password"}),
        ),
        (
            "sync_link_complete",
            Perm(DeviceLink),
            json!({"mode": "create", "businessId": null, "replaceExisting": false}),
        ),
        ("sync_repair_queue", Perm(SyncManage), json!({})),
        (
            "sync_repair_action",
            Perm(SyncManage),
            json!({"action": "view_payload", "eventId": UNKNOWN_ID}),
        ),
        ("sale_sync_state", Session, json!({"saleId": UNKNOWN_ID})),
        ("sync_conflicts_list", Perm(ConflictResolve), json!({})),
        (
            "sync_conflict_resolve",
            Perm(ConflictResolve),
            json!({"conflictId": UNKNOWN_ID, "choices": [], "identifiers": null}),
        ),
        (
            "auth_change_pin",
            Session,
            json!({"currentPin": "0000", "newPin": "1111"}),
        ),
        (
            "auth_request_approval",
            Session,
            json!({"approverUserId": UNKNOWN_ID, "pin": "0000", "scope": {"permission": "cart_void", "entityId": "x", "maxPpm": null}}),
        ),
        // Owner only; a wrong PIN, so the permitted call is refused for the PIN, not the role.
        ("register_go_live", Perm(SettingsEdit), json!({"pinCode": "0000"})),
        ("settings_get", Perm(SettingsEdit), json!({})),
        // An unknown key: permitted roles get VALIDATION_FAILED, so nothing changes.
        (
            "settings_update",
            Perm(SettingsEdit),
            json!({"key": "harness.none", "valueJson": "true", "baseVersion": 1}),
        ),
        ("app_preferences", Session, json!({})),
        ("staff_list", Perm(StaffManage), json!({})),
        (
            "staff_create",
            Perm(StaffManage),
            json!({"input": {"displayName": "Temp", "role": "cashier", "pin": "4444"}}),
        ),
        (
            "staff_update",
            Perm(StaffManage),
            json!({"input": {"userId": UNKNOWN_ID, "baseVersion": 1, "displayName": "Y", "role": null}}),
        ),
        (
            "staff_deactivate",
            Perm(StaffManage),
            json!({"userId": UNKNOWN_ID, "baseVersion": 1}),
        ),
        (
            "staff_reset_pin",
            Perm(StaffManage),
            json!({"userId": UNKNOWN_ID, "baseVersion": 1, "pin": "5555"}),
        ),
        ("tax_list", Perm(CatalogView), json!({})),
        ("categories_list", Perm(CatalogView), json!({})),
        // An empty name: permitted roles get VALIDATION_FAILED and nothing is written.
        (
            "category_save",
            Perm(CatalogEdit),
            json!({"categoryId": null, "name": " ", "parentId": null, "baseVersion": null}),
        ),
        (
            "category_archive",
            Perm(CatalogEdit),
            json!({"categoryId": UNKNOWN_ID, "baseVersion": 1}),
        ),
        ("tax_config_get", Perm(SettingsEdit), json!({})),
        (
            "tax_rate_save",
            Perm(SettingsEdit),
            json!({"taxRateId": null, "name": " ", "ratePpm": 0, "from": null, "to": null, "baseVersion": null}),
        ),
        (
            "tax_rate_archive",
            Perm(SettingsEdit),
            json!({"taxRateId": UNKNOWN_ID, "baseVersion": 1}),
        ),
        (
            "tax_category_save",
            Perm(SettingsEdit),
            json!({"taxCategoryId": null, "name": " ", "rateIds": [], "isDefault": false, "baseVersion": null}),
        ),
        ("business_profile_get", Perm(SettingsEdit), json!({})),
        // No dialog in tests: an allowed call answers null (cancelled).
        ("import_products_open", Perm(CatalogImport), json!({})),
        (
            "import_products_validate",
            Perm(CatalogImport),
            json!({"jobId": UNKNOWN_ID, "mapping": [{"column": "name", "index": 0}, {"column": "price", "index": 1}], "onDuplicate": "skip"}),
        ),
        (
            "import_products_commit",
            Perm(CatalogImport),
            json!({"jobId": UNKNOWN_ID}),
        ),
        (
            "import_products_error_report",
            Perm(CatalogImport),
            json!({"jobId": UNKNOWN_ID}),
        ),
        (
            "import_products_cancel",
            Perm(CatalogImport),
            json!({"jobId": UNKNOWN_ID}),
        ),
        // Stale versions: refused for the version, after the role check.
        (
            "business_profile_update",
            Perm(SettingsEdit),
            json!({"input": {"name": "X", "legalName": null, "taxRegistrationNo": null, "addressLines": [],
                   "phone": null, "email": null, "registerName": "X"}, "businessVersion": 999, "registerVersion": 999}),
        ),
        (
            "product_list",
            Perm(CatalogView),
            json!({"filter": {"query": "", "status": "active", "trackStock": null, "categoryId": null}, "offset": 0, "limit": 10}),
        ),
        ("product_get", Perm(CatalogView), json!({"productId": UNKNOWN_ID})),
        ("product_create", Perm(CatalogEdit), product),
        ("product_update", Perm(CatalogEdit), update),
        (
            "product_archive",
            Perm(CatalogEdit),
            json!({"productId": UNKNOWN_ID, "baseVersion": 1}),
        ),
        ("catalog_lookup_code", Perm(CatalogView), json!({"code": "NOPE"})),
        (
            "catalog_search",
            Perm(CatalogView),
            json!({"query": "cola", "limit": 5}),
        ),
        (
            "scan_submit",
            Perm(SaleComplete),
            json!({"scanEventId": UNKNOWN_ID, "raw": "NOPE", "source": "scanner"}),
        ),
        ("cart_current", Perm(SaleComplete), json!({})),
        ("cart_recovery", Perm(SaleComplete), json!({})),
        ("cart_recovery_resume", Perm(SaleComplete), json!({})),
        ("cart_recovery_discard", Perm(CartClear), json!({"revision": 0})),
        (
            "cart_add_variant",
            Perm(SaleComplete),
            json!({"variantId": UNKNOWN_ID, "qtyMilli": null, "via": "search"}),
        ),
        (
            "cart_set_quantity",
            Perm(SaleComplete),
            json!({"lineId": UNKNOWN_ID, "qtyMilli": "2000"}),
        ),
        (
            "cart_remove_line",
            Perm(SaleComplete),
            json!({"lineId": UNKNOWN_ID}),
        ),
        (
            "cart_set_line_discount",
            Perm(CartLineDiscount),
            json!({"lineId": UNKNOWN_ID, "discount": {"kind": "percent", "ratePpm": 50000, "reason": null}, "approvalTokenId": null}),
        ),
        (
            "cart_set_order_discount",
            Perm(CartOrderDiscount),
            json!({"discount": null, "approvalTokenId": null}),
        ),
        (
            "cart_override_price",
            Perm(PriceOverride),
            json!({"lineId": UNKNOWN_ID, "unitPriceMinor": "50", "reason": "harness", "approvalTokenId": null}),
        ),
        // The cart holds a line, so voiding needs CartVoid (an empty cart can always be closed).
        (
            "cart_void",
            Perm(CartVoid),
            json!({"revision": 0, "reason": "harness", "approvalTokenId": null}),
        ),
        ("cart_clear", Perm(CartClear), json!({"revision": 0})),
        // POS-012. A stale revision: allowed roles get past the permission check, and the
        // harness's cart keeps its line.
        (
            "cart_park",
            Perm(SaleComplete),
            json!({"revision": 99999, "label": "harness"}),
        ),
        ("cart_parked_list", Perm(SaleComplete), json!({})),
        ("cart_resume", Perm(SaleComplete), json!({"cartId": UNKNOWN_ID})),
        (
            "cart_discard_parked",
            Perm(CartVoid),
            json!({"cartId": UNKNOWN_ID, "reason": "harness", "approvalTokenId": null}),
        ),
        (
            "checkout_complete",
            Perm(SaleComplete),
            json!({"cartId": UNKNOWN_ID, "revision": 0, "tenders": [{"kind": "cash", "amountMinor": "100", "reference": null}]}),
        ),
        (
            "receipt_get",
            Perm(SalesView),
            json!({"saleId": UNKNOWN_ID, "widthMm": 58}),
        ),
        ("receipt_print", Perm(SalesView), json!({"saleId": UNKNOWN_ID})),
        ("print_job_retry", Perm(SalesView), json!({"jobId": UNKNOWN_ID})),
        (
            "print_job_resolve",
            Perm(SalesView),
            json!({"jobId": UNKNOWN_ID, "printed": true}),
        ),
        (
            "return_find",
            Perm(SalesView),
            json!({"receiptNumber": "R01-999999"}),
        ),
        (
            "return_preview",
            Perm(SalesView),
            json!({"saleId": UNKNOWN_ID, "lines": [{"saleLineId": UNKNOWN_ID, "quantityMilli": "1000", "disposition": "restock"}]}),
        ),
        // A cashier gets APPROVAL_REQUIRED (the matrix's "approval" cell).
        (
            "return_create",
            Perm(ReturnCreate),
            json!({"saleId": UNKNOWN_ID, "lines": [{"saleLineId": UNKNOWN_ID, "quantityMilli": "1000", "disposition": "restock"}],
                   "reason": "changed_mind", "note": null, "refunds": [{"kind": "cash", "amountMinor": "100", "reference": null}], "approvalTokenId": null}),
        ),
        (
            "return_receipt_get",
            Perm(SalesView),
            json!({"returnId": UNKNOWN_ID, "widthMm": null}),
        ),
        (
            "return_receipt_print",
            Perm(SalesView),
            json!({"returnId": UNKNOWN_ID}),
        ),
        (
            "receipt_export_pdf",
            Perm(SalesView),
            json!({"saleId": UNKNOWN_ID, "returnId": null, "paper": "a4"}),
        ),
        (
            "sales_list",
            Perm(SalesView),
            json!({"filter": {"from": null, "to": null, "receipt": null, "cashierUserId": null, "tender": null}, "after": null, "limit": null}),
        ),
        ("sales_get", Perm(SalesView), json!({"saleId": UNKNOWN_ID})),
        // An adjustment would need InventoryAdjust instead; the matrix gives both to the same roles.
        (
            "inventory_post_document",
            Perm(InventoryReceive),
            json!({"draft": {"docType": "receipt", "reason": null, "supplierName": null, "reference": null, "note": null,
                             "lines": [{"variantId": UNKNOWN_ID, "quantityMilli": "1000", "unitCostMinor": null}]}}),
        ),
        ("stock_verify", Perm(InventoryView), json!({})),
        (
            "audit_list",
            Perm(AuditView),
            json!({"filter": {"action": null, "userId": null, "from": null, "to": null}, "before": null, "limit": 10}),
        ),
        ("stock_overview", Perm(InventoryView), json!({"lowOnly": true})),
        (
            "stock_history",
            Perm(InventoryView),
            json!({"variantId": UNKNOWN_ID, "limit": 10}),
        ),
        // Owner only (diagnostics).
        ("stock_rebuild", Perm(SettingsEdit), json!({})),
        (
            "reports_sales_summary",
            Perm(ReportsView),
            json!({"range": {"from": "2026-01-01", "to": "2026-01-31"}}),
        ),
        (
            "reports_tender_mix",
            Perm(ReportsView),
            json!({"range": {"from": "2026-01-01", "to": "2026-01-31"}}),
        ),
        (
            "reports_top_products",
            Perm(ReportsView),
            json!({"range": {"from": "2026-01-01", "to": "2026-01-31"}, "by": "value", "limit": 10}),
        ),
        ("reports_stock", Perm(ReportsView), json!({"lowOnly": false})),
        ("backup_status", Perm(SettingsEdit), json!({})),
        ("backup_now", Perm(SettingsEdit), json!({})),
        ("backup_choose_folder", Perm(SettingsEdit), json!({})),
        ("backup_clear_folder", Perm(SettingsEdit), json!({})),
        (
            "backup_verify",
            Perm(SettingsEdit),
            json!({"backupId": "none.db"}),
        ),
        (
            "backup_restore",
            Perm(SettingsEdit),
            json!({"backupId": "none.db", "confirmLoss": false}),
        ),
        ("data_export_full", Perm(SettingsEdit), json!({})),
        ("security_recovery_key", Perm(SettingsEdit), json!({})),
        ("diagnostics_export", Session, json!({})),
        ("security_recovery_key_confirmed", Perm(SettingsEdit), json!({})),
        // Only acts on a locked database; refused (as a validation error) otherwise.
        (
            "db_unlock",
            Public,
            json!({"recoveryKey": "00000000-00000000-00000000-00000000-00000000-00000000-00000000-00000000"}),
        ),
        (
            "reports_cloud_sales",
            Perm(ReportsView),
            json!({"range": {"from": "2026-01-01", "to": "2026-01-31"}, "groupBy": "day"}),
        ),
        // Products and stock need CatalogExport instead; the matrix gives both to the same roles.
        (
            "export_csv",
            Perm(ReportsExport),
            json!({"dataset": "transactions", "range": {"from": "2026-01-01", "to": "2026-01-31"}}),
        ),
        ("cash_session_current", Perm(CashSessionOpen), json!({})),
        // A negative float is refused after the permission check: nothing is opened.
        (
            "cash_session_open",
            Perm(CashSessionOpen),
            json!({"floatMinor": "-1"}),
        ),
        // No session is open, so a permitted close fails with CASH_SESSION_REQUIRED.
        (
            "cash_session_close",
            Perm(CashSessionClose),
            json!({"countedMinor": "0", "note": null}),
        ),
        (
            "cash_session_z_get",
            Perm(CashSessionViewVariance),
            json!({"sessionId": null, "widthMm": null}),
        ),
        (
            "cash_session_z_print",
            Perm(CashSessionViewVariance),
            json!({"sessionId": null}),
        ),
    ]
}

fn workspace_input(name: &str, pin: &str) -> Value {
    json!({
        "businessName": name, "legalName": null, "addressLines": ["1 High St"], "phone": null, "email": null,
        "countryCode": "GB", "timezone": "Europe/London", "currencyCode": "GBP", "pricesIncludeTax": true,
        "locationName": "Main", "registerName": "Till 1", "ownerName": "Olu", "ownerPin": pin,
        "taxRates": [{"name": "VAT 20%", "ratePpm": 200000}]
    })
}

/// RET-002 (E12-T04): a completed sale is never changed or removed; the only way back is a
/// return. No command may void, edit or delete a sale, a return or a tender.
#[test]
fn nothing_can_void_or_delete_a_completed_sale() {
    for cmd in COMMANDS {
        let touches_records = ["sale_", "sales_", "return_", "tender_"]
            .iter()
            .any(|p| cmd.starts_with(p));
        let destructive = ["void", "delete", "update", "edit", "cancel"]
            .iter()
            .any(|w| cmd.contains(w));
        assert!(
            !(touches_records && destructive),
            "{cmd} would change a completed record"
        );
    }
}

#[test]
fn every_command_is_classified() {
    let classified: BTreeSet<&str> = cases().iter().map(|c| c.0).collect();
    let registered: BTreeSet<&str> = COMMANDS.iter().copied().collect();
    assert_eq!(
        classified, registered,
        "every IPC command needs an entry in the permission table"
    );
    // The registry the UI is generated from is the same list (no command outside the ACL).
    let bindings = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../desktop-ui/src/ipc/bindings.ts"
    ))
    .unwrap();
    for cmd in COMMANDS {
        assert!(
            bindings.contains(&format!("__TAURI_INVOKE(\"{cmd}\"")),
            "{cmd} missing from the generated bindings"
        );
    }
}

fn app_origin() -> tauri::Url {
    let origin = if cfg!(windows) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    origin.parse().expect("valid origin")
}

fn invoke(w: &WebviewWindow<MockRuntime>, cmd: &str, body: Value) -> Result<Value, Value> {
    get_ipc_response(
        w,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: app_origin(),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|r| match r {
        InvokeResponseBody::Json(s) => serde_json::from_str(&s).unwrap(),
        InvokeResponseBody::Raw(_) => Value::Null,
    })
}

/// The catalogued code of a refusal. A plain string error means the arguments did not
/// deserialize: a broken table entry, which must not pass as a refusal.
fn code(cmd: &str, res: &Result<Value, Value>) -> Option<String> {
    match res {
        Ok(_) => None,
        Err(e) => Some(
            e["code"]
                .as_str()
                .unwrap_or_else(|| panic!("{cmd}: not an IpcError (bad arguments?): {e}"))
                .to_owned(),
        ),
    }
}

const REFUSED: [&str; 2] = ["PERMISSION_DENIED", "APPROVAL_REQUIRED"];

fn start() -> (
    tempfile::TempDir,
    tauri::App<MockRuntime>,
    WebviewWindow<MockRuntime>,
) {
    start_with(None)
}

fn start_with(
    dialog: Option<Arc<dyn desktop_tauri_lib::FileDialog>>,
) -> (
    tempfile::TempDir,
    tauri::App<MockRuntime>,
    WebviewWindow<MockRuntime>,
) {
    let dir = tempfile::tempdir().unwrap();
    // Cheap Argon2 parameters: this test signs in many times. The app uses ADR-013's cost.
    let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));
    let mut services =
        tauri::async_runtime::block_on(AppServices::start_with_hasher(dir.path(), None, hasher));
    if let Some(d) = dialog {
        services = services.with_file_dialog(d);
    }
    let app = mock_builder()
        .invoke_handler(desktop_tauri_lib::guarded_invoke_handler(
            &desktop_tauri_lib::specta_builder::<MockRuntime>(),
        ))
        .build(tauri::generate_context!())
        .unwrap();
    app.manage(services);
    let w = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    (dir, app, w)
}

#[test]
fn each_role_gets_exactly_what_the_matrix_grants() {
    let (_dir, _app, w) = start();

    // Before setup there is no register: nothing that needs a session can run.
    for (cmd, need, args) in cases() {
        if matches!(need, Need::Session | Need::Perm(_)) && !args.is_null() {
            let res = invoke(&w, cmd, args);
            assert_eq!(
                code(cmd, &res).as_deref(),
                Some("SESSION_LOCKED"),
                "{cmd} before setup"
            );
        }
    }

    // Set up, sign the owner in, and create the other roles and a product, all over IPC.
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    let owner = created["ownerUserId"].as_str().unwrap().to_owned();
    invoke(&w, "auth_login", json!({"userId": owner, "pin": "1234"})).unwrap();
    let mut users = vec![(Role::Owner, owner, "1234")];
    for (role, name, pin) in [(Role::Manager, "Mia", "2222"), (Role::Cashier, "Cal", "3333")] {
        let s = invoke(
            &w,
            "staff_create",
            json!({"input": {"displayName": name, "role": role, "pin": pin}}),
        )
        .unwrap();
        users.push((role, s["userId"].as_str().unwrap().to_owned(), pin));
    }
    let tax = invoke(&w, "tax_list", json!({})).unwrap()[0]["taxCategoryId"].clone();
    let mut product = cases().into_iter().find(|c| c.0 == "product_create").unwrap().2;
    product["product"]["taxCategoryId"] = tax;
    product["product"]["variants"][0]["sku"] = json!("COLA");
    invoke(&w, "product_create", product).unwrap();
    let scanned = invoke(
        &w,
        "scan_submit",
        json!({"scanEventId": "0190a5b0-0000-7000-8000-000000000001", "raw": "COLA", "source": "manual"}),
    )
    .unwrap();
    assert_eq!(scanned["outcome"], "added");

    for (role, user_id, pin) in &users {
        invoke(&w, "auth_login", json!({"userId": user_id, "pin": pin})).unwrap();
        for (cmd, need, args) in cases() {
            if args.is_null() {
                continue;
            }
            let res = invoke(&w, cmd, args);
            let got = code(cmd, &res);
            match need {
                Need::Public | Need::Session => assert!(
                    !matches!(
                        got.as_deref(),
                        Some("SESSION_LOCKED" | "PERMISSION_DENIED" | "APPROVAL_REQUIRED")
                    ),
                    "{role:?} {cmd}: {res:?}"
                ),
                Need::SetupOnly => assert_eq!(got.as_deref(), Some("PERMISSION_DENIED"), "{role:?} {cmd}"),
                Need::Perm(p) if role_has(*role, p) => assert!(
                    !matches!(
                        got.as_deref(),
                        Some("SESSION_LOCKED" | "PERMISSION_DENIED" | "APPROVAL_REQUIRED")
                    ),
                    "{role:?} holds {p:?} but {cmd} refused: {res:?}"
                ),
                Need::Perm(p) => assert!(
                    got.as_deref().is_some_and(|c| REFUSED.contains(&c)),
                    "{role:?} lacks {p:?} but {cmd} gave {res:?}"
                ),
            }
        }
        // Signing out locks everything that needs a session.
        invoke(&w, "auth_logout", json!({})).unwrap();
        for (cmd, need, args) in cases() {
            if matches!(need, Need::Session | Need::Perm(_)) && !args.is_null() {
                let res = invoke(&w, cmd, args);
                assert_eq!(
                    code(cmd, &res).as_deref(),
                    Some("SESSION_LOCKED"),
                    "{cmd} after {role:?} logout"
                );
            }
        }
    }

    // Nothing the refused calls attempted happened: the cart still has its one line.
    let owner = &users[0];
    invoke(&w, "auth_login", json!({"userId": owner.1, "pin": owner.2})).unwrap();
    let cart = invoke(&w, "cart_current", json!({})).unwrap();
    assert_eq!(cart["lines"].as_array().unwrap().len(), 1);
    assert_eq!(cart["lines"][0]["quantityMilli"], "1000");
}

#[test]
fn a_failed_sign_in_keeps_the_current_session() {
    // Regression: auth_login used to end the current session before checking the PIN.
    let (_dir, app, w) = start();
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    let owner = created["ownerUserId"].as_str().unwrap().to_owned();
    invoke(&w, "auth_login", json!({"userId": owner, "pin": "1234"})).unwrap();
    let wrong = invoke(&w, "auth_login", json!({"userId": owner, "pin": "9999"}));
    assert_eq!(code("auth_login", &wrong).as_deref(), Some("AUTH_PIN_INVALID"));
    let state = invoke(&w, "app_bootstrap_state", json!({})).unwrap();
    assert_eq!(state["session"]["userId"], owner.as_str());
    assert_eq!(state["setupComplete"], true);
    assert_eq!(state["workspace"]["registerName"], "Till 1");
    assert_eq!(state["workspace"]["mode"], "sandbox");
    // A successful switch ends the previous session in the sessions table.
    let cashier = invoke(
        &w,
        "staff_create",
        json!({"input": {"displayName": "Cal", "role": "cashier", "pin": "3333"}}),
    )
    .unwrap();
    let s = invoke(
        &w,
        "auth_login",
        json!({"userId": cashier["userId"], "pin": "3333"}),
    )
    .unwrap();
    assert_eq!(s["role"], "cashier");
    let services = app.state::<AppServices>();
    let db = services.db.as_ref().unwrap().clone();
    let (open, closed): (i64, i64) = tauri::async_runtime::block_on(async move {
        sqlx::query_as("SELECT sum(ended_at IS NULL), sum(ended_at IS NOT NULL) FROM staff_sessions")
            .fetch_one(db.readers())
            .await
            .unwrap()
    });
    assert_eq!((open, closed), (1, 1));
}

#[test]
fn an_approval_round_trip_over_ipc() {
    // E05-T04 through the shell: APPROVAL_REQUIRED carries the scope, a manager's PIN turns
    // it into a token, a failed attempt keeps the token, a successful one uses it up.
    let (_dir, _app, w) = start();
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    let owner = created["ownerUserId"].as_str().unwrap().to_owned();
    invoke(&w, "auth_login", json!({"userId": owner, "pin": "1234"})).unwrap();
    let manager = invoke(
        &w,
        "staff_create",
        json!({"input": {"displayName": "Mia", "role": "manager", "pin": "2222"}}),
    )
    .unwrap();
    let cashier = invoke(
        &w,
        "staff_create",
        json!({"input": {"displayName": "Cal", "role": "cashier", "pin": "3333"}}),
    )
    .unwrap();
    let tax = invoke(&w, "tax_list", json!({})).unwrap()[0]["taxCategoryId"].clone();
    let mut product = cases().into_iter().find(|c| c.0 == "product_create").unwrap().2;
    product["product"]["taxCategoryId"] = tax;
    product["product"]["variants"][0]["sku"] = json!("COLA");
    invoke(&w, "product_create", product).unwrap();
    invoke(
        &w,
        "auth_login",
        json!({"userId": cashier["userId"], "pin": "3333"}),
    )
    .unwrap();
    let cart = invoke(
        &w,
        "scan_submit",
        json!({"scanEventId": "0190a5b0-0000-7000-8000-000000000009", "raw": "COLA", "source": "manual"}),
    )
    .unwrap()["cart"]
        .clone();
    let revision = cart["revision"].clone();

    let void = |token: Value, revision: Value| {
        invoke(
            &w,
            "cart_void",
            json!({"revision": revision, "reason": "walked out", "approvalTokenId": token}),
        )
    };
    let refused = void(Value::Null, revision.clone()).unwrap_err();
    assert_eq!(refused["code"], "APPROVAL_REQUIRED");
    let scope = refused["details"]["scope"].clone();
    assert_eq!(scope["permission"], "cart_void");
    assert_eq!(scope["entityId"], cart["cartId"]);

    let token = invoke(
        &w,
        "auth_request_approval",
        json!({"approverUserId": manager["userId"], "pin": "2222", "scope": scope}),
    )
    .unwrap();
    let id = token["approvalTokenId"].clone();
    // A stale revision fails for another reason: the token is kept for the retry.
    let stale = void(id.clone(), json!(0)).unwrap_err();
    assert_eq!(stale["code"], "CART_REVISION_MISMATCH");
    let done = void(id.clone(), revision.clone()).unwrap();
    assert_eq!(done["lines"].as_array().unwrap().len(), 0);
    // Used up.
    let again = void(id, json!(0)).unwrap_err();
    assert_eq!(again["code"], "APPROVAL_INVALID");
}

#[test]
fn a_cash_sale_over_ipc() {
    // E09-T08 through the shell: short tender reports what is still due; enough cash
    // completes with a receipt number, change, and a fresh cart.
    let out = tempfile::tempdir().unwrap();
    let (_dir, _app, w) = start_with(Some(Arc::new(ImportDialog {
        open: std::path::PathBuf::new(),
        save_dir: out.path().to_path_buf(),
    })));
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    invoke(
        &w,
        "auth_login",
        json!({"userId": created["ownerUserId"], "pin": "1234"}),
    )
    .unwrap();
    let tax = invoke(&w, "tax_list", json!({})).unwrap()[0]["taxCategoryId"].clone();
    let mut product = cases().into_iter().find(|c| c.0 == "product_create").unwrap().2;
    product["product"]["taxCategoryId"] = tax;
    product["product"]["variants"][0]["sku"] = json!("COLA");
    invoke(&w, "product_create", product).unwrap();
    let cart = invoke(
        &w,
        "scan_submit",
        json!({"scanEventId": "0190a5b0-0000-7000-8000-00000000000a", "raw": "COLA", "source": "manual"}),
    )
    .unwrap()["cart"]
        .clone();
    let pay = |minor: &str| {
        invoke(
            &w,
            "checkout_complete",
            json!({
                "cartId": cart["cartId"], "revision": cart["revision"],
                "tenders": [{"kind": "cash", "amountMinor": minor, "reference": null}]
            }),
        )
    };
    let short = pay("40").unwrap_err();
    assert_eq!(short["code"], "TENDER_INSUFFICIENT");
    assert_eq!(short["details"]["remainingMinor"], "60");
    let done = pay("500").unwrap();
    assert_eq!(done["receiptNumber"], "R01-000001");
    assert_eq!(
        (done["totalMinor"].as_str(), done["changeMinor"].as_str()),
        (Some("100"), Some("400"))
    );
    assert_eq!(done["syncPending"], true);
    let next = invoke(&w, "cart_current", json!({})).unwrap();
    assert_ne!(next["cartId"], cart["cartId"]);
    assert_eq!(next["lines"].as_array().unwrap().len(), 0);
    // The receipt, as stored, for the last sale.
    let receipt = invoke(&w, "receipt_get", json!({"saleId": null, "widthMm": 58})).unwrap();
    assert_eq!(receipt["receiptNumber"], "R01-000001");
    assert_eq!(receipt["columns"], 32);
    let text: Vec<&str> = receipt["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["text"].as_str().unwrap())
        .collect();
    assert!(
        text.iter()
            .any(|l| l.starts_with("Change") && l.ends_with("4.00")),
        "{text:?}"
    );
    assert!(text.iter().all(|l| l.chars().count() <= 32));
    // Printed on the virtual printer; a second copy is a reprint.
    let printed = invoke(&w, "receipt_print", json!({"saleId": null})).unwrap();
    assert_eq!(
        (printed["status"].as_str(), printed["reprint"].as_bool()),
        (Some("completed"), Some(false))
    );
    let again = invoke(&w, "receipt_print", json!({"saleId": null})).unwrap();
    assert_eq!(again["reprint"], true);
    let services = _app.state::<AppServices>();
    assert!(services.data_dir.join("virtual-printer/R01-000001.bin").exists());
    // Paying the old cart again is refused: it is closed.
    assert_eq!(pay("500").unwrap_err()["code"], "CART_REVISION_MISMATCH");

    // E12: the cola comes back. Found by the receipt number, previewed, then made.
    let sale = invoke(&w, "return_find", json!({"receiptNumber": "R01-000001"})).unwrap();
    assert_eq!(sale["lines"][0]["availableMilli"], "1000");
    let lines = json!([{"saleLineId": sale["lines"][0]["saleLineId"], "quantityMilli": "1000", "disposition": "restock"}]);
    let plan = invoke(
        &w,
        "return_preview",
        json!({"saleId": sale["saleId"], "lines": lines}),
    )
    .unwrap();
    assert_eq!(plan["totalMinor"], "100");
    let make = |refund: &str| {
        invoke(
            &w,
            "return_create",
            json!({"saleId": sale["saleId"], "lines": lines, "reason": "defective", "note": null,
                   "refunds": [{"kind": "cash", "amountMinor": refund, "reference": null}], "approvalTokenId": null}),
        )
    };
    let wrong = make("90").unwrap_err();
    assert_eq!(wrong["code"], "RETURN_REFUND_MISMATCH");
    let done = make("100").unwrap();
    assert_eq!(done["returnNumber"], "R01-R-000001");
    assert_eq!(done["originalReceiptNumber"], "R01-000001");
    // Nothing more to return.
    assert_eq!(make("100").unwrap_err()["code"], "RETURN_QTY_EXCEEDS");
    let refund = invoke(
        &w,
        "return_receipt_get",
        json!({"returnId": done["returnId"], "widthMm": 58}),
    )
    .unwrap();
    assert_eq!(refund["receiptNumber"], "R01-R-000001");
    assert!(
        refund["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["text"].as_str().unwrap().starts_with("REFUND TOTAL"))
    );
    let printed = invoke(&w, "return_receipt_print", json!({"returnId": done["returnId"]})).unwrap();
    assert_eq!(printed["status"], "completed");
    assert!(
        services
            .data_dir
            .join("virtual-printer/R01-R-000001.bin")
            .exists()
    );

    // E12-T03: the sale in the history, with its return.
    let page = invoke(
        &w,
        "sales_list",
        json!({"filter": {"from": null, "to": null, "receipt": "1", "cashierUserId": null, "tender": "cash"}, "after": null, "limit": 10}),
    )
    .unwrap();
    assert_eq!(page["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page["rows"][0]["refundedMinor"], "100");
    assert_eq!(page["next"], Value::Null);
    let detail = invoke(&w, "sales_get", json!({"saleId": page["rows"][0]["saleId"]})).unwrap();
    assert_eq!(detail["lines"][0]["returnedMilli"], "1000");
    assert_eq!(detail["tenders"][0]["confirmationSource"], "operator");
    assert_eq!(detail["returns"][0]["returnNumber"], "R01-R-000001");
    let bad = invoke(
        &w,
        "sales_list",
        json!({"filter": {"from": "yesterday", "to": null, "receipt": null, "cashierUserId": null, "tender": null}, "after": null, "limit": null}),
    )
    .unwrap_err();
    assert_eq!(bad["code"], "VALIDATION_FAILED");

    // E14-T04: export the day's lines where the save dialog says (the suggested name here).
    let day = page["rows"][0]["businessDate"].clone();
    let first = invoke(
        &w,
        "export_csv",
        json!({"dataset": "transactions", "range": {"from": day, "to": day}}),
    )
    .unwrap();
    assert_eq!(first["rows"], 1);
    let name = first["fileName"].as_str().unwrap();
    assert!(
        name.starts_with("transactions_") && name.ends_with(".csv"),
        "{name}"
    );
    assert_eq!(
        first["path"].as_str().unwrap(),
        out.path().join(name).display().to_string()
    );
    let bytes = std::fs::read(out.path().join(name)).unwrap();
    assert!(bytes.starts_with("\u{feff}receipt_number,".as_bytes()));
    let products = invoke(&w, "export_csv", json!({"dataset": "products", "range": null})).unwrap();
    assert_eq!(products["rows"], 1);
}

/// A save dialog that answers from a script and remembers the names it suggested.
#[derive(Default)]
struct ScriptedDialog {
    answers: std::sync::Mutex<Vec<Option<std::path::PathBuf>>>,
    suggested: std::sync::Mutex<Vec<String>>,
}

impl desktop_tauri_lib::FileDialog for ScriptedDialog {
    fn ask(
        &self,
        suggested: &str,
        filter: (&str, &[&str]),
    ) -> tokio::sync::oneshot::Receiver<Option<std::path::PathBuf>> {
        assert_eq!(filter, ("PDF", ["pdf"].as_slice()));
        self.suggested.lock().unwrap().push(suggested.to_owned());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(self.answers.lock().unwrap().remove(0));
        rx
    }
}

#[test]
fn a_receipt_pdf_over_ipc() {
    // E13-T04 / AT-08: no printer involved. Rust asks where to save (the dialog is scripted
    // here), writes the PDF and audits it; cancelling writes and records nothing.
    let out = tempfile::tempdir().unwrap();
    let dialog = Arc::new(ScriptedDialog::default());
    *dialog.answers.lock().unwrap() = vec![
        None,
        Some(out.path().join("my receipt")),
        Some(out.path().join("refund.pdf")),
    ];
    let (_dir, _app, w) = start_with(Some(dialog.clone()));
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    invoke(
        &w,
        "auth_login",
        json!({"userId": created["ownerUserId"], "pin": "1234"}),
    )
    .unwrap();
    let tax = invoke(&w, "tax_list", json!({})).unwrap()[0]["taxCategoryId"].clone();
    let mut product = cases().into_iter().find(|c| c.0 == "product_create").unwrap().2;
    product["product"]["taxCategoryId"] = tax;
    product["product"]["name"] = json!("可口可乐 كولا");
    product["product"]["variants"][0]["sku"] = json!("COLA");
    invoke(&w, "product_create", product).unwrap();
    let cart = invoke(
        &w,
        "scan_submit",
        json!({"scanEventId": "0190a5b0-0000-7000-8000-00000000000a", "raw": "COLA", "source": "manual"}),
    )
    .unwrap()["cart"]
        .clone();
    invoke(
        &w,
        "checkout_complete",
        json!({"cartId": cart["cartId"], "revision": cart["revision"],
               "tenders": [{"kind": "cash", "amountMinor": "100", "reference": null}]}),
    )
    .unwrap();
    let sale = invoke(&w, "return_find", json!({"receiptNumber": "R01-000001"})).unwrap();
    let pdf = |args: Value| invoke(&w, "receipt_export_pdf", args);
    let exported = || {
        invoke(
            &w,
            "audit_list",
            json!({"filter": {"action": "receipt.exported_pdf", "userId": null, "from": null, "to": null}, "before": null, "limit": 10}),
        )
        .unwrap()["rows"]
            .as_array()
            .unwrap()
            .clone()
    };

    // A sale or a return, not both and not neither.
    for (s, r) in [
        (Value::Null, Value::Null),
        (sale["saleId"].clone(), json!(UNKNOWN_ID)),
    ] {
        let e = pdf(json!({"saleId": s, "returnId": r, "paper": "a4"})).unwrap_err();
        assert_eq!(e["code"], "VALIDATION_FAILED");
    }
    // Cancelled: nothing saved, nothing recorded.
    assert_eq!(
        pdf(json!({"saleId": sale["saleId"], "returnId": null, "paper": "receipt"})).unwrap(),
        Value::Null
    );
    assert!(exported().is_empty());
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    // Saved: ".pdf" is added to a bare name; the file is a PDF; the save is audited.
    let saved = pdf(json!({"saleId": sale["saleId"], "returnId": null, "paper": "a4"})).unwrap();
    assert_eq!(saved["fileName"], "my receipt.pdf");
    let bytes = std::fs::read(out.path().join("my receipt.pdf")).unwrap();
    assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
    let rows = exported();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["entityType"], "receipt");
    let meta: Value = serde_json::from_str(rows[0]["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(meta["file"], "my receipt.pdf");
    assert_eq!(meta["format"], "a4");
    assert_eq!(meta["receiptNumber"], "R01-000001");
    // Only the temporary file's rename leaves the folder: no ".tmp" behind.
    let names: Vec<String> = std::fs::read_dir(out.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["my receipt.pdf".to_owned()]);

    // A return's refund receipt, on the till's paper width (80 mm by default).
    let lines = json!([{"saleLineId": sale["lines"][0]["saleLineId"], "quantityMilli": "1000", "disposition": "restock"}]);
    let done = invoke(
        &w,
        "return_create",
        json!({"saleId": sale["saleId"], "lines": lines, "reason": "defective", "note": null,
               "refunds": [{"kind": "cash", "amountMinor": "100", "reference": null}], "approvalTokenId": null}),
    )
    .unwrap();
    let saved = pdf(json!({"saleId": null, "returnId": done["returnId"], "paper": "receipt"})).unwrap();
    assert_eq!(saved["fileName"], "refund.pdf");
    assert!(
        std::fs::read(out.path().join("refund.pdf"))
            .unwrap()
            .starts_with(b"%PDF-")
    );
    assert_eq!(exported().len(), 2);
    assert_eq!(
        *dialog.suggested.lock().unwrap(),
        vec![
            "receipt-R01-000001.pdf",
            "receipt-R01-000001-A4.pdf",
            "refund-R01-R-000001.pdf"
        ]
    );
}

#[test]
fn a_cash_session_over_ipc() {
    // E15-T02 through the shell: open with a float, sell for cash, close with a count,
    // then read and print the Z summary.
    let (_dir, app, w) = start();
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    invoke(
        &w,
        "auth_login",
        json!({"userId": created["ownerUserId"], "pin": "1234"}),
    )
    .unwrap();
    let state = invoke(&w, "cash_session_current", json!({})).unwrap();
    // A new till starts in sandbox: practice sales need no drawer.
    assert_eq!(
        (state["session"].clone(), state["required"].clone()),
        (Value::Null, json!(false))
    );
    let bad = invoke(&w, "cash_session_open", json!({"floatMinor": "-1"})).unwrap_err();
    assert_eq!(bad["code"], "VALIDATION_FAILED");
    let opened = invoke(&w, "cash_session_open", json!({"floatMinor": "5000"})).unwrap();
    assert_eq!(opened["session"]["number"], "R01-Z-0001");
    assert_eq!(opened["expectedMinor"], "5000");
    let again = invoke(&w, "cash_session_open", json!({"floatMinor": "5000"})).unwrap_err();
    assert_eq!(again["code"], "CASH_SESSION_ALREADY_OPEN");

    let tax = invoke(&w, "tax_list", json!({})).unwrap()[0]["taxCategoryId"].clone();
    let mut product = cases().into_iter().find(|c| c.0 == "product_create").unwrap().2;
    product["product"]["taxCategoryId"] = tax;
    product["product"]["variants"][0]["sku"] = json!("COLA");
    invoke(&w, "product_create", product).unwrap();
    let cart = invoke(
        &w,
        "scan_submit",
        json!({"scanEventId": "0190a5b0-0000-7000-8000-00000000000b", "raw": "COLA", "source": "manual"}),
    )
    .unwrap()["cart"]
        .clone();
    invoke(
        &w,
        "checkout_complete",
        json!({"cartId": cart["cartId"], "revision": cart["revision"],
               "tenders": [{"kind": "cash", "amountMinor": "500", "reference": null}]}),
    )
    .unwrap();
    let now = invoke(&w, "cash_session_current", json!({})).unwrap();
    assert_eq!(now["expectedMinor"], "5100"); // 50.00 + the 1.00 sale
    assert_eq!(now["totals"]["tenders"][0]["kind"], "cash");

    let closed = invoke(
        &w,
        "cash_session_close",
        json!({"countedMinor": "5090", "note": "short a coin"}),
    )
    .unwrap();
    assert_eq!(
        (closed["expectedMinor"].as_str(), closed["varianceMinor"].as_str()),
        (Some("5100"), Some("-10"))
    );
    assert_eq!(closed["session"]["open"], false);
    let z = invoke(
        &w,
        "cash_session_z_get",
        json!({"sessionId": null, "widthMm": 58}),
    )
    .unwrap();
    assert_eq!(z["receiptNumber"], "R01-Z-0001");
    let printed = invoke(&w, "cash_session_z_print", json!({"sessionId": null})).unwrap();
    assert_eq!(printed["status"], "completed");
    let services = app.state::<AppServices>();
    assert!(services.data_dir.join("virtual-printer/R01-Z-0001.bin").exists());
    let gone = invoke(
        &w,
        "cash_session_close",
        json!({"countedMinor": "0", "note": null}),
    )
    .unwrap_err();
    assert_eq!(gone["code"], "CASH_SESSION_REQUIRED");

    // Going live: the owner's PIN; then the till is live and sales need an open drawer.
    let wrong = invoke(&w, "register_go_live", json!({"pinCode": "9999"})).unwrap_err();
    assert_eq!(wrong["code"], "AUTH_PIN_INVALID");
    invoke(&w, "register_go_live", json!({"pinCode": "1234"})).unwrap();
    let boot = invoke(&w, "app_bootstrap_state", json!({})).unwrap();
    assert_eq!(boot["workspace"]["mode"], "live");
    assert_eq!(
        invoke(&w, "cash_session_current", json!({})).unwrap()["required"],
        true
    );
    let again = invoke(&w, "register_go_live", json!({"pinCode": "1234"})).unwrap_err();
    assert_eq!(again["code"], "VALIDATION_FAILED");

    // E04-T03: settings over IPC, then the idle lock ends the session.
    let all = invoke(&w, "settings_get", json!({})).unwrap();
    let width = all
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "receipt.paper_width_mm")
        .unwrap()
        .clone();
    assert_eq!(
        (width["valueJson"].as_str(), width["kind"]["type"].as_str()),
        (Some("80"), Some("choice"))
    );
    let changed = invoke(
        &w,
        "settings_update",
        json!({"key": "receipt.paper_width_mm", "valueJson": "58", "baseVersion": width["version"]}),
    )
    .unwrap();
    assert_eq!(changed["valueJson"], "58");
    let bad = invoke(
        &w,
        "settings_update",
        json!({"key": "receipt.paper_width_mm", "valueJson": "{oops", "baseVersion": changed["version"]}),
    )
    .unwrap_err();
    assert_eq!(bad["code"], "VALIDATION_FAILED");
    let prefs = invoke(&w, "app_preferences", json!({})).unwrap();
    assert_eq!(
        (prefs["scannerSuffix"].as_str(), prefs["lockTimeoutSecs"].as_u64()),
        (Some("Enter"), Some(300))
    );
    invoke(&w, "auth_lock", json!({})).unwrap();
    assert_eq!(
        invoke(&w, "app_preferences", json!({})).unwrap_err()["code"],
        "SESSION_LOCKED"
    );
}

/// Opens a given file, and saves the error report into a folder.
struct ImportDialog {
    open: std::path::PathBuf,
    save_dir: std::path::PathBuf,
}

impl desktop_tauri_lib::FileDialog for ImportDialog {
    fn ask(
        &self,
        suggested: &str,
        filter: (&str, &[&str]),
    ) -> tokio::sync::oneshot::Receiver<Option<std::path::PathBuf>> {
        assert_eq!(filter.0, "CSV");
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(Some(self.save_dir.join(suggested)));
        rx
    }
    fn open(&self, filter: (&str, &[&str])) -> tokio::sync::oneshot::Receiver<Option<std::path::PathBuf>> {
        assert_eq!(filter, ("CSV", ["csv", "txt"].as_slice()));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(Some(self.open.clone()));
        rx
    }
}

#[test]
fn a_products_import_over_ipc() {
    // E06-T07: Rust opens the file the dialog names; the WebView sends only the mapping.
    let out = tempfile::tempdir().unwrap();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/catalogue/demo20.csv");
    let dialog = Arc::new(ImportDialog {
        open: fixture,
        save_dir: out.path().to_path_buf(),
    });
    let (_dir, _app, w) = start_with(Some(dialog));
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    invoke(
        &w,
        "auth_login",
        json!({"userId": created["ownerUserId"], "pin": "1234"}),
    )
    .unwrap();
    let preview = invoke(&w, "import_products_open", json!({})).unwrap();
    assert_eq!(preview["fileName"], "demo20.csv");
    assert_eq!(preview["totalRows"], 22);
    assert_eq!(preview["delimiter"], ",");
    assert_eq!(preview["header"][0], "name");
    let job = preview["jobId"].clone();
    // Without the price column: refused.
    let mut mapping: Vec<Value> = preview["mapping"].as_array().unwrap().clone();
    let no_price: Vec<Value> = mapping
        .iter()
        .filter(|m| m["column"] != "price")
        .cloned()
        .collect();
    let e = invoke(
        &w,
        "import_products_validate",
        json!({"jobId": job, "mapping": no_price, "onDuplicate": "error"}),
    )
    .unwrap_err();
    assert_eq!(e["code"], "VALIDATION_FAILED");
    let summary = invoke(
        &w,
        "import_products_validate",
        json!({"jobId": job, "mapping": mapping, "onDuplicate": "error"}),
    )
    .unwrap();
    assert_eq!(
        (
            summary["validRows"].as_u64(),
            summary["productsToCreate"].as_u64()
        ),
        (Some(22), Some(20))
    );
    let done = invoke(&w, "import_products_commit", json!({"jobId": job})).unwrap();
    assert_eq!(done["productsCreated"], 20);
    let list = invoke(
        &w,
        "product_list",
        json!({"filter": {"query": "", "status": "active", "trackStock": null, "categoryId": null}, "offset": 0, "limit": 50}),
    )
    .unwrap();
    assert_eq!(list["total"], 20);
    // The same file again: every product exists, so every row is a duplicate; the report lists them.
    let again = invoke(&w, "import_products_open", json!({})).unwrap();
    mapping = again["mapping"].as_array().unwrap().clone();
    let s = invoke(
        &w,
        "import_products_validate",
        json!({"jobId": again["jobId"], "mapping": mapping, "onDuplicate": "error"}),
    )
    .unwrap();
    assert_eq!(s["duplicateRows"], 22);
    assert_eq!(s["problems"][0]["status"], "duplicate_existing");
    let saved = invoke(
        &w,
        "import_products_error_report",
        json!({"jobId": again["jobId"]}),
    )
    .unwrap();
    assert_eq!(saved["fileName"], "demo20-errors.csv");
    let report = std::fs::read_to_string(out.path().join("demo20-errors.csv")).unwrap();
    assert_eq!(report.lines().count(), 23);
    invoke(&w, "import_products_cancel", json!({"jobId": again["jobId"]})).unwrap();
}

#[test]
fn at01_new_business_twenty_products_and_a_cash_sale() {
    // AT-01 (PRD §16.4.1): a new business creates its profile, adds at least 20 products and
    // completes a cash sale, all on this machine. Stage 2 has no network client at all (no
    // sync transport exists yet and the CSP allows only local content), so nothing here can
    // reach a cloud.
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/catalogue/demo20.csv");
    let out = tempfile::tempdir().unwrap();
    let (_dir, _app, w) = start_with(Some(Arc::new(ImportDialog {
        open: fixture,
        save_dir: out.path().to_path_buf(),
    })));
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Corner Shop", "1234")}),
    )
    .unwrap();
    invoke(
        &w,
        "auth_login",
        json!({"userId": created["ownerUserId"], "pin": "1234"}),
    )
    .unwrap();
    // The profile.
    let profile = invoke(&w, "business_profile_get", json!({})).unwrap();
    let mut input = json!({"name": "Corner Shop", "legalName": null, "taxRegistrationNo": "GB123456789",
        "addressLines": ["1 High Street"], "phone": null, "email": null, "registerName": "Front till"});
    input["name"] = profile["name"].clone();
    invoke(
        &w,
        "business_profile_update",
        json!({"input": input, "businessVersion": profile["businessVersion"], "registerVersion": profile["registerVersion"]}),
    )
    .unwrap();
    // 20 products from the demo file.
    let preview = invoke(&w, "import_products_open", json!({})).unwrap();
    invoke(
        &w,
        "import_products_validate",
        json!({"jobId": preview["jobId"], "mapping": preview["mapping"], "onDuplicate": "error"}),
    )
    .unwrap();
    assert_eq!(
        invoke(&w, "import_products_commit", json!({"jobId": preview["jobId"]})).unwrap()["productsCreated"],
        20
    );
    // AT-02 on the way: found by barcode, by SKU and by name.
    let by_code = invoke(&w, "catalog_lookup_code", json!({"code": "4006381333931"})).unwrap();
    assert_eq!(by_code["name"], "Cola 330ml");
    let by_sku = invoke(&w, "catalog_lookup_code", json!({"code": "BREAD-W"})).unwrap();
    assert_eq!(by_sku["name"], "White Bread");
    let by_name = invoke(&w, "catalog_search", json!({"query": "banan", "limit": 5})).unwrap();
    assert_eq!(by_name[0]["name"], "Bananas");
    // The test cash sale: two scans (one barcode, one SKU), paid in cash with change.
    let mut cart = Value::Null;
    for (i, code) in ["4006381333931", "BREAD-W"].iter().enumerate() {
        cart = invoke(
            &w,
            "scan_submit",
            json!({"scanEventId": format!("0190a5b0-0000-7000-8000-0000000001{i:02}"), "raw": code, "source": "manual"}),
        )
        .unwrap()["cart"]
            .clone();
    }
    assert_eq!(cart["lines"].as_array().unwrap().len(), 2);
    let done = invoke(
        &w,
        "checkout_complete",
        json!({"cartId": cart["cartId"], "revision": cart["revision"],
               "tenders": [{"kind": "cash", "amountMinor": "500", "reference": null}]}),
    )
    .unwrap();
    assert_eq!(done["receiptNumber"], "R01-000001");
    assert_eq!(
        (done["totalMinor"].as_str(), done["changeMinor"].as_str()),
        (Some("260"), Some("240"))
    );
    // Saved here, waiting for a cloud that is not linked.
    assert_eq!(done["syncPending"], true);
    let receipt = invoke(&w, "receipt_get", json!({"saleId": null, "widthMm": 80})).unwrap();
    let text: Vec<&str> = receipt["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["text"].as_str().unwrap())
        .collect();
    assert!(text.iter().any(|l| l.trim() == "Corner Shop"), "{text:?}");
    assert!(text.iter().any(|l| l.contains("Tax no. GB123456789")), "{text:?}");
}

/// E19-T03 over IPC: a backup, a change after it, then the restore (which needs the owner to
/// confirm the loss on an unlinked till). The restored database has no trace of the change,
/// records who restored what, and keeps the replaced database as a `pre_restore` backup.
#[test]
fn a_restore_puts_the_till_back_to_the_backup_and_records_it() {
    let (dir, _app, w) = start();
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    let owner = created["ownerUserId"].as_str().unwrap().to_owned();
    invoke(&w, "auth_login", json!({"userId": owner, "pin": "1234"})).unwrap();
    let made = invoke(&w, "backup_now", json!({})).unwrap();
    let id = made["id"].as_str().unwrap().to_owned();
    assert_eq!(made["kind"], "manual");
    let check = invoke(&w, "backup_verify", json!({"backupId": id})).unwrap();
    assert_eq!(check["restorable"], true, "{check}");

    // A product made after the backup.
    let tax = invoke(&w, "tax_list", json!({})).unwrap()[0]["taxCategoryId"].clone();
    let mut product = cases().into_iter().find(|c| c.0 == "product_create").unwrap().2;
    product["product"]["taxCategoryId"] = tax;
    invoke(&w, "product_create", product).unwrap();

    // Not confirmed: refused, nothing changes.
    let refused = invoke(
        &w,
        "backup_restore",
        json!({"backupId": id, "confirmLoss": false}),
    );
    assert_eq!(
        code("backup_restore", &refused).as_deref(),
        Some("VALIDATION_FAILED")
    );
    let status = invoke(&w, "backup_status", json!({})).unwrap();
    assert_eq!(status["linked"], false);
    assert!(
        status["backups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == id.as_str())
    );

    invoke(&w, "backup_restore", json!({"backupId": id, "confirmLoss": true})).unwrap();

    // What the app finds when it starts again.
    tauri::async_runtime::block_on(async {
        let db = pos_local_db::LocalDb::open(&pos_local_db::DbOptions::in_dir(dir.path()))
            .await
            .unwrap();
        let q = |sql: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, i64>(sql)
                    .fetch_one(db.readers())
                    .await
                    .unwrap()
            }
        };
        assert_eq!(
            q("SELECT count(*) FROM products").await,
            0,
            "the product after the backup is gone"
        );
        assert_eq!(
            q("SELECT count(*) FROM audit_events WHERE action = 'backup.restored'").await,
            1
        );
        assert!(
            db.backups()
                .iter()
                .any(|b| b.meta.kind == pos_local_db::backup::BackupKind::PreRestore),
            "the replaced database is kept"
        );
        db.close().await;
    });
}

/// A save dialog for the full export: answers with a path in `dir`, or cancels.
struct ZipDialog {
    to: Option<std::path::PathBuf>,
}

impl desktop_tauri_lib::FileDialog for ZipDialog {
    fn ask(
        &self,
        suggested: &str,
        filter: (&str, &[&str]),
    ) -> tokio::sync::oneshot::Receiver<Option<std::path::PathBuf>> {
        assert_eq!(filter, ("ZIP", ["zip"].as_slice()));
        assert!(
            suggested.starts_with("sale-os-export-") && suggested.ends_with(".zip"),
            "{suggested}"
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(self.to.clone());
        rx
    }
}

/// E19-T04 over IPC: the owner saves the export where the dialog says; it is audited with the
/// file's name. Cancelling writes and records nothing.
#[test]
fn the_owner_exports_all_data_and_it_is_audited() {
    let out = tempfile::tempdir().unwrap();
    for to in [None, Some(out.path().join("everything"))] {
        let (_dir, _app, w) = start_with(Some(Arc::new(ZipDialog { to: to.clone() })));
        let created = invoke(
            &w,
            "setup_create_workspace",
            json!({"input": workspace_input("Shop", "1234")}),
        )
        .unwrap();
        let owner = created["ownerUserId"].as_str().unwrap().to_owned();
        invoke(&w, "auth_login", json!({"userId": owner, "pin": "1234"})).unwrap();
        let done = invoke(&w, "data_export_full", json!({})).unwrap();
        let audited = invoke(
            &w,
            "audit_list",
            json!({"filter": {"action": "data.exported", "userId": null, "from": null, "to": null}, "before": null, "limit": 10}),
        )
        .unwrap();
        let rows = audited["rows"].as_array().unwrap().len();
        if to.is_none() {
            assert_eq!(done, Value::Null);
            assert_eq!(rows, 0);
            continue;
        }
        assert_eq!(done["fileName"], "everything.zip");
        assert!(done["tables"].as_u64().unwrap() > 20, "{done}");
        let bytes = std::fs::read(out.path().join("everything.zip")).unwrap();
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert_eq!(rows, 1);
        assert_eq!(audited["rows"][0]["entityId"], "everything.zip");
    }
}

/// Every string (and empty optional) argument position in `args`, as a JSON pointer and the
/// guard's dotted path, with the key it sits under.
fn text_positions(args: &Value) -> Vec<(String, String, String)> {
    fn walk(v: &Value, pointer: &str, dotted: &str, key: &str, out: &mut Vec<(String, String, String)>) {
        match v {
            Value::String(_) | Value::Null => out.push((pointer.into(), dotted.into(), key.into())),
            Value::Array(items) => {
                for (i, x) in items.iter().enumerate() {
                    walk(x, &format!("{pointer}/{i}"), &format!("{dotted}[{i}]"), key, out);
                }
            }
            Value::Object(map) => {
                for (k, x) in map {
                    let d = if dotted.is_empty() {
                        k.clone()
                    } else {
                        format!("{dotted}.{k}")
                    };
                    walk(x, &format!("{pointer}/{k}"), &d, k, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(args, "", "", "", &mut out);
    out
}

/// E19-T05 / PAY-005: a test card number typed into any text argument of any command is
/// refused before the command runs (a swipe, for scans and searches); and afterwards it is
/// nowhere on disk: not in the database, its WAL, the backups, or the logs.
#[test]
fn a_card_number_typed_anywhere_is_refused_and_stored_nowhere() {
    const PAN: &str = "4111 1111 1111 1111";
    const SWIPE: &str = "%B4111111111111111^TEST/CARD^2512101000000000000?";
    let logs = tempfile::tempdir().unwrap();
    let log = pos_observability::init(&pos_observability::LogConfig {
        dir: Some(logs.path().to_path_buf()),
        file_prefix: "sweep".into(),
        retention_days: 1,
        stderr: false,
        default_filter: "debug".into(),
    })
    .ok();
    let (dir, _app, w) = start();
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    let owner = created["ownerUserId"].as_str().unwrap().to_owned();
    invoke(&w, "auth_login", json!({"userId": owner, "pin": "1234"})).unwrap();
    invoke(&w, "backup_now", json!({})).unwrap();

    let never_stored = [
        "pin",
        "ownerPin",
        "password",
        "newPin",
        "currentPin",
        "recoveryKey",
    ];
    let mut swept = 0;
    for (cmd, _, args) in cases() {
        for (pointer, dotted, key) in text_positions(&args) {
            // Identifiers the app made (ids, epoch milliseconds) are not typed text.
            if never_stored.contains(&key.as_str())
                || key == "id"
                || key.ends_with("Id")
                || key.ends_with("Ids")
                || key.ends_with("Ms")
            {
                continue;
            }
            let mut a = args.clone();
            let typed = if key == "raw" || key == "query" {
                SWIPE
            } else {
                PAN
            };
            *a.pointer_mut(&pointer).unwrap() = Value::String(format!("note {typed}"));
            let e = invoke(&w, cmd, a).expect_err(cmd);
            assert_eq!(e["code"], "CARD_DATA_REJECTED", "{cmd} {dotted}: {e}");
            assert_eq!(e["details"]["field"], dotted.as_str(), "{cmd}");
            assert!(
                !e.to_string().contains("4111"),
                "the refusal repeats the number: {e}"
            );
            swept += 1;
        }
    }
    assert!(swept > 80, "only {swept} text arguments were tried");
    // Typed with dashes, too.
    let e = invoke(
        &w,
        "product_create",
        json!({"product": {"name": "4111-1111-1111-1111"}}),
    )
    .unwrap_err();
    assert_eq!(e["details"]["field"], "product.name");

    drop(w);
    drop(log); // flushes the log file
    let mut files = Vec::new();
    for root in [dir.path(), logs.path()] {
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    files.push(p);
                }
            }
        }
    }
    assert!(files.iter().any(|f| f.extension().is_some_and(|e| e == "db")));
    assert!(
        files.iter().any(|f| f.starts_with(logs.path())),
        "the test wrote no logs to check"
    );
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for needle in ["4111111111111111", "4111 1111 1111 1111", "4111-1111-1111-1111"] {
            assert!(!text.contains(needle), "{} holds the card number", f.display());
        }
    }
    let logged: String = files
        .iter()
        .filter(|f| f.starts_with(logs.path()))
        .map(|f| std::fs::read_to_string(f).unwrap())
        .collect();
    assert!(
        logged.contains("card data refused at the IPC boundary"),
        "the refusals were logged (and the log was read)"
    );
    eprintln!("{swept} text arguments refused; {} files clean", files.len());
}

/// The app over a given data folder and key store (ADR-023).
fn start_keyed(
    dir: &std::path::Path,
    keys: Arc<dyn desktop_tauri_lib::db_key::DbKeyStore>,
) -> (tauri::App<MockRuntime>, WebviewWindow<MockRuntime>) {
    let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));
    let services = tauri::async_runtime::block_on(AppServices::start_with(dir, None, hasher, keys));
    let app = mock_builder()
        .invoke_handler(desktop_tauri_lib::guarded_invoke_handler(
            &desktop_tauri_lib::specta_builder::<MockRuntime>(),
        ))
        .build(tauri::generate_context!())
        .unwrap();
    app.manage(services);
    let w = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    (app, w)
}

/// ADR-023 over IPC: the till's data is encrypted with a key from the keychain; the owner can
/// read the recovery key (audited); on a computer whose keychain lacks the key, the app is
/// locked until the recovery key is typed, and a wrong one is refused.
#[test]
fn encrypted_till_recovery_key_and_unlock() {
    use desktop_tauri_lib::db_key::MemoryDbKey;
    let dir = tempfile::tempdir().unwrap();
    let keychain = Arc::new(MemoryDbKey::default());
    let recovery;
    {
        let (_app, w) = start_keyed(dir.path(), keychain.clone());
        let created = invoke(
            &w,
            "setup_create_workspace",
            json!({"input": workspace_input("Shop", "1234")}),
        )
        .unwrap();
        let owner = created["ownerUserId"].as_str().unwrap().to_owned();
        invoke(&w, "auth_login", json!({"userId": owner, "pin": "1234"})).unwrap();
        let status = invoke(&w, "backup_status", json!({})).unwrap();
        assert_eq!(status["encrypted"], true, "{status}");
        assert_eq!(status["recoveryKeyConfirmed"], false);
        recovery = invoke(&w, "security_recovery_key", json!({}))
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(recovery.split('-').count(), 8);
        let status = invoke(&w, "security_recovery_key_confirmed", json!({})).unwrap();
        assert_eq!(status["recoveryKeyConfirmed"], true);
        let audited = invoke(
            &w,
            "audit_list",
            json!({"filter": {"action": "security.recovery_key_viewed", "userId": null, "from": null, "to": null}, "before": null, "limit": 10}),
        )
        .unwrap();
        assert_eq!(audited["rows"].as_array().unwrap().len(), 1);
        assert!(
            !audited.to_string().contains(&recovery),
            "the key is not in the audit log"
        );
        // Not locked: unlocking is refused.
        let e = invoke(&w, "db_unlock", json!({"recoveryKey": recovery})).unwrap_err();
        assert_eq!(e["code"], "VALIDATION_FAILED");
        invoke(&w, "auth_logout", json!({})).unwrap();
    }
    let bytes = std::fs::read(dir.path().join("pos.db")).unwrap();
    assert!(!bytes.starts_with(b"SQLite format 3"), "encrypted on disk");

    // A new Windows account (an empty keychain): locked, and the app says so.
    let empty = Arc::new(MemoryDbKey::default());
    {
        let (_app, w) = start_keyed(dir.path(), empty.clone());
        let e = invoke(&w, "app_bootstrap_state", json!({})).unwrap_err();
        assert_eq!(e["code"], "STORE_LOCKED", "{e}");
        let wrong = "0123ABCD-0123ABCD-0123ABCD-0123ABCD-0123ABCD-0123ABCD-0123ABCD-0123ABCD";
        let e = invoke(&w, "db_unlock", json!({"recoveryKey": wrong})).unwrap_err();
        assert_eq!(e["code"], "STORE_LOCKED");
        let e = invoke(&w, "db_unlock", json!({"recoveryKey": "too short"})).unwrap_err();
        assert_eq!(e["code"], "VALIDATION_FAILED");
        assert!(empty.0.lock().unwrap().is_none(), "nothing kept from a wrong key");
        invoke(&w, "db_unlock", json!({"recoveryKey": recovery.to_lowercase()})).unwrap();
    }
    // After the restart it opens.
    let (_app, w) = start_keyed(dir.path(), empty);
    let state = invoke(&w, "app_bootstrap_state", json!({})).unwrap();
    assert_eq!(state["setupComplete"], true);
}

/// Saves to a fixed folder, whatever the suggested name.
struct SaveInto(std::path::PathBuf);

impl desktop_tauri_lib::FileDialog for SaveInto {
    fn ask(
        &self,
        suggested: &str,
        _filter: (&str, &[&str]),
    ) -> tokio::sync::oneshot::Receiver<Option<std::path::PathBuf>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(Some(self.0.join(suggested)));
        rx
    }
}

/// The entries of a ZIP whose local headers carry their sizes (as Sale OS writes them).
fn unzip(bytes: &[u8]) -> BTreeMap<String, String> {
    use std::io::Read;
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]) as usize;
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
    let mut out = BTreeMap::new();
    let mut i = 0;
    while u32_at(i) == 0x0403_4b50 {
        let compressed = u32_at(i + 18) as usize;
        let (name_len, extra_len) = (u16_at(i + 26), u16_at(i + 28));
        let name = String::from_utf8(bytes[i + 30..i + 30 + name_len].to_vec()).unwrap();
        let start = i + 30 + name_len + extra_len;
        let mut text = String::new();
        flate2::read::DeflateDecoder::new(&bytes[start..start + compressed])
            .read_to_string(&mut text)
            .unwrap();
        out.insert(name, text);
        i = start + compressed;
    }
    out
}

/// E19-T07: the diagnostics bundle holds the till's state and the redacted logs, and no PIN
/// hash, payload or card number.
#[test]
fn the_diagnostics_bundle_tells_support_enough_and_nothing_private() {
    let out = tempfile::tempdir().unwrap();
    let (dir, _app, w) = start_with(Some(Arc::new(SaveInto(out.path().to_path_buf()))));
    let created = invoke(
        &w,
        "setup_create_workspace",
        json!({"input": workspace_input("Shop", "1234")}),
    )
    .unwrap();
    let owner = created["ownerUserId"].as_str().unwrap().to_owned();
    invoke(&w, "auth_login", json!({"userId": owner, "pin": "1234"})).unwrap();
    let tax = invoke(&w, "tax_list", json!({})).unwrap()[0]["taxCategoryId"].clone();
    let mut product = cases().into_iter().find(|c| c.0 == "product_create").unwrap().2;
    product["product"]["taxCategoryId"] = tax;
    product["product"]["name"] = json!("Zebra Biscuits Deluxe");
    invoke(&w, "product_create", product).unwrap();
    // A log line that slipped a card number through (redaction at write time is the first line
    // of defence; the bundle masks again).
    std::fs::create_dir_all(dir.path().join("logs")).unwrap();
    std::fs::write(
        dir.path().join("logs/sale-os.2026-11-19.log"),
        "{\"msg\":\"reference 4111 1111 1111 1111 refused\"}\n",
    )
    .unwrap();

    let done = invoke(&w, "diagnostics_export", json!({})).unwrap();
    let peek = unzip(&std::fs::read(out.path().join(done["fileName"].as_str().unwrap())).unwrap());
    let peek: Value = serde_json::from_str(&peek["diagnostics.json"]).unwrap();
    assert_eq!(
        done["integrityOk"], true,
        "{}",
        peek["database"]["integrityCheck"]
    );
    assert_eq!(done["stockDrift"], 0);
    assert_eq!(done["files"], 2);
    let name = done["fileName"].as_str().unwrap();
    assert!(
        name.starts_with("sale-os-diagnostics-") && name.ends_with(".zip"),
        "{name}"
    );
    let files = unzip(&std::fs::read(out.path().join(name)).unwrap());
    let diag: Value = serde_json::from_str(&files["diagnostics.json"]).unwrap();
    assert_eq!(diag["database"]["integrityCheck"], json!(["ok"]));
    assert!(diag["database"]["schemaVersion"].as_i64().unwrap() >= 18);
    assert!(!diag["database"]["recentAudit"].as_array().unwrap().is_empty());
    assert!(!diag["database"]["settings"].as_array().unwrap().is_empty());
    assert!(
        diag["database"]["outbox"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["events"].as_i64() > Some(0))
    );
    assert_eq!(
        diag["app"]["encrypted"], false,
        "this harness runs without a keychain"
    );

    let all: String = files.values().cloned().collect();
    let hashes: Vec<String> = tauri::async_runtime::block_on(async {
        let db = pos_local_db::LocalDb::open(&pos_local_db::DbOptions::in_dir(dir.path()))
            .await
            .unwrap();
        let h = sqlx::query_scalar("SELECT pin_hash FROM staff_users")
            .fetch_all(db.readers())
            .await
            .unwrap();
        db.close().await;
        h
    });
    for h in &hashes {
        assert!(!all.contains(h.as_str()), "a PIN hash is in the bundle");
    }
    assert!(!all.contains("$argon2"));
    assert!(
        !all.contains("Zebra Biscuits Deluxe"),
        "no event payloads or business records"
    );
    assert!(!all.contains("4111 1111 1111 1111"), "the card number is masked");
    assert!(
        files["logs/sale-os.2026-11-19.log"].contains("1111]"),
        "masked, last four kept"
    );
}
