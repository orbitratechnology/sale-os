//! Human-readable document numbers (POS-009, docs/backlog/04 `sales.receipt_number`).

/// `{prefix}{register_code}-{seq:06}`, e.g. `R01-000042` or `SHOP-R01-000042` with prefix
/// `SHOP-`. Sequences wider than six digits keep growing (`R01-1000000`), never wrap.
pub fn receipt_number(prefix: &str, register_code: &str, seq: u64) -> String {
    format!("{prefix}{register_code}-{seq:06}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(receipt_number("", "R01", 42), "R01-000042");
        assert_eq!(receipt_number("SHOP-", "R01", 1), "SHOP-R01-000001");
        assert_eq!(receipt_number("", "R2", 1_000_000), "R2-1000000");
    }
}
