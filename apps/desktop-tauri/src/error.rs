use pos_domain::{ErrorCode, HasErrorCode};
use serde::Serialize;

/// The error shape of every IPC command (docs/backlog/07-contracts.md §2).
/// `message` is for logs; the UI maps `code` to actionable copy.
#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct IpcError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    /// Matches the log line written when the error was produced.
    pub correlation_id: String,
    /// Structured facts the UI needs for some codes (07-contracts §2). Boxed to keep
    /// `Result<_, IpcError>` small; the wire format is unaffected.
    pub details: Option<Box<ErrorDetails>>,
}

/// Typed `details` (07-contracts §2). Only the fields relevant to the code are set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetails {
    /// `AUTH_PIN_INVALID`: wrong PINs left before a temporary lock.
    pub remaining_attempts: Option<u32>,
    /// `AUTH_LOCKED_OUT`: when sign-in is possible again, epoch ms as a decimal string.
    pub retry_at_ms: Option<String>,
    /// `APPROVAL_REQUIRED`: what to get approved (pass it to `auth_request_approval`).
    pub scope: Option<pos_domain::ApprovalScope>,
    /// Validation and duplicate errors: the input at fault, for an inline message
    /// (`name`, `sku`, `barcode`, `price`, `scanQty`, `lowStockThreshold`, `options`, `variants`).
    pub field: Option<String>,
    /// The offending value when there is one (the SKU or barcode).
    pub value: Option<String>,
    /// `CATALOG_DUPLICATE_*`: the product that already uses the value.
    pub owner_product_id: Option<pos_domain::ProductId>,
}

/// Which product-editor input a catalogue error is about.
fn catalog_details(e: &pos_domain::catalog::CatalogError) -> Option<ErrorDetails> {
    use pos_domain::catalog::CatalogError as C;
    let at = |field: &str, value: Option<&str>, owner: Option<pos_domain::ProductId>| ErrorDetails {
        field: Some(field.into()),
        value: value.map(Into::into),
        owner_product_id: owner,
        ..ErrorDetails::default()
    };
    Some(match e {
        C::NameRequired | C::NameTooLong => at("name", None, None),
        C::NoVariants | C::UnknownVariant(_) => at("variants", None, None),
        C::NegativePrice => at("price", None, None),
        C::FractionNotAllowed(..) => at("scanQty", None, None),
        C::NotPositive(what) if what.contains("threshold") => at("lowStockThreshold", None, None),
        C::NotPositive(_) => at("scanQty", None, None),
        C::DuplicateSku(sku) => at("sku", Some(sku), None),
        C::SkuTaken { sku, owner } => at("sku", Some(sku), Some(*owner)),
        C::SkuTooLong => at("sku", None, None),
        C::DuplicateBarcode(v) => at("barcode", Some(v), None),
        C::BarcodeTaken { value, owner } => at("barcode", Some(value), Some(*owner)),
        C::Barcode(_) => at("barcode", None, None),
        C::DuplicateOptions => at("options", None, None),
        C::Archived | C::NotFound => return None,
    })
}

impl IpcError {
    /// Maps any catalogued error and logs it with a correlation id.
    pub fn from_error<E: HasErrorCode + std::fmt::Display>(e: &E) -> Self {
        Self::new(e.error_code(), e.to_string())
    }

    /// For failures without a catalogued code: always `INTERNAL`.
    pub fn internal(e: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::Internal, e.to_string())
    }

    /// Maps an application error, filling `details` where the contract defines them.
    pub fn from_app(e: &pos_application::context::AppError) -> Self {
        use pos_application::context::AppError;
        let mut out = Self::from_error(e);
        out.details = match e {
            AppError::PinInvalid { remaining_attempts } => Some(Box::new(ErrorDetails {
                remaining_attempts: Some(*remaining_attempts),
                ..ErrorDetails::default()
            })),
            AppError::LockedOut { retry_at_ms } => Some(Box::new(ErrorDetails {
                retry_at_ms: Some(retry_at_ms.to_string()),
                ..ErrorDetails::default()
            })),
            AppError::ApprovalRequired(scope) => Some(Box::new(ErrorDetails {
                scope: Some(scope.clone()),
                ..ErrorDetails::default()
            })),
            AppError::Catalog(c) => catalog_details(c).map(Box::new),
            _ => None,
        };
        out
    }

    /// A catalogued code raised by the shell itself.
    pub fn coded(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, message.into())
    }

    fn new(code: ErrorCode, message: String) -> Self {
        let correlation_id = uuid::Uuid::now_v7().to_string();
        if code == ErrorCode::Internal {
            tracing::error!(%correlation_id, code = code.as_str(), %message, "ipc error");
        } else {
            tracing::warn!(%correlation_id, code = code.as_str(), %message, "ipc error");
        }
        Self {
            code,
            retryable: code.retryable(),
            message,
            correlation_id,
            details: None,
        }
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for IpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Busy;
    impl std::fmt::Display for Busy {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("busy")
        }
    }
    impl HasErrorCode for Busy {
        fn error_code(&self) -> ErrorCode {
            ErrorCode::StoreBusy
        }
    }

    #[test]
    fn serialises_to_the_contract_shape() {
        let e = IpcError::from_error(&Busy);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["code"], "STORE_BUSY");
        assert_eq!(v["retryable"], true);
        assert_eq!(v["message"], "busy");
        assert_eq!(v["correlationId"].as_str().unwrap().len(), 36);
        assert_eq!(v["details"], serde_json::Value::Null);
    }

    #[test]
    fn uncatalogued_errors_are_internal() {
        let e = IpcError::internal("boom");
        assert_eq!(e.code, ErrorCode::Internal);
        assert!(!e.retryable);
    }
}

#[cfg(test)]
mod details_tests {
    use super::*;
    use pos_application::context::AppError;

    #[test]
    fn sign_in_errors_carry_structured_details() {
        let e = IpcError::from_app(&AppError::PinInvalid {
            remaining_attempts: 2,
        });
        assert_eq!(e.code, ErrorCode::AuthPinInvalid);
        assert_eq!(e.details.unwrap().remaining_attempts, Some(2));
        let owner = pos_domain::ProductId::generate(&pos_domain::UuidV7Generator);
        let e = IpcError::from_app(&AppError::Catalog(
            pos_domain::catalog::CatalogError::BarcodeTaken {
                value: "5449000000996".into(),
                owner,
            },
        ));
        let d = e.details.unwrap();
        assert_eq!(
            (d.field.as_deref(), d.value.as_deref(), d.owner_product_id),
            (Some("barcode"), Some("5449000000996"), Some(owner))
        );
        let e = IpcError::from_app(&AppError::LockedOut {
            retry_at_ms: 1_767_225_630_000,
        });
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["details"]["retryAtMs"], "1767225630000");
    }
}
