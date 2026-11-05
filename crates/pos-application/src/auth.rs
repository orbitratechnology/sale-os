//! PIN sign-in, lockout and sessions (E05-T02, IAM-005, ADR-013).

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::StaffQueries;
use crate::records::{SessionEnd, StaffRecord, ThrottleRecord};
use crate::{LocalStore, OutboxDraft, WriteSet};
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use pos_domain::staff::{FREE_ATTEMPTS, Pin, StaffError, lockout_duration_ms};
use pos_domain::{Role, SessionId, Timestamp, UserId};
use std::sync::Arc;

/// Hashes and verifies PINs. Implementations are CPU-heavy by design; callers run them on
/// a blocking thread.
pub trait PinHasher: Send + Sync {
    fn hash(&self, pin: &Pin) -> Result<String, AppError>;
    fn verify(&self, pin: &Pin, phc: &str) -> Result<bool, AppError>;
}

/// Argon2id, m = 64 MiB, t = 3, p = 1, per-user random salt, PHC string output (ADR-013).
/// Measured at about 135 ms per verification on the dev container (E05-T02 target: 50–300 ms
/// on the reference machine).
pub struct Argon2PinHasher {
    params: Params,
}

impl Argon2PinHasher {
    pub fn new() -> Self {
        Self::with_params(64 * 1024, 3, 1)
    }

    /// Weaker parameters, for tests only.
    pub fn with_params(m_kib: u32, t: u32, p: u32) -> Self {
        Self {
            params: Params::new(m_kib, t, p, None).expect("valid argon2 params"),
        }
    }

    fn argon(&self) -> Argon2<'static> {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, self.params.clone())
    }
}

impl Default for Argon2PinHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl PinHasher for Argon2PinHasher {
    fn hash(&self, pin: &Pin) -> Result<String, AppError> {
        let salt = SaltString::generate(&mut OsRng);
        self.argon()
            .hash_password(pin.expose().as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| AppError::Internal(format!("pin hashing failed: {e}")))
    }

    fn verify(&self, pin: &Pin, phc: &str) -> Result<bool, AppError> {
        let parsed = PasswordHash::new(phc)
            .map_err(|e| AppError::Internal(format!("stored PIN hash is invalid: {e}")))?;
        // Verification uses the parameters recorded in the hash, so stronger defaults later
        // keep existing PINs working.
        Ok(Argon2::default()
            .verify_password(pin.expose().as_bytes(), &parsed)
            .is_ok())
    }
}

/// Runs a hasher call on a blocking thread so the async runtime stays responsive.
pub(crate) async fn blocking<T: Send + 'static>(
    hasher: Arc<dyn PinHasher>,
    f: impl FnOnce(&dyn PinHasher) -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(move || f(hasher.as_ref()))
        .await
        .map_err(|e| AppError::Internal(format!("hashing task failed: {e}")))?
}

/// What a successful sign-in returns. The shell keeps it in memory as the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGrant {
    pub session_id: SessionId,
    pub user_id: UserId,
    pub display_name: String,
    pub role: Role,
    /// The PIN was reset by an owner: the UI must ask for a new PIN before anything else.
    pub must_change_pin: bool,
}

pub struct AuthService {
    pub store: Arc<dyn LocalStore>,
    pub staff: Arc<dyn StaffQueries>,
    pub hasher: Arc<dyn PinHasher>,
}

impl AuthService {
    /// Active staff for the PIN pad (no session required).
    pub async fn sign_in_list(&self) -> Result<Vec<StaffRecord>, AppError> {
        Ok(self.staff.active_staff().await?)
    }

    /// Verifies a PIN, applying the lockout policy, and starts a session.
    pub async fn login(
        &self,
        ctx: &CommandContext,
        user_id: UserId,
        pin: Pin,
    ) -> Result<SessionGrant, AppError> {
        let staff = self.staff.staff(user_id).await?.ok_or(StaffError::NotFound)?;
        if !staff.active {
            return Err(StaffError::Deactivated.into());
        }
        let now = ctx.now();
        let throttle = self.staff.throttle(user_id).await?.unwrap_or(ThrottleRecord {
            user_id,
            failed_attempts: 0,
            locked_until: None,
            last_failed_at: None,
        });
        if let Some(until) = throttle.locked_until.filter(|u| *u > now) {
            return Err(AppError::LockedOut {
                retry_at_ms: until.as_millis(),
            });
        }

        let phc = staff.pin_hash.clone();
        let ok = blocking(self.hasher.clone(), move |h| h.verify(&pin, &phc)).await?;
        let ids = ctx.ids.as_ref();
        let mut wctx = ctx.write_context();
        wctx.actor = Some(user_id);

        if ok {
            let session_id = SessionId::generate(ids);
            let set = WriteSet::builder(wctx)
                .put_login_throttle(ThrottleRecord {
                    user_id,
                    failed_attempts: 0,
                    locked_until: None,
                    last_failed_at: None,
                })
                .start_session(session_id, user_id)
                .audit(AuditEntry::new(ids, audit::AUTH_LOGIN, "staff_user", user_id))
                .build()?;
            self.store.apply(set).await?;
            return Ok(SessionGrant {
                session_id,
                user_id,
                display_name: staff.display_name,
                role: staff.role,
                must_change_pin: staff.must_change_pin,
            });
        }

        let failed = throttle.failed_attempts.saturating_add(1);
        let locked_until = lockout_duration_ms(failed).map(|ms| Timestamp::from_millis(now.as_millis() + ms));
        let failed_entry = AuditEntry::new(ids, audit::AUTH_LOGIN_FAILED, "staff_user", user_id)
            .with_metadata(serde_json::json!({ "failedAttempts": failed }));
        let mut b = WriteSet::builder(wctx)
            .put_login_throttle(ThrottleRecord {
                user_id,
                failed_attempts: failed,
                locked_until,
                last_failed_at: Some(now),
            })
            .outbox(OutboxDraft::audit_recorded(ids, &failed_entry)?)
            .audit(failed_entry);
        if let Some(until) = locked_until {
            let locked = AuditEntry::new(ids, audit::AUTH_LOCKED_OUT, "staff_user", user_id)
                .with_metadata(serde_json::json!({ "failedAttempts": failed, "lockedUntil": until }));
            b = b.outbox(OutboxDraft::audit_recorded(ids, &locked)?).audit(locked);
        }
        self.store.apply(b.build()?).await?;
        match locked_until {
            Some(until) => Err(AppError::LockedOut {
                retry_at_ms: until.as_millis(),
            }),
            None => Err(AppError::PinInvalid {
                remaining_attempts: FREE_ATTEMPTS.saturating_sub(failed),
            }),
        }
    }

    /// Ends a session (logout, inactivity lock, user switch, app exit).
    pub async fn end_session(&self, ctx: &CommandContext, reason: SessionEnd) -> Result<(), AppError> {
        let actor = ctx.actor()?.clone();
        let action = match reason {
            SessionEnd::InactivityLock => audit::AUTH_SESSION_LOCKED,
            _ => audit::AUTH_LOGOUT,
        };
        let set = WriteSet::builder(ctx.write_context())
            .end_session(actor.session_id, reason)
            .audit(
                AuditEntry::new(ctx.ids.as_ref(), action, "staff_user", actor.user_id)
                    .with_metadata(serde_json::json!({ "reason": reason.as_str() })),
            )
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trip_and_wrong_pin() {
        let h = Argon2PinHasher::with_params(256, 1, 1);
        let phc = h.hash(&Pin::parse("4821").unwrap()).unwrap();
        assert!(phc.starts_with("$argon2id$v=19$m=256,t=1,p=1$"));
        assert!(h.verify(&Pin::parse("4821").unwrap(), &phc).unwrap());
        assert!(!h.verify(&Pin::parse("4822").unwrap(), &phc).unwrap());
        // Salts differ per hash.
        assert_ne!(phc, h.hash(&Pin::parse("4821").unwrap()).unwrap());
    }

    /// E05-T02: verification with production parameters must cost 50–300 ms on the
    /// reference machine. Timing-sensitive, so it runs in release builds on demand:
    /// `cargo test -p pos-application --release -- --ignored production_pin_hash_cost`.
    #[test]
    #[ignore = "timing; run in release on the reference machine"]
    fn production_pin_hash_cost() {
        let h = Argon2PinHasher::new();
        let pin = Pin::parse("4821").unwrap();
        let phc = h.hash(&pin).unwrap();
        assert!(phc.starts_with("$argon2id$v=19$m=65536,t=3,p=1$"));
        let mut samples: Vec<_> = (0..20)
            .map(|_| {
                let t = std::time::Instant::now();
                assert!(h.verify(&pin, &phc).unwrap());
                t.elapsed()
            })
            .collect();
        samples.sort();
        let p95 = samples[18];
        eprintln!("argon2id verify p50 {:?} p95 {:?}", samples[10], p95);
        assert!(
            p95 <= std::time::Duration::from_millis(300),
            "too slow for the PIN pad: {p95:?}"
        );
        assert!(
            samples[0] >= std::time::Duration::from_millis(50) || cfg!(debug_assertions),
            "too cheap: {:?}",
            samples[0]
        );
    }
}
