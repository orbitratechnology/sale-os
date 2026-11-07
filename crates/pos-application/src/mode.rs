//! Going live (E04-T02, PRD §7.1 steps 8–9): a new till starts in sandbox (practice sales,
//! marked and left out of reports); the owner switches it to live with their PIN.
//!
//! One way only: a live till never goes back to practice. The switch is refused while a cart
//! has items or a drawer session is open, so practice and live takings never mix.

use crate::approval::Approvals;
use crate::audit::{self, AuditEntry};
use crate::auth::{AuthService, PinHasher, Verified};
use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, CashQueries, SetupQueries, StaffQueries};
use crate::records::RegisterMode;
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::staff::Pin;
use pos_domain::{ErrorCode, Permission};
use std::sync::Arc;

pub struct ModeService {
    pub store: Arc<dyn LocalStore>,
    pub staff: Arc<dyn StaffQueries>,
    pub hasher: Arc<dyn PinHasher>,
    pub setup: Arc<dyn SetupQueries>,
    pub carts: Arc<dyn CartQueries>,
    pub cash: Arc<dyn CashQueries>,
}

fn refuse(msg: &str) -> AppError {
    AppError::Validation(msg.into(), ErrorCode::ValidationFailed)
}

impl ModeService {
    /// The owner switches this till from sandbox to live, confirming with their own PIN.
    pub async fn go_live(&self, ctx: &CommandContext, pin: Pin) -> Result<(), AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        let actor = ctx.actor()?.clone();
        let identity = self
            .setup
            .device_identity()
            .await?
            .ok_or_else(|| refuse("this till is not set up"))?;
        if identity.mode == RegisterMode::Live {
            return Err(refuse("this till is already live"));
        }
        if self
            .carts
            .open_cart(ctx.register.register_id)
            .await?
            .is_some_and(|c| !c.lines.is_empty())
        {
            return Err(refuse("finish or clear the cart before going live"));
        }
        if self
            .cash
            .open_cash_session(ctx.register.register_id)
            .await?
            .is_some()
        {
            return Err(refuse("close the practice drawer session before going live"));
        }
        // The same PIN check as sign-in: wrong PINs count towards the lockout.
        let Verified { reset, .. } = AuthService {
            store: self.store.clone(),
            staff: self.staff.clone(),
            hasher: self.hasher.clone(),
            approvals: Arc::new(Approvals::default()),
        }
        .verify_pin(ctx, actor.user_id, pin)
        .await?;
        let ids = ctx.ids.as_ref();
        let entry = AuditEntry::new(ids, audit::MODE_CHANGED, "register", ctx.register.register_id)
            .with_metadata(serde_json::json!({ "from": "sandbox", "to": "live" }));
        let set = WriteSet::builder(ctx.write_context())
            .put_login_throttle(reset)
            .set_register_mode(RegisterMode::Sandbox, RegisterMode::Live)
            .outbox(OutboxDraft::audit_recorded(ids, &entry)?)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }
}
