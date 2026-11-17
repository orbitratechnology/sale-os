//! Staff management (E05-T01, IAM-001/002). Owners create, update, deactivate and reset
//! PINs; anyone signed in can change their own PIN.

use crate::audit::{self, AuditEntry};
use crate::auth::{PinHasher, blocking};
use crate::context::{AppError, CommandContext};
use crate::queries::StaffQueries;
use crate::records::StaffRecord;
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::staff::{Pin, StaffError, name_key, validate_display_name};
use pos_domain::{Permission, Role, UserId};
use std::sync::Arc;

pub struct StaffService {
    pub store: Arc<dyn LocalStore>,
    pub staff: Arc<dyn StaffQueries>,
    pub hasher: Arc<dyn PinHasher>,
}

pub struct NewStaff {
    pub display_name: String,
    pub role: Role,
    pub pin: Pin,
}

impl StaffService {
    /// All staff for the management screen (deactivated included).
    pub async fn list(&self, ctx: &CommandContext) -> Result<Vec<StaffRecord>, AppError> {
        ctx.authorize(Permission::StaffManage)?;
        Ok(self.staff.all_staff().await?)
    }

    pub async fn create(&self, ctx: &CommandContext, input: NewStaff) -> Result<StaffRecord, AppError> {
        ctx.authorize(Permission::StaffManage)?;
        let name = validate_display_name(&input.display_name)?;
        self.ensure_name_free(&name, None).await?;
        let pin = input.pin;
        let pin_hash = blocking(self.hasher.clone(), move |h| h.hash(&pin)).await?;
        let now = ctx.now();
        let record = StaffRecord {
            user_id: UserId::generate(ctx.ids.as_ref()),
            business_id: ctx.register.business_id,
            display_name: name,
            role: input.role,
            pin_hash,
            pin_updated_at: now,
            must_change_pin: false,
            active: true,
            version: 1,
            created_at: now,
            updated_at: now,
        };
        self.save(
            ctx,
            record.clone(),
            None,
            audit::STAFF_CREATED,
            serde_json::json!({ "role": input.role }),
        )
        .await?;
        Ok(record)
    }

    /// Renames and/or changes the role. `base_version` is the version the edit is based on.
    pub async fn update(
        &self,
        ctx: &CommandContext,
        user_id: UserId,
        base_version: u64,
        display_name: Option<String>,
        role: Option<Role>,
    ) -> Result<StaffRecord, AppError> {
        ctx.authorize(Permission::StaffManage)?;
        let mut record = self.load(user_id).await?;
        if let Some(name) = display_name {
            let name = validate_display_name(&name)?;
            self.ensure_name_free(&name, Some(user_id)).await?;
            record.display_name = name;
        }
        if let Some(role) = role {
            if record.role == Role::Owner && role != Role::Owner && record.active {
                self.ensure_another_owner(user_id).await?;
            }
            record.role = role;
        }
        self.bump(ctx, &mut record, base_version);
        let meta = serde_json::json!({ "role": record.role, "displayName": record.display_name });
        self.save(
            ctx,
            record.clone(),
            Some(base_version),
            audit::STAFF_UPDATED,
            meta,
        )
        .await?;
        Ok(record)
    }

    pub async fn deactivate(
        &self,
        ctx: &CommandContext,
        user_id: UserId,
        base_version: u64,
    ) -> Result<StaffRecord, AppError> {
        ctx.authorize(Permission::StaffManage)?;
        let mut record = self.load(user_id).await?;
        if record.role == Role::Owner && record.active {
            self.ensure_another_owner(user_id).await?;
        }
        record.active = false;
        self.bump(ctx, &mut record, base_version);
        self.save(
            ctx,
            record.clone(),
            Some(base_version),
            audit::STAFF_DEACTIVATED,
            serde_json::json!({}),
        )
        .await?;
        Ok(record)
    }

    /// Owner sets a temporary PIN; the staff member must choose a new one at next sign-in.
    pub async fn reset_pin(
        &self,
        ctx: &CommandContext,
        user_id: UserId,
        base_version: u64,
        pin: Pin,
    ) -> Result<StaffRecord, AppError> {
        ctx.authorize(Permission::StaffManage)?;
        let mut record = self.load(user_id).await?;
        record.pin_hash = blocking(self.hasher.clone(), move |h| h.hash(&pin)).await?;
        record.pin_updated_at = ctx.now();
        record.must_change_pin = true;
        self.bump(ctx, &mut record, base_version);
        self.save(
            ctx,
            record.clone(),
            Some(base_version),
            audit::STAFF_PIN_RESET,
            serde_json::json!({ "byOwner": true }),
        )
        .await?;
        Ok(record)
    }

    /// The signed-in user replaces their own PIN (required after an owner reset).
    pub async fn change_own_pin(
        &self,
        ctx: &CommandContext,
        current: Pin,
        new: Pin,
    ) -> Result<StaffRecord, AppError> {
        let actor = ctx.actor()?.clone();
        let mut record = self.load(actor.user_id).await?;
        if current == new {
            return Err(StaffError::SamePin.into());
        }
        let phc = record.pin_hash.clone();
        if !blocking(self.hasher.clone(), move |h| h.verify(&current, &phc)).await? {
            return Err(AppError::PinInvalid {
                remaining_attempts: 0,
            });
        }
        let base = record.version;
        record.pin_hash = blocking(self.hasher.clone(), move |h| h.hash(&new)).await?;
        record.pin_updated_at = ctx.now();
        record.must_change_pin = false;
        self.bump(ctx, &mut record, base);
        self.save(
            ctx,
            record.clone(),
            Some(base),
            audit::STAFF_PIN_RESET,
            serde_json::json!({ "byOwner": false }),
        )
        .await?;
        Ok(record)
    }

    async fn load(&self, user_id: UserId) -> Result<StaffRecord, AppError> {
        Ok(self.staff.staff(user_id).await?.ok_or(StaffError::NotFound)?)
    }

    async fn ensure_name_free(&self, name: &str, except: Option<UserId>) -> Result<(), AppError> {
        let key = name_key(name);
        let taken = self
            .staff
            .active_staff()
            .await?
            .iter()
            .any(|s| Some(s.user_id) != except && name_key(&s.display_name) == key);
        if taken {
            Err(StaffError::NameTaken.into())
        } else {
            Ok(())
        }
    }

    /// At least one *other* active owner must remain.
    async fn ensure_another_owner(&self, user_id: UserId) -> Result<(), AppError> {
        let others = self
            .staff
            .active_staff()
            .await?
            .iter()
            .filter(|s| s.role == Role::Owner && s.user_id != user_id)
            .count();
        if others == 0 {
            Err(StaffError::LastOwner.into())
        } else {
            Ok(())
        }
    }

    fn bump(&self, ctx: &CommandContext, record: &mut StaffRecord, base_version: u64) {
        record.version = base_version + 1;
        record.updated_at = ctx.now();
    }

    async fn save(
        &self,
        ctx: &CommandContext,
        record: StaffRecord,
        expected: Option<u64>,
        action: audit::AuditAction,
        metadata: serde_json::Value,
    ) -> Result<(), AppError> {
        let ids = ctx.ids.as_ref();
        let event = OutboxDraft::staff_upserted(ids, &record)?;
        let entry = AuditEntry::new(ids, action, "staff_user", record.user_id)
            .with_metadata(metadata)
            .linked_to(event.event_id);
        let set = WriteSet::builder(ctx.write_context())
            .put_staff(record, expected)
            .outbox(event)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }
}
