include!("src/command_names.rs");

fn main() {
    // Integration tests link Tauri too. On Windows they need the same Common Controls v6
    // manifest as the app, or they fail to start with STATUS_ENTRYPOINT_NOT_FOUND.
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|v| v == "msvc");
    if msvc {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
            .join("windows-test-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}", manifest.display());
    }

    // Declaring the app's commands makes Tauri generate an `allow-<command>` permission for
    // each, and denies any command a window's capability does not grant (E01-T03).
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri-build failed");
}
