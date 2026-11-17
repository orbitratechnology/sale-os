//! E17-T04: a reused event id raises an alert (an `error` log line with a stable `alert`
//! field that monitoring keys on). Its own test binary, for the reason in `auth_logs.rs`.

mod common;

use common::{audit_event, push, state_with, tenant, test_appliers};

#[test]
fn a_reused_event_id_raises_an_alert() {
    if std::env::var("TEST_DATABASE_URL").is_err() {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    }
    let mut ids = (String::new(), String::new());
    let lines = pos_observability::capture_json_lines(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let s = state_with(test_appliers()).await.unwrap();
            let t = tenant(&s, "Alert").await;
            let e = audit_event(&t, 1, "ok");
            push(&s, &t, std::slice::from_ref(&e)).await;
            let mut forged = audit_event(&t, 2, "ok");
            forged.event_id = e.event_id;
            push(&s, &t, &[forged]).await;
            ids = (e.event_id.to_string(), t.register.to_string());
        });
    });
    let lines: Vec<String> = lines.iter().map(ToString::to_string).collect();
    let alert = lines
        .iter()
        .find(|l| l.contains("sync.event_id_reused"))
        .unwrap_or_else(|| panic!("no alert in {lines:#?}"));
    assert!(alert.contains("\"ERROR\""), "{alert}");
    assert!(alert.contains(&ids.0) && alert.contains(&ids.1), "{alert}");
}
