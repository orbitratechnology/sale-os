//! Error catalogue (docs/backlog/07-contracts.md §2) and shared arithmetic errors.

use serde::{Deserialize, Serialize};

/// Every error that can reach the UI or the REST API carries exactly one of these codes.
/// The UI maps codes to actionable copy; `message` strings are for logs only.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    SessionLocked,
    AuthPinInvalid,
    AuthLockedOut,
    PermissionDenied,
    ApprovalRequired,
    ApprovalInvalid,
    ValidationFailed,
    CatalogDuplicateBarcode,
    CatalogDuplicateSku,
    CatalogNotFound,
    ScanNotFound,
    ScanInvalid,
    CartRevisionMismatch,
    CartEmpty,
    PricingDiscountExceeds,
    TenderInsufficient,
    TenderChangeFromNonCash,
    TenderOverpayNonCash,
    StockNegativeBlocked,
    ReturnQtyExceeds,
    ReturnRefundMismatch,
    CashSessionRequired,
    CashSessionAlreadyOpen,
    PrintAdapterUnavailable,
    PrintUnknownOutcome,
    StoreBusy,
    StoreIo,
    StoreCorrupt,
    StoreSchemaTooNew,
    SyncNotLinked,
    SyncDeviceRevoked,
    SyncUpgradeRequired,
    SyncOutOfOrder,
    ConflictVersion,
    Internal,
}

impl ErrorCode {
    /// Whether retrying the same operation unchanged may succeed.
    pub fn retryable(self) -> bool {
        matches!(self, ErrorCode::StoreBusy | ErrorCode::SyncOutOfOrder)
    }

    /// The wire string, e.g. `"STORE_BUSY"`.
    pub fn as_str(self) -> &'static str {
        use ErrorCode::*;
        match self {
            SessionLocked => "SESSION_LOCKED",
            AuthPinInvalid => "AUTH_PIN_INVALID",
            AuthLockedOut => "AUTH_LOCKED_OUT",
            PermissionDenied => "PERMISSION_DENIED",
            ApprovalRequired => "APPROVAL_REQUIRED",
            ApprovalInvalid => "APPROVAL_INVALID",
            ValidationFailed => "VALIDATION_FAILED",
            CatalogDuplicateBarcode => "CATALOG_DUPLICATE_BARCODE",
            CatalogDuplicateSku => "CATALOG_DUPLICATE_SKU",
            CatalogNotFound => "CATALOG_NOT_FOUND",
            ScanNotFound => "SCAN_NOT_FOUND",
            ScanInvalid => "SCAN_INVALID",
            CartRevisionMismatch => "CART_REVISION_MISMATCH",
            CartEmpty => "CART_EMPTY",
            PricingDiscountExceeds => "PRICING_DISCOUNT_EXCEEDS",
            TenderInsufficient => "TENDER_INSUFFICIENT",
            TenderChangeFromNonCash => "TENDER_CHANGE_FROM_NON_CASH",
            TenderOverpayNonCash => "TENDER_OVERPAY_NON_CASH",
            StockNegativeBlocked => "STOCK_NEGATIVE_BLOCKED",
            ReturnQtyExceeds => "RETURN_QTY_EXCEEDS",
            ReturnRefundMismatch => "RETURN_REFUND_MISMATCH",
            CashSessionRequired => "CASH_SESSION_REQUIRED",
            CashSessionAlreadyOpen => "CASH_SESSION_ALREADY_OPEN",
            PrintAdapterUnavailable => "PRINT_ADAPTER_UNAVAILABLE",
            PrintUnknownOutcome => "PRINT_UNKNOWN_OUTCOME",
            StoreBusy => "STORE_BUSY",
            StoreIo => "STORE_IO",
            StoreCorrupt => "STORE_CORRUPT",
            StoreSchemaTooNew => "STORE_SCHEMA_TOO_NEW",
            SyncNotLinked => "SYNC_NOT_LINKED",
            SyncDeviceRevoked => "SYNC_DEVICE_REVOKED",
            SyncUpgradeRequired => "SYNC_UPGRADE_REQUIRED",
            SyncOutOfOrder => "SYNC_OUT_OF_ORDER",
            ConflictVersion => "CONFLICT_VERSION",
            Internal => "INTERNAL",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Implemented by every error type that can cross a layer boundary.
pub trait HasErrorCode {
    fn error_code(&self) -> ErrorCode;
}

/// Checked-arithmetic failure. Money/quantity arithmetic never panics (ADR-001).
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArithmeticError {
    #[error("arithmetic overflow")]
    Overflow,
    #[error("allocation weights must be non-negative")]
    NegativeWeight,
    #[error("cannot allocate a non-zero amount across zero parts")]
    EmptyAllocation,
}

impl HasErrorCode for ArithmeticError {
    fn error_code(&self) -> ErrorCode {
        match self {
            ArithmeticError::Overflow => ErrorCode::ValidationFailed,
            ArithmeticError::NegativeWeight | ArithmeticError::EmptyAllocation => ErrorCode::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_string_matches_serde() {
        for code in [
            ErrorCode::StoreBusy,
            ErrorCode::TenderChangeFromNonCash,
            ErrorCode::Internal,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }
}
