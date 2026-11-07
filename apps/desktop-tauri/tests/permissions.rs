//! E05-T03 / AT-11: every IPC command, called directly (bypassing the UI) as each role, is
//! allowed or refused exactly as the role matrix says (docs/backlog/07 §1.2).
//!
//! The table below must list every registered command. Adding a command without deciding
//! what it requires fails `every_command_is_classified`.

use desktop_tauri_lib::{AppServices, COMMANDS};
use pos_application::auth::{Argon2PinHasher, PinHasher};
use pos_domain::{Permission, Role, role_has};
use serde_json::{Value, json};
use std::collections::BTreeSet;
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
    let dir = tempfile::tempdir().unwrap();
    // Cheap Argon2 parameters: this test signs in many times. The app uses ADR-013's cost.
    let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));
    let services = tauri::async_runtime::block_on(AppServices::start_with_hasher(dir.path(), None, hasher));
    let app = mock_builder()
        .invoke_handler(desktop_tauri_lib::specta_builder::<MockRuntime>().invoke_handler())
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
    let (_dir, _app, w) = start();
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

    // E14-T04: export the day's lines; a second export never overwrites the first.
    let day = page["rows"][0]["businessDate"].clone();
    let export = || {
        invoke(
            &w,
            "export_csv",
            json!({"dataset": "transactions", "range": {"from": day, "to": day}}),
        )
        .unwrap()
    };
    let first = export();
    let second = export();
    assert_eq!(first["rows"], 1);
    assert_ne!(first["path"], second["path"]);
    assert!(second["fileName"].as_str().unwrap().ends_with("-2.csv"));
    let bytes = std::fs::read(first["path"].as_str().unwrap()).unwrap();
    assert!(bytes.starts_with("\u{feff}receipt_number,".as_bytes()));
    let products = invoke(&w, "export_csv", json!({"dataset": "products", "range": null})).unwrap();
    assert_eq!(products["rows"], 1);
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
}
