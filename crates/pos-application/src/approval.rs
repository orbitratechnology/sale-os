//! Issued approvals waiting to be used (E05-T04).
//!
//! Tokens live only in memory: they last 120 s and a restart rightly voids them. Taking a
//! token removes it, so two commands can never both use one approval.

use crate::context::{AppError, ApprovalToken};
use pos_domain::{ApprovalId, Timestamp};
use std::collections::HashMap;
use std::sync::Mutex;

/// How long an approval stays usable.
pub const APPROVAL_TTL_MS: i64 = 120_000;

#[derive(Default)]
pub struct Approvals {
    tokens: Mutex<HashMap<ApprovalId, ApprovalToken>>,
}

impl Approvals {
    pub fn issue(&self, token: ApprovalToken) {
        self.lock().insert(token.id, token);
    }

    /// Removes and returns the token. Unknown, used or expired tokens give `APPROVAL_INVALID`.
    pub fn take(&self, id: ApprovalId, now: Timestamp) -> Result<ApprovalToken, AppError> {
        let mut tokens = self.lock();
        tokens.retain(|_, t| t.expires_at > now);
        tokens.remove(&id).ok_or(AppError::ApprovalInvalid(
            "the approval was already used or has expired",
        ))
    }

    /// Puts back a token whose command failed for another reason, so the cashier can retry.
    pub fn restore(&self, token: ApprovalToken) {
        self.issue(token);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ApprovalId, ApprovalToken>> {
        // A panic while holding this lock cannot leave the map half-updated: recover it.
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_domain::{ApprovalScope, IdGenerator, Permission, UserId, UuidV7Generator};

    fn token(expires_ms: i64) -> ApprovalToken {
        let ids: &dyn IdGenerator = &UuidV7Generator;
        ApprovalToken {
            id: ApprovalId::generate(ids),
            approver: UserId::generate(ids),
            scope: ApprovalScope {
                permission: Permission::CartVoid,
                entity_id: "cart".into(),
                max_ppm: None,
            },
            expires_at: Timestamp::from_millis(expires_ms),
        }
    }

    #[test]
    fn a_token_is_single_use_and_expires() {
        let a = Approvals::default();
        let t = token(120_000);
        a.issue(t.clone());
        assert_eq!(a.take(t.id, Timestamp::from_millis(1)).unwrap(), t);
        assert!(matches!(
            a.take(t.id, Timestamp::from_millis(1)),
            Err(AppError::ApprovalInvalid(_))
        ));
        a.restore(t.clone());
        assert!(matches!(
            a.take(t.id, Timestamp::from_millis(120_000)),
            Err(AppError::ApprovalInvalid(_))
        ));
    }
}
