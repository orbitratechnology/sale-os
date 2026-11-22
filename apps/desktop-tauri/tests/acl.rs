//! E01-T03 acceptance: the WebView can call only the app's declared commands (plus event
//! delivery). fs, shell, http, path, dialog and window APIs are refused by Tauri's ACL, and so is
//! any command from a window that has no capability.

use serde_json::{Value, json};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{WebviewWindow, WebviewWindowBuilder};

/// The app's own origin as Tauri sees it: `http://tauri.localhost` on Windows (WebView2
/// serves custom protocols over http), `tauri://localhost` elsewhere. Using the wrong one
/// makes Tauri refuse every command as coming from a foreign URL, which would let refusal
/// tests pass for the wrong reason.
fn app_origin() -> tauri::Url {
    let origin = if cfg!(windows) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    origin.parse().expect("valid origin")
}

fn window(label: &str) -> (tauri::App<MockRuntime>, WebviewWindow<MockRuntime>) {
    let commands = desktop_tauri_lib::specta_builder::<MockRuntime>();
    let app = mock_builder()
        // Registered as in the app (Rust opens the save dialog), so the refusal below is the
        // capability's doing, not a missing plugin.
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(commands.invoke_handler())
        .build(tauri::generate_context!())
        .expect("mock app builds");
    let w = WebviewWindowBuilder::new(&app, label, Default::default())
        .build()
        .expect("window builds");
    (app, w)
}

/// The app's services on a fresh data directory, so an allowed command actually runs.
fn with_services(app: &tauri::App<MockRuntime>) -> tempfile::TempDir {
    use tauri::Manager;
    let dir = tempfile::tempdir().expect("temp dir");
    let hasher: std::sync::Arc<dyn pos_application::auth::PinHasher> =
        std::sync::Arc::new(pos_application::auth::Argon2PinHasher::with_params(256, 1, 1));
    let services = tauri::async_runtime::block_on(desktop_tauri_lib::AppServices::start_with_hasher(
        dir.path(),
        None,
        hasher,
    ));
    app.manage(services);
    dir
}

fn invoke(w: &WebviewWindow<MockRuntime>, cmd: &str, body: Value) -> Result<String, String> {
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
    .map(|r| format!("{r:?}"))
    .map_err(|e| e.to_string())
}

/// The refusal must come from Tauri's ACL ("... not allowed ..."), not from bad arguments:
/// a test that only checked `is_err()` would pass even if the permission were granted.
fn assert_refused_by_acl(res: Result<String, String>, what: &str) {
    match res {
        Err(e) if e.contains("not allowed") => {}
        other => panic!("{what} must be refused by the ACL, got {other:?}"),
    }
}

#[test]
fn declared_command_is_allowed_from_main_window() {
    let (app, w) = window("main");
    let _dir = with_services(&app);
    let body = invoke(&w, "app_info", json!({})).expect("allowed command must succeed");
    assert!(body.contains("schemaVersion"), "{body}");
}

#[test]
fn plugin_and_core_apis_are_refused() {
    let (_app, w) = window("main");
    for (cmd, body) in [
        ("plugin:fs|read_text_file", json!({"path": "/etc/passwd"})),
        (
            "plugin:shell|execute",
            json!({"program": "cmd", "args": ["/c", "calc"]}),
        ),
        (
            "plugin:http|fetch",
            json!({"clientConfig": {"url": "https://example.com"}}),
        ),
        ("plugin:path|resolve_directory", json!({"directory": 1})),
        // The dialog is opened from Rust only; the WebView cannot pick paths itself.
        ("plugin:dialog|save", json!({"options": {}})),
        ("plugin:dialog|open", json!({"options": {}})),
        ("plugin:window|close", json!({"label": "main"})),
        (
            "plugin:webview|create_webview_window",
            json!({"options": {"label": "x", "url": "https://evil.example"}}),
        ),
    ] {
        assert_refused_by_acl(invoke(&w, cmd, body), cmd);
    }
}

#[test]
fn undeclared_command_is_refused() {
    let (_app, w) = window("main");
    assert_refused_by_acl(invoke(&w, "export_database", json!({})), "export_database");
}

#[test]
fn windows_without_a_capability_cannot_call_anything() {
    let (_app, w) = window("popup");
    assert_refused_by_acl(
        invoke(&w, "app_info", json!({})),
        "app_info from an unlisted window",
    );
}
