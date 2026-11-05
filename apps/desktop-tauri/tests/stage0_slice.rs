//! E00-T06 exit gate, through the IPC layer: set up -> scan -> complete cash sale ->
//! virtual print, against a real database, on Tauri's mock runtime with the real ACL.

use desktop_tauri_lib::{AppServices, DEMO_BARCODE};
use serde_json::{Value, json};
use tauri::Manager;
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{WebviewWindow, WebviewWindowBuilder};

fn invoke(w: &WebviewWindow<MockRuntime>, cmd: &str, body: Value) -> Result<Value, Value> {
    get_ipc_response(
        w,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
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

fn count(app: &tauri::App<MockRuntime>, table: &str) -> i64 {
    let services = app.state::<AppServices>();
    let db = services.db.as_ref().unwrap().clone();
    tauri::async_runtime::block_on(async move {
        sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(db.readers())
            .await
            .unwrap()
    })
}

#[test]
fn set_up_scan_and_sell_through_ipc() {
    let dir = tempfile::tempdir().unwrap();
    let services = tauri::async_runtime::block_on(AppServices::start(dir.path(), None));
    let commands = desktop_tauri_lib::specta_builder::<MockRuntime>();
    let app = mock_builder()
        .invoke_handler(commands.invoke_handler())
        .build(tauri::generate_context!())
        .unwrap();
    app.manage(services);
    let w = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();

    // 1. Database exists and is migrated.
    assert!(dir.path().join("pos.db").exists());

    // 2. Demo product (and workspace) through the write path.
    let product = invoke(&w, "dev_demo_setup", json!({})).expect("setup");
    assert_eq!(product["barcode"], DEMO_BARCODE);
    assert_eq!(product["priceFormatted"], "1.20");

    // 3. A scan adds the line; an unknown code is a catalogued error and changes nothing.
    let cart = invoke(
        &w,
        "dev_demo_scan",
        json!({"raw": format!(" {DEMO_BARCODE}\r"), "scanEventId": "e1"}),
    )
    .expect("scan");
    assert_eq!(cart["lines"].as_array().unwrap().len(), 1);
    assert_eq!(cart["totalFormatted"], "1.20");
    let not_found = invoke(&w, "dev_demo_scan", json!({"raw": "999", "scanEventId": "e2"})).unwrap_err();
    assert_eq!(not_found["code"], "SCAN_NOT_FOUND");
    let short = invoke(&w, "dev_demo_complete_sale", json!({"cashReceivedMinor": "100"})).unwrap_err();
    assert_eq!(short["code"], "TENDER_INSUFFICIENT");

    // 4. Cash sale: one atomic write set.
    let sale = invoke(&w, "dev_demo_complete_sale", json!({"cashReceivedMinor": "500"})).expect("sale");
    assert_eq!(sale["receiptNumber"], "R01-000001");
    assert_eq!(sale["changeFormatted"], "3.80");
    assert_eq!(count(&app, "spike_sales"), 1);
    assert_eq!(count(&app, "spike_sale_lines"), 1);
    assert_eq!(count(&app, "spike_tenders"), 1);
    assert_eq!(count(&app, "spike_stock_movements"), 1);
    assert_eq!(count(&app, "spike_cart_lines"), 0);
    // Workspace (3) + product (1) events precede the sale under ADR-009 (outbox from day zero).
    assert_eq!(sale["outboxSequence"], 5);
    assert_eq!(count(&app, "outbox_events"), 5);

    // 5. ESC/POS bytes captured by the virtual printer after the commit.
    let capture = std::path::PathBuf::from(sale["print"].as_str().unwrap());
    assert!(
        capture.starts_with(dir.path().join("virtual-printer")),
        "{capture:?}"
    );
    let bytes = std::fs::read(&capture).unwrap();
    assert!(bytes.starts_with(&[0x1B, 0x40]), "starts with ESC @");
    assert!(
        bytes.ends_with(&[0x1D, 0x56, 0x42, 0x00]),
        "ends with a partial cut"
    );
    let text = std::fs::read_to_string(capture.with_extension("txt")).unwrap();
    assert!(
        text.contains("R01-000001") && text.contains("CHANGE") && text.contains("3.80"),
        "{text}"
    );

    // A second sale gets the next receipt number.
    invoke(
        &w,
        "dev_demo_scan",
        json!({"raw": DEMO_BARCODE, "scanEventId": "e3"}),
    )
    .unwrap();
    let second = invoke(&w, "dev_demo_complete_sale", json!({"cashReceivedMinor": "120"})).unwrap();
    assert_eq!(second["receiptNumber"], "R01-000002");
    assert_eq!(second["changeFormatted"], "0.00");
}
