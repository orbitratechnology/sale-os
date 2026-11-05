//! Writes the generated TypeScript IPC bindings (ADR-012).
//! Usage: `cargo run -p desktop-tauri --bin export-bindings -- <output.ts>`
//! Normally invoked via `cargo xtask gen-bindings [--check]`.

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "apps/desktop-ui/src/ipc/bindings.ts".to_owned());
    if let Err(e) = desktop_tauri_lib::export_bindings(std::path::Path::new(&out)) {
        eprintln!("failed to export bindings: {e}");
        std::process::exit(1);
    }
}
