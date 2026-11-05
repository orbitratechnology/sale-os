// Single list of IPC command names. Included by build.rs (to generate one `allow-*`
// permission per command, ADR-012 / E01-T03) and by the crate (for consistency tests).
// Adding a command: add it here, to `specta_builder()`, and to capabilities/main.json.
pub const COMMANDS: &[&str] = &[
    "app_info",
    "dev_money_add",
    // Stage 0 vertical slice (E00-T06), removed with the spike.
    "dev_demo_setup",
    "dev_demo_cart",
    "dev_demo_scan",
    "dev_demo_complete_sale",
];
