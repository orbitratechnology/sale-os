//! Audit actions (docs/backlog/07-contracts.md §5). Every state-changing command records
//! one of these in the same transaction as its change (ADR-008).

use pos_domain::{AuditId, EventId, IdGenerator, UserId};
use serde::Serialize;

/// A catalogued audit action. Construct only from the constants in this module.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct AuditAction(&'static str);

impl AuditAction {
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

macro_rules! actions {
    ($($name:ident = $value:literal),+ $(,)?) => {
        $(pub const $name: AuditAction = AuditAction($value);)+
        /// Every catalogued action, for documentation and completeness tests.
        pub const ALL: &[AuditAction] = &[$($name),+];
    };
}

actions! {
    AUTH_LOGIN = "auth.login",
    AUTH_LOGIN_FAILED = "auth.login_failed",
    AUTH_LOCKED_OUT = "auth.locked_out",
    AUTH_LOGOUT = "auth.logout",
    AUTH_SESSION_LOCKED = "auth.session_locked",
    AUTH_APPROVAL_GRANTED = "auth.approval_granted",
    STAFF_CREATED = "staff.created",
    STAFF_UPDATED = "staff.updated",
    STAFF_DEACTIVATED = "staff.deactivated",
    STAFF_PIN_RESET = "staff.pin_reset",
    SETTINGS_UPDATED = "settings.updated",
    BUSINESS_CREATED = "business.created",
    BUSINESS_UPDATED = "business.updated",
    LOCATION_UPSERTED = "location.upserted",
    REGISTER_UPDATED = "register.updated",
    MODE_CHANGED = "mode.changed",
    CATALOG_PRODUCT_CREATED = "catalog.product_created",
    CATALOG_PRODUCT_UPDATED = "catalog.product_updated",
    CATALOG_PRODUCT_ARCHIVED = "catalog.product_archived",
    CATALOG_IMPORT_COMMITTED = "catalog.import_committed",
    CART_CLEARED = "cart.cleared",
    CART_VOIDED = "cart.voided",
    CART_PRICE_OVERRIDDEN = "cart.price_overridden",
    CART_DISCOUNT_APPLIED = "cart.discount_applied",
    CART_RECOVERED = "cart.recovered",
    CART_RECOVERY_DISCARDED = "cart.recovery_discarded",
    SALE_COMPLETED = "sale.completed",
    TENDER_RECORDED_EXTERNAL = "tender.recorded_external",
    TENDER_STATUS_CHANGED = "tender.status_changed",
    RETURN_COMPLETED = "return.completed",
    REFUND_RECORDED = "refund.recorded",
    INVENTORY_OPENING_POSTED = "inventory.opening_posted",
    INVENTORY_RECEIPT_POSTED = "inventory.receipt_posted",
    INVENTORY_ADJUSTMENT_POSTED = "inventory.adjustment_posted",
    INVENTORY_WRITE_OFF_POSTED = "inventory.write_off_posted",
    INVENTORY_NEGATIVE_FLAGGED = "inventory.negative_flagged",
    RECEIPT_REPRINTED = "receipt.reprinted",
    RECEIPT_EXPORTED_PDF = "receipt.exported_pdf",
    PRINT_UNKNOWN_RESOLVED = "print.unknown_resolved",
    CASH_SESSION_OPENED = "cash_session.opened",
    CASH_SESSION_CLOSED = "cash_session.closed",
    REPORT_EXPORTED = "report.exported",
    DATA_EXPORTED = "data.exported",
    BACKUP_CREATED = "backup.created",
    BACKUP_RESTORED = "backup.restored",
    SYNC_LINKED = "sync.linked",
    SYNC_DEVICE_REVOKED = "sync.device_revoked",
    SYNC_CONFLICT_RESOLVED = "sync.conflict_resolved",
    SYNC_EVENT_REJECTED = "sync.event_rejected",
}

/// One audit row, written in the same transaction as the change it describes.
#[derive(Clone, Debug, PartialEq)]
pub struct AuditEntry {
    pub audit_id: AuditId,
    pub action: AuditAction,
    pub entity_type: &'static str,
    pub entity_id: String,
    pub approver: Option<UserId>,
    pub metadata: serde_json::Value,
    /// Links the audit row to the outbox event that carries the change, when there is one.
    pub outbox_event_id: Option<EventId>,
}

impl AuditEntry {
    pub fn new(
        ids: &dyn IdGenerator,
        action: AuditAction,
        entity_type: &'static str,
        entity_id: impl ToString,
    ) -> Self {
        Self {
            audit_id: AuditId::generate(ids),
            action,
            entity_type,
            entity_id: entity_id.to_string(),
            approver: None,
            metadata: serde_json::Value::Object(Default::default()),
            outbox_event_id: None,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_approver(mut self, approver: UserId) -> Self {
        self.approver = Some(approver);
        self
    }

    pub fn linked_to(mut self, event: EventId) -> Self {
        self.outbox_event_id = Some(event);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalogue_is_unique_and_well_formed() {
        let mut seen = HashSet::new();
        for a in ALL {
            assert!(seen.insert(a.as_str()), "duplicate {}", a.as_str());
            let (area, verb) = a.as_str().split_once('.').expect("area.verb");
            assert!(!area.is_empty() && !verb.is_empty());
            assert!(
                a.as_str()
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_')
            );
        }
    }
}
