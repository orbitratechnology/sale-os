//! `Secret<T>` keeps sensitive values (PINs, passwords, tokens) out of logs and error
//! messages: its `Debug` and `Display` always print `[REDACTED]`, and it deliberately does
//! not implement `Serialize`, so it cannot leak into a JSON payload by accident (PRD §13).

use serde::{Deserialize, Deserializer};

#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }
    /// The only way to read the value. Grep for `expose` when auditing secret handling.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T> std::fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    #[allow(dead_code)] // fields are read through Debug only
    struct Login {
        user: String,
        pin: Secret<String>,
        token: Secret<String>,
    }

    #[test]
    fn debug_and_display_never_show_the_value() {
        let l = Login {
            user: "alice".into(),
            pin: Secret::new("4821".into()),
            token: Secret::new("tok".into()),
        };
        let dbg = format!("{l:?}");
        assert_eq!(
            dbg,
            r#"Login { user: "alice", pin: [REDACTED], token: [REDACTED] }"#
        );
        assert_eq!(l.pin.to_string(), "[REDACTED]");
        assert_eq!(l.pin.expose(), "4821");
    }

    #[test]
    fn deserialises_from_plain_json() {
        let s: Secret<String> = serde_json::from_str("\"1234\"").unwrap();
        assert_eq!(s.expose(), "1234");
    }
}
