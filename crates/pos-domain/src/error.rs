//! Error catalogue (docs/backlog/07-contracts.md §2) and shared arithmetic errors.

use serde::{Deserialize, Serialize};

/// Every error that can reach the UI or the REST API carries exactly one of these codes.
/// The UI maps codes to actionable copy; `message` strings are for logs only.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, strum::EnumIter)]
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
    AuthCredentialsInvalid,
    AuthEmailTaken,
    AuthRateLimited,
    AuthRequired,
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
            AuthCredentialsInvalid => "AUTH_CREDENTIALS_INVALID",
            AuthEmailTaken => "AUTH_EMAIL_TAKEN",
            AuthRateLimited => "AUTH_RATE_LIMITED",
            AuthRequired => "AUTH_REQUIRED",
            ConflictVersion => "CONFLICT_VERSION",
            Internal => "INTERNAL",
        }
    }
}

/// What a code means and what the UI should offer the operator (07-contracts §2).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ErrorDoc {
    pub meaning: &'static str,
    pub ui_action: &'static str,
}

impl ErrorCode {
    /// Every code, in declaration order.
    pub fn all() -> impl Iterator<Item = ErrorCode> {
        <ErrorCode as strum::IntoEnumIterator>::iter()
    }

    /// Documentation for this code. The match is exhaustive, so a new code cannot be
    /// added without describing it; `docs/errors.md` is generated from this.
    pub fn doc(self) -> ErrorDoc {
        use ErrorCode::*;
        let (meaning, ui_action) = match self {
            SessionLocked => ("No unlocked staff session.", "Show the lock screen."),
            AuthPinInvalid => (
                "Wrong PIN. `details.remainingAttempts` says how many tries are left.",
                "Let the user retry.",
            ),
            AuthLockedOut => (
                "Too many wrong PINs. `details.retryAt` says when to try again.",
                "Show a countdown.",
            ),
            PermissionDenied => (
                "The signed-in role lacks the permission.",
                "Explain, and offer a manager sign-in.",
            ),
            ApprovalRequired => (
                "A manager approval is needed (`details.permission`, `details.scope`).",
                "Open the approval dialog, then retry with the token.",
            ),
            ApprovalInvalid => (
                "The approval token expired, was already used, or is out of scope.",
                "Ask for approval again.",
            ),
            ValidationFailed => (
                "Input failed validation. `details.fields` lists the field errors.",
                "Show inline field errors.",
            ),
            CatalogDuplicateBarcode => (
                "The barcode belongs to another active product (`details.owner`).",
                "Link to the owning product.",
            ),
            CatalogDuplicateSku => (
                "The SKU belongs to another active product.",
                "Show the conflicting product.",
            ),
            CatalogNotFound => ("The product or variant does not exist.", "Refresh the list."),
            ScanNotFound => (
                "No product matches the scanned or typed code.",
                "Offer search, or create a product if permitted.",
            ),
            ScanInvalid => (
                "The scanned input is malformed (bad check digit, partial read).",
                "Ask for a re-scan.",
            ),
            CartRevisionMismatch => (
                "The UI acted on an out-of-date cart.",
                "Reload the cart and show it again.",
            ),
            CartEmpty => (
                "The cart has no lines.",
                "Nothing to complete; keep the cart open.",
            ),
            PricingDiscountExceeds => (
                "The discount is larger than the line or order.",
                "Show the maximum allowed.",
            ),
            TenderInsufficient => (
                "Tenders do not cover the total (`details.remaining`).",
                "Show the remaining amount due.",
            ),
            TenderChangeFromNonCash => (
                "Change can only be given from cash.",
                "Reduce the non-cash amount.",
            ),
            TenderOverpayNonCash => (
                "A non-cash tender exceeds the amount due.",
                "Reduce the non-cash amount.",
            ),
            StockNegativeBlocked => (
                "The negative-stock policy blocks this sale (`details.variants`).",
                "Show the affected items; the cart is kept.",
            ),
            ReturnQtyExceeds => (
                "More is being returned than remains unreturned (`details.available`).",
                "Show the available quantity.",
            ),
            ReturnRefundMismatch => (
                "Refund tenders do not add up to the return total.",
                "Show the expected refund.",
            ),
            CashSessionRequired => ("A sale needs an open cash session.", "Offer to open a session."),
            CashSessionAlreadyOpen => (
                "This register already has an open cash session.",
                "Show the open session.",
            ),
            PrintAdapterUnavailable => ("No printer adapter is available.", "Offer PDF export instead."),
            PrintUnknownOutcome => (
                "The printer did not confirm the job.",
                "Ask whether it printed; reprint only on request.",
            ),
            StoreBusy => (
                "The local database is briefly busy. Retryable.",
                "Retry automatically up to 3 times, then show.",
            ),
            StoreIo => (
                "The local database could not be read or written. Nothing was saved.",
                "Show the error; offer diagnostics.",
            ),
            StoreCorrupt => (
                "The local database failed an integrity check.",
                "Stop and guide the owner to restore a backup.",
            ),
            StoreSchemaTooNew => (
                "The database was written by a newer app version. It was not modified.",
                "Ask the user to update the app.",
            ),
            SyncNotLinked => (
                "This register is not linked to a cloud account.",
                "Open the link wizard.",
            ),
            SyncDeviceRevoked => (
                "The owner disconnected this register from the cloud.",
                "Explain; local sales continue.",
            ),
            SyncUpgradeRequired => (
                "The cloud requires a newer app (`details.minVersion`).",
                "Prompt to update; local sales continue.",
            ),
            SyncOutOfOrder => (
                "The server expected a different event sequence (REST only).",
                "Resend automatically.",
            ),
            AuthCredentialsInvalid => (
                "Cloud sign-in failed: the email or the password is wrong (never says which).",
                "Let the owner retry or reset the password.",
            ),
            AuthEmailTaken => (
                "A cloud account with this email already exists.",
                "Offer to sign in instead.",
            ),
            AuthRateLimited => (
                "Too many cloud sign-in attempts. The `Retry-After` header says when to try again.",
                "Show when to try again.",
            ),
            AuthRequired => (
                "The cloud request has no valid token (missing, expired or wrong kind).",
                "Sign in to the cloud again.",
            ),
            ConflictVersion => (
                "Someone else changed this record first.",
                "Open the conflict view.",
            ),
            Internal => (
                "A bug. The correlation ID matches a log line.",
                "Offer the diagnostics bundle.",
            ),
        };
        ErrorDoc { meaning, ui_action }
    }

    /// The generated `docs/errors.md` content.
    pub fn catalogue_markdown() -> String {
        let mut out = String::from(
            "# Error codes\n\n\
             <!-- GENERATED from `pos_domain::ErrorCode::doc()`. Regenerate with\n\
             `UPDATE_ERRORS_DOC=1 cargo test -p pos-domain errors_doc`. Do not edit by hand. -->\n\n\
             Every error that reaches the UI or the REST API carries exactly one of these codes\n\
             (docs/backlog/07-contracts.md §2). `message` strings are for logs; the UI maps the code\n\
             to the action below.\n\n\
             | Code | Retryable | Meaning | UI action |\n|---|:-:|---|---|\n",
        );
        for code in Self::all() {
            let d = code.doc();
            let retry = if code.retryable() { "yes" } else { "" };
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                code.as_str(),
                retry,
                d.meaning,
                d.ui_action
            ));
        }
        out
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
    fn errors_doc_is_current() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/errors.md");
        let generated = ErrorCode::catalogue_markdown();
        if std::env::var_os("UPDATE_ERRORS_DOC").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            committed == generated,
            "docs/errors.md is out of date; run `UPDATE_ERRORS_DOC=1 cargo test -p pos-domain errors_doc`"
        );
    }

    #[test]
    fn every_code_is_documented_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for code in ErrorCode::all() {
            assert!(seen.insert(code.as_str()));
            let d = code.doc();
            assert!(!d.meaning.is_empty() && !d.ui_action.is_empty(), "{code}");
        }
        assert_eq!(seen.len(), 39);
    }

    #[test]
    fn wire_string_matches_serde() {
        for code in ErrorCode::all() {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }
}
