//! Business settings defaults written at setup (E04-T01). The typed settings registry with
//! per-key validation and permissions arrives with E04-T03.

/// `(key, default)`. Keys follow docs/backlog/07 §1.2 and 04 `business_settings`.
pub fn defaults() -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    vec![
        ("inventory.allow_negative", json!(true)),
        ("security.lock_timeout_secs", json!(300)),
        ("discount.cashier_max_ppm", json!(100_000)),
        ("returns.cashier_requires_approval", json!(true)),
        ("payments.cashier_external_tender", json!(true)),
        ("cash.require_session_for_sale", json!(true)),
        ("receipt.paper_width_mm", json!(80)),
        ("receipt.auto_print", json!(false)),
        ("scanner.suffix", json!("Enter")),
    ]
}
