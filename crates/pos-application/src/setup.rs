//! First-run workspace creation (E04-T01, SET-001/002, PRD §7.1). Works with no network:
//! the cloud account is linked later (D-04), and every record written here reaches the
//! cloud through the outbox when it is (ADR-009).

use crate::audit::{self, AuditEntry};
use crate::auth::{PinHasher, blocking};
use crate::context::{AppError, RegisterContext};
use crate::queries::SetupQueries;
use crate::records::{
    BusinessRecord, DeviceIdentity, LocationRecord, RegisterMode, RegisterRecord, SettingRecord, StaffRecord,
    TaxCategoryRecord, TaxRateRecord,
};
use crate::{LocalStore, OutboxDraft, WriteContext, WriteSet};
use pos_domain::staff::{Pin, validate_display_name};
use pos_domain::time::parse_time_zone;
use pos_domain::{
    BusinessId, Clock, Currency, ErrorCode, HasErrorCode, IdGenerator, LocationId, RatePpm, RegisterId, Role,
    TaxCategoryId, TaxRateId, UserId, geo,
};
use std::sync::Arc;

pub const MAX_TAX_RATES_AT_SETUP: usize = 3;

pub struct NewTaxRate {
    pub name: String,
    pub rate: RatePpm,
}

pub struct CreateWorkspace {
    pub business_name: String,
    pub legal_name: Option<String>,
    pub address: serde_json::Value,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub country_code: String,
    pub timezone: String,
    pub currency_code: String,
    pub prices_include_tax: bool,
    pub location_name: String,
    pub register_name: String,
    pub owner_name: String,
    pub owner_pin: Pin,
    /// Up to three rates; they form the default "Standard" tax category. None = exempt.
    pub tax_rates: Vec<NewTaxRate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub register: RegisterContext,
    pub owner_user_id: UserId,
    pub register_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetupError {
    #[error("{0} is required")]
    Required(&'static str),
    #[error("{0:?} is not an ISO 3166 country code")]
    Country(String),
    #[error("{0:?} is not a known time zone")]
    TimeZone(String),
    #[error("{0}")]
    Currency(String),
    #[error("at most {MAX_TAX_RATES_AT_SETUP} tax rates can be set up here; add more in settings")]
    TooManyRates,
    #[error("tax rate names must be unique and non-empty")]
    RateName,
    #[error("setup has already been completed on this computer")]
    AlreadyDone,
}

impl HasErrorCode for SetupError {
    fn error_code(&self) -> ErrorCode {
        match self {
            SetupError::AlreadyDone => ErrorCode::PermissionDenied,
            _ => ErrorCode::ValidationFailed,
        }
    }
}

pub struct SetupService {
    pub store: Arc<dyn LocalStore>,
    pub setup: Arc<dyn SetupQueries>,
    pub hasher: Arc<dyn PinHasher>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
}

fn required(value: &str, field: &'static str) -> Result<String, SetupError> {
    let v = value.trim();
    if v.is_empty() {
        Err(SetupError::Required(field))
    } else {
        Ok(v.to_owned())
    }
}

fn optional(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

impl SetupService {
    /// Creates the business, location, register `R01`, owner, tax defaults, default
    /// settings and device identity in **one** transaction. Refused once setup is done.
    pub async fn create_workspace(&self, input: CreateWorkspace) -> Result<Workspace, AppError> {
        if self.setup.device_identity().await?.is_some() {
            return Err(SetupError::AlreadyDone.into());
        }
        let business_name = required(&input.business_name, "business name")?;
        let location_name = required(&input.location_name, "location name")?;
        let register_name = required(&input.register_name, "register name")?;
        let owner_name = validate_display_name(&input.owner_name)?;
        let country = input.country_code.trim().to_ascii_uppercase();
        if !geo::is_country_code(&country) {
            return Err(SetupError::Country(input.country_code).into());
        }
        parse_time_zone(input.timezone.trim()).map_err(|_| SetupError::TimeZone(input.timezone.clone()))?;
        // The exponent comes from the embedded ISO-4217 table, never from user input.
        let currency = Currency::from_iso(input.currency_code.trim())
            .map_err(|e| SetupError::Currency(e.to_string()))?;
        if input.tax_rates.len() > MAX_TAX_RATES_AT_SETUP {
            return Err(SetupError::TooManyRates.into());
        }
        let mut names = std::collections::HashSet::new();
        if input
            .tax_rates
            .iter()
            .any(|r| r.name.trim().is_empty() || !names.insert(r.name.trim().to_lowercase()))
        {
            return Err(SetupError::RateName.into());
        }

        let ids = self.ids.as_ref();
        let now = self.clock.now();
        let register = RegisterContext {
            business_id: BusinessId::generate(ids),
            location_id: LocationId::generate(ids),
            register_id: RegisterId::generate(ids),
        };
        let owner_id = UserId::generate(ids);
        let pin = input.owner_pin;
        let pin_hash = blocking(self.hasher.clone(), move |h| h.hash(&pin)).await?;

        let business = BusinessRecord {
            business_id: register.business_id,
            name: business_name,
            legal_name: optional(input.legal_name),
            tax_registration_no: None,
            address: input.address,
            phone: optional(input.phone),
            email: optional(input.email),
            logo_path: None,
            country_code: country,
            timezone: input.timezone.trim().to_owned(),
            currency,
            prices_include_tax: input.prices_include_tax,
            version: 1,
            created_at: now,
            updated_at: now,
        };
        let location = LocationRecord {
            location_id: register.location_id,
            business_id: register.business_id,
            name: location_name,
            address: serde_json::json!({}),
            archived: false,
            version: 1,
            created_at: now,
            updated_at: now,
        };
        let register_code = "R01".to_owned();
        let register_rec = RegisterRecord {
            register_id: register.register_id,
            business_id: register.business_id,
            location_id: register.location_id,
            name: register_name,
            code: register_code.clone(),
            receipt_prefix: String::new(),
            retired: false,
            version: 1,
            created_at: now,
            updated_at: now,
        };
        let owner = StaffRecord {
            user_id: owner_id,
            business_id: register.business_id,
            display_name: owner_name,
            role: Role::Owner,
            pin_hash,
            pin_updated_at: now,
            must_change_pin: false,
            active: true,
            version: 1,
            created_at: now,
            updated_at: now,
        };
        let rates: Vec<TaxRateRecord> = input
            .tax_rates
            .into_iter()
            .map(|r| TaxRateRecord {
                tax_rate_id: TaxRateId::generate(ids),
                business_id: register.business_id,
                name: r.name.trim().to_owned(),
                rate: r.rate,
                effective_from: None,
                effective_to: None,
                archived: false,
                version: 1,
                created_at: now,
                updated_at: now,
            })
            .collect();
        let category = TaxCategoryRecord {
            tax_category_id: TaxCategoryId::generate(ids),
            business_id: register.business_id,
            name: "Standard".into(),
            is_default: true,
            rate_ids: rates.iter().map(|r| r.tax_rate_id).collect(),
            archived: false,
            version: 1,
            created_at: now,
            updated_at: now,
        };

        let ctx = WriteContext {
            business_id: register.business_id,
            location_id: register.location_id,
            register_id: register.register_id,
            actor: Some(owner_id),
            occurred_at: now,
        };
        let business_event = OutboxDraft::business_updated(ids, &business)?;
        let created = AuditEntry::new(ids, audit::BUSINESS_CREATED, "business", register.business_id)
            .with_metadata(serde_json::json!({ "registerCode": register_code }))
            .linked_to(business_event.event_id);
        let mut b = WriteSet::builder(ctx)
            .outbox(business_event)
            .outbox(OutboxDraft::location_upserted(ids, &location)?)
            .outbox(OutboxDraft::register_upserted(ids, &register_rec)?)
            .outbox(OutboxDraft::staff_upserted(ids, &owner)?)
            .put_business(business, None)
            .put_location(location, None)
            .put_register(register_rec, None)
            .put_staff(owner, None)
            .audit(created)
            .audit(AuditEntry::new(
                ids,
                audit::LOCATION_UPSERTED,
                "location",
                register.location_id,
            ))
            .audit(AuditEntry::new(ids, audit::STAFF_CREATED, "staff_user", owner_id));
        for r in rates {
            b = b
                .outbox(OutboxDraft::tax_rate_upserted(ids, &r)?)
                .put_tax_rate(r, None);
        }
        b = b
            .outbox(OutboxDraft::tax_category_upserted(ids, &category)?)
            .put_tax_category(category, None);
        for (key, value) in crate::settings::defaults() {
            let s = SettingRecord {
                business_id: register.business_id,
                key: key.into(),
                value,
                version: 1,
                updated_at: now,
            };
            b = b
                .outbox(OutboxDraft::setting_updated(ids, &s)?)
                .put_setting(s, None);
        }
        b = b.set_device_identity(DeviceIdentity {
            installation_id: uuid_like(ids),
            business_id: register.business_id,
            register_id: register.register_id,
            mode: RegisterMode::Sandbox,
        });
        self.store.apply(b.build()?).await?;
        Ok(Workspace {
            register,
            owner_user_id: owner_id,
            register_code,
        })
    }
}

fn uuid_like(ids: &dyn IdGenerator) -> String {
    ids.new_uuid_v7().to_string()
}
