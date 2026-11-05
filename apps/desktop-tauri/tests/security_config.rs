//! E01-T03: the security configuration stays least-privilege. These checks fail CI if a
//! broad permission, a remote origin, or `unsafe-eval` is ever added.

use serde_json::Value;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn json(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{ROOT}/{path}")).unwrap()).unwrap()
}

fn kebab(cmd: &str) -> String {
    cmd.replace('_', "-")
}

#[test]
fn capability_grants_exactly_the_declared_commands() {
    let cap = json("capabilities/main.json");
    let granted: Vec<String> = cap["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_owned())
        .collect();
    let mut expected: Vec<String> = desktop_tauri_lib::COMMANDS
        .iter()
        .map(|c| format!("allow-{}", kebab(c)))
        .collect();
    expected.push("core:event:default".into());
    let mut g = granted.clone();
    g.sort();
    expected.sort();
    assert_eq!(
        g, expected,
        "capabilities/main.json must grant exactly the declared commands + core:event:default"
    );
    for p in &granted {
        for banned in [
            "core:default",
            "core:path",
            "core:webview",
            "core:window",
            "fs:",
            "shell:",
            "http:",
            "opener:",
            "process:",
        ] {
            assert!(
                !p.starts_with(banned),
                "broad permission {p} is not allowed in the main window"
            );
        }
    }
}

#[test]
fn generated_bindings_cover_exactly_the_declared_commands() {
    let bindings = std::fs::read_to_string(format!("{ROOT}/../desktop-ui/src/ipc/bindings.ts")).unwrap();
    let mut invoked: Vec<&str> = bindings
        .match_indices("__TAURI_INVOKE(\"")
        .map(|(i, m)| {
            let rest = &bindings[i + m.len()..];
            &rest[..rest.find('"').unwrap()]
        })
        .collect();
    invoked.sort();
    let mut declared = desktop_tauri_lib::COMMANDS.to_vec();
    declared.sort();
    assert_eq!(
        invoked, declared,
        "COMMANDS (build.rs ACL) and specta_builder() must list the same commands"
    );
}

#[test]
fn csp_allows_only_local_content() {
    let conf = json("tauri.conf.json");
    let security = &conf["app"]["security"];
    assert_eq!(security["freezePrototype"], true);
    assert_eq!(conf["app"]["withGlobalTauri"], false);
    let csp = security["csp"]
        .as_object()
        .expect("CSP is configured as a directive map");
    assert_eq!(csp["script-src"], "'self'");
    assert_eq!(csp["default-src"], "'self'");
    assert_eq!(csp["object-src"], "'none'");
    for (directive, value) in csp {
        let v = value.as_str().unwrap();
        assert!(!v.contains("unsafe-eval"), "{directive} allows unsafe-eval");
        assert!(!v.contains('*'), "{directive} uses a wildcard");
        for token in v.split_whitespace() {
            let remote = token.starts_with("http") && token != "http://ipc.localhost";
            assert!(
                !remote && !token.starts_with("ws"),
                "{directive} allows remote origin {token}"
            );
        }
    }
}
