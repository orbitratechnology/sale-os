include!("src/command_names.rs");

fn main() {
    // Declaring the app's commands makes Tauri generate an `allow-<command>` permission for
    // each, and denies any command a window's capability does not grant (E01-T03).
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri-build failed");
}
