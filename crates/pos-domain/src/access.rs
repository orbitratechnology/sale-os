//! Roles and permissions (IAM-001/003, docs/backlog/07-contracts.md §1.2).
//!
//! Permissions are checked in the application layer for every command (IAM-003); the UI
//! only hides controls. "Approval" entries in the contract matrix are *not* granted to the
//! cashier: the command fails with `APPROVAL_REQUIRED` and succeeds when a user who holds
//! the permission approves it (E05-T04).

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, strum::EnumIter)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Manager,
    Cashier,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Manager => "manager",
            Role::Cashier => "cashier",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "owner" => Ok(Role::Owner),
            "manager" => Ok(Role::Manager),
            "cashier" => Ok(Role::Cashier),
            other => Err(format!("unknown role {other:?}")),
        }
    }
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, strum::EnumIter,
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    SaleComplete,
    CartLineDiscount,
    CartOrderDiscount,
    DiscountAboveThreshold,
    PriceOverride,
    CartClear,
    CartVoid,
    ExternalTenderRecord,
    SalesView,
    ReceiptReprint,
    ReturnCreate,
    ReturnApprove,
    CatalogView,
    CatalogEdit,
    CatalogImport,
    CatalogExport,
    BarcodeCreateFromScan,
    InventoryView,
    InventoryReceive,
    InventoryAdjust,
    CashSessionOpen,
    CashSessionClose,
    CashSessionViewVariance,
    ReportsView,
    ReportsExport,
    AuditView,
    SettingsEdit,
    StaffManage,
    DeviceLink,
    SyncManage,
    ConflictResolve,
    /// Simulators and dev panels; additionally restricted to debug builds by the shell.
    DevtoolsUse,
}

use Permission::*;

const CASHIER: &[Permission] = &[
    SaleComplete,
    CartLineDiscount,
    CartOrderDiscount,
    CartClear,
    ExternalTenderRecord,
    SalesView,
    ReceiptReprint,
    CatalogView,
    InventoryView,
    CashSessionOpen,
    // Blind count: a cashier may close the drawer but not see the variance.
    CashSessionClose,
];

const MANAGER_EXTRA: &[Permission] = &[
    DiscountAboveThreshold,
    PriceOverride,
    CartVoid,
    ReturnCreate,
    ReturnApprove,
    CatalogEdit,
    CatalogImport,
    CatalogExport,
    BarcodeCreateFromScan,
    InventoryReceive,
    InventoryAdjust,
    CashSessionViewVariance,
    ReportsView,
    ReportsExport,
    ConflictResolve,
];

const OWNER_EXTRA: &[Permission] = &[
    AuditView,
    SettingsEdit,
    StaffManage,
    DeviceLink,
    SyncManage,
    DevtoolsUse,
];

/// Whether `role` holds `permission` (the contract matrix, 07 §1.2).
pub fn role_has(role: Role, permission: Permission) -> bool {
    let in_list = |l: &[Permission]| l.contains(&permission);
    match role {
        Role::Cashier => in_list(CASHIER),
        Role::Manager => in_list(CASHIER) || in_list(MANAGER_EXTRA),
        Role::Owner => in_list(CASHIER) || in_list(MANAGER_EXTRA) || in_list(OWNER_EXTRA),
    }
}

/// Whether `role` can still do `permission` with another user's approval: the "approval"
/// cells of the matrix (07 §1.2). Everything else a role lacks is simply refused.
pub fn approvable(role: Role, permission: Permission) -> bool {
    !role_has(role, permission)
        && role == Role::Cashier
        && matches!(
            permission,
            DiscountAboveThreshold | PriceOverride | CartVoid | ReturnCreate
        )
}

/// What a manager approval covers (E05-T04): one permission, on one entity (a cart, a sale),
/// up to a limit. A token for a 15% discount on cart A does not cover 20%, or cart B.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ApprovalScope {
    pub permission: Permission,
    pub entity_id: String,
    /// For discounts: the largest effective rate approved, in parts per million.
    pub max_ppm: Option<u32>,
}

impl ApprovalScope {
    /// Whether an approval for `self` covers the action described by `needed`.
    pub fn covers(&self, needed: &ApprovalScope) -> bool {
        self.permission == needed.permission
            && self.entity_id == needed.entity_id
            && match (self.max_ppm, needed.max_ppm) {
                (_, None) => true,
                (Some(have), Some(need)) => need <= have,
                (None, Some(_)) => false,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn only_the_approval_cells_are_approvable() {
        for p in Permission::iter() {
            assert!(!approvable(Role::Owner, p), "{p:?}");
            assert!(!approvable(Role::Manager, p), "{p:?}");
        }
        for p in [DiscountAboveThreshold, PriceOverride, CartVoid, ReturnCreate] {
            assert!(approvable(Role::Cashier, p), "{p:?}");
        }
        for p in [
            StaffManage,
            SettingsEdit,
            InventoryAdjust,
            CatalogEdit,
            SaleComplete,
        ] {
            assert!(!approvable(Role::Cashier, p), "{p:?}");
        }
    }

    #[test]
    fn a_scope_covers_the_same_action_up_to_its_limit() {
        let s = |p, e: &str, m| ApprovalScope {
            permission: p,
            entity_id: e.into(),
            max_ppm: m,
        };
        let granted = s(DiscountAboveThreshold, "cart-a", Some(150_000));
        assert!(granted.covers(&s(DiscountAboveThreshold, "cart-a", Some(150_000))));
        assert!(granted.covers(&s(DiscountAboveThreshold, "cart-a", Some(120_000))));
        assert!(
            !granted.covers(&s(DiscountAboveThreshold, "cart-a", Some(150_001))),
            "larger discount"
        );
        assert!(
            !granted.covers(&s(DiscountAboveThreshold, "cart-b", Some(100_000))),
            "other cart"
        );
        assert!(
            !granted.covers(&s(PriceOverride, "cart-a", None)),
            "other permission"
        );
        let void = s(CartVoid, "cart-a", None);
        assert!(void.covers(&s(CartVoid, "cart-a", None)));
        assert!(!void.covers(&s(CartVoid, "cart-a", Some(1))));
    }

    #[test]
    fn owner_holds_everything_and_roles_are_nested() {
        for p in Permission::iter() {
            assert!(role_has(Role::Owner, p), "{p:?}");
            if role_has(Role::Cashier, p) {
                assert!(
                    role_has(Role::Manager, p),
                    "manager must hold cashier permission {p:?}"
                );
            }
        }
    }

    #[test]
    fn sensitive_permissions_are_not_cashier_permissions() {
        for p in [
            StaffManage,
            SettingsEdit,
            InventoryAdjust,
            ReturnCreate,
            PriceOverride,
            CashSessionViewVariance,
            AuditView,
        ] {
            assert!(!role_has(Role::Cashier, p), "{p:?}");
        }
        for p in [StaffManage, SettingsEdit, DeviceLink, AuditView] {
            assert!(!role_has(Role::Manager, p), "owner-only {p:?}");
        }
    }

    #[test]
    fn every_permission_is_assigned_to_exactly_one_tier() {
        for p in Permission::iter() {
            let tiers = [CASHIER, MANAGER_EXTRA, OWNER_EXTRA]
                .iter()
                .filter(|l| l.contains(&p))
                .count();
            assert_eq!(tiers, 1, "{p:?}");
        }
    }
}
