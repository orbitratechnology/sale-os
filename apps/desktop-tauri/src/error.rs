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
    }

    #[test]
    fn uncatalogued_errors_are_internal() {
        let e = IpcError::internal("boom");
        assert_eq!(e.code, ErrorCode::Internal);
        assert!(!e.retryable);
    }
}
