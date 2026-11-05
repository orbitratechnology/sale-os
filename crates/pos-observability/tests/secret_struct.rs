//! E01-T04 acceptance: logging a struct that holds a PIN and a token never writes either.

use pos_domain::Secret;
use pos_observability::{REDACTED, capture_json_lines};

#[derive(Debug)]
#[allow(dead_code)] // read through Debug
struct LoginAttempt {
    user_id: String,
    pin: Secret<String>,
    device_token: Secret<String>,
}

#[test]
fn struct_with_pin_and_token_is_redacted() {
    let attempt = LoginAttempt {
        user_id: "u-1".into(),
        pin: Secret::new("4821".into()),
        device_token: Secret::new("eyJhbGciOi.secret".into()),
    };
    let lines = capture_json_lines(|| {
        tracing::info!(?attempt, "login attempt");
        // Also when the secret is passed directly under an innocent field name.
        tracing::info!(value = %attempt.pin, "raw secret");
    });
    let raw: String = lines.iter().map(|l| l.to_string()).collect();
    assert!(!raw.contains("4821"), "{raw}");
    assert!(!raw.contains("eyJhbGciOi"), "{raw}");
    assert!(raw.contains(REDACTED));
    assert_eq!(lines[1]["value"], REDACTED);
}
