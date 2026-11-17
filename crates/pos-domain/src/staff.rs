//! Staff identity rules (IAM-001/002/005, ADR-013). No HR data: name, role, PIN, status.

use crate::{ErrorCode, HasErrorCode, Secret};

/// A staff PIN: 4 to 8 ASCII digits. Wrapped in `Secret` so it cannot be logged.
#[derive(Clone, PartialEq, Eq)]
pub struct Pin(Secret<String>);

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pin([REDACTED])")
    }
}

impl Pin {
    pub fn parse(s: &str) -> Result<Pin, StaffError> {
        if (4..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) {
            Ok(Pin(Secret::new(s.to_owned())))
        } else {
            Err(StaffError::PinFormat)
        }
    }
    pub fn expose(&self) -> &str {
        self.0.expose()
    }
}

/// Wrong PINs allowed before the first lockout (ADR-013).
pub const FREE_ATTEMPTS: u32 = 5;
const FIRST_LOCKOUT_MS: i64 = 30_000;
const MAX_LOCKOUT_MS: i64 = 15 * 60_000;

/// How long to lock a user after `failed_attempts` consecutive wrong PINs: none for the
/// first four, 30 s at the fifth, doubling with each further failure, capped at 15 min.
pub fn lockout_duration_ms(failed_attempts: u32) -> Option<i64> {
    if failed_attempts < FREE_ATTEMPTS {
        return None;
    }
    let doublings = (failed_attempts - FREE_ATTEMPTS).min(20);
    Some(
        FIRST_LOCKOUT_MS
            .saturating_mul(1i64 << doublings)
            .min(MAX_LOCKOUT_MS),
    )
}

pub const MAX_NAME_LEN: usize = 40;

/// Trims and validates a staff display name.
pub fn validate_display_name(name: &str) -> Result<String, StaffError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(StaffError::NameRequired);
    }
    if trimmed.chars().count() > MAX_NAME_LEN {
        return Err(StaffError::NameTooLong);
    }
    if trimmed.chars().any(char::is_control) {
        return Err(StaffError::NameRequired);
    }
    Ok(trimmed.to_owned())
}

/// Case-insensitive comparison key for name uniqueness among active staff.
pub fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StaffError {
    #[error("a PIN must be 4 to 8 digits")]
    PinFormat,
    #[error("a name is required")]
    NameRequired,
    #[error("names can be at most {MAX_NAME_LEN} characters")]
    NameTooLong,
    #[error("another active staff member already has this name")]
    NameTaken,
    #[error("the business must keep at least one active owner")]
    LastOwner,
    #[error("this staff member is deactivated")]
    Deactivated,
    #[error("staff member not found")]
    NotFound,
    #[error("the new PIN must differ from the current one")]
    SamePin,
}

impl HasErrorCode for StaffError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ValidationFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_four_to_eight_digits_and_never_printed() {
        for ok in ["1234", "00000000", "4821"] {
            assert!(Pin::parse(ok).is_ok(), "{ok}");
        }
        for bad in ["123", "123456789", "12a4", " 1234", "", "１２３４"] {
            assert_eq!(Pin::parse(bad), Err(StaffError::PinFormat), "{bad:?}");
        }
        assert_eq!(format!("{:?}", Pin::parse("4821").unwrap()), "Pin([REDACTED])");
    }

    #[test]
    fn lockout_schedule() {
        assert_eq!(
            (1..=4).map(lockout_duration_ms).collect::<Vec<_>>(),
            vec![None; 4]
        );
        assert_eq!(lockout_duration_ms(5), Some(30_000));
        assert_eq!(lockout_duration_ms(6), Some(60_000));
        assert_eq!(lockout_duration_ms(7), Some(120_000));
        assert_eq!(lockout_duration_ms(10), Some(900_000)); // 30 s × 32 capped at 15 min
        assert_eq!(lockout_duration_ms(u32::MAX), Some(900_000));
    }

    #[test]
    fn names() {
        assert_eq!(validate_display_name("  Ada  "), Ok("Ada".into()));
        assert_eq!(validate_display_name("   "), Err(StaffError::NameRequired));
        assert_eq!(
            validate_display_name(&"x".repeat(41)),
            Err(StaffError::NameTooLong)
        );
        assert_eq!(name_key(" ADA "), name_key("ada"));
    }
}
