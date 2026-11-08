//! Categories and tax configuration (E06-T05). Rules: pos_domain::catalog::config.
//!
//! Categories: anyone who edits the catalogue (`CatalogEdit`). Tax rates and tax categories
//! change what customers pay, so they are the owner's (`SettingsEdit`). So is the business
//! profile printed on receipts, with this till's name (E04-T03).

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::QueryError;
use crate::records::{BusinessRecord, CategoryRecord, RegisterRecord, TaxCategoryRecord, TaxRateRecord};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::catalog::config::{
    BusinessProfile, CategoryNode, ConfigError, RatePeriod, check_category_parent, check_rate_archivable,
    check_rate_period, clean_name, clean_profile,
};
use pos_domain::time::BusinessDate;
use pos_domain::{CategoryId, ErrorCode, Permission, RatePpm, TaxCategoryId, TaxRateId};
use std::sync::Arc;

#[async_trait::async_trait]
pub trait ConfigQueries: Send + Sync {
    async fn all_categories(&self) -> Result<Vec<CategoryRecord>, QueryError>;
    async fn all_tax_rates(&self) -> Result<Vec<TaxRateRecord>, QueryError>;
    async fn all_tax_categories(&self) -> Result<Vec<TaxCategoryRecord>, QueryError>;
    /// The business this installation belongs to, `None` before setup.
    async fn business(&self) -> Result<Option<BusinessRecord>, QueryError>;
    async fn register(&self, id: pos_domain::RegisterId) -> Result<Option<RegisterRecord>, QueryError>;
}

/// The business profile as stored, and this till.
#[derive(Clone, Debug)]
pub struct ProfileView {
    pub business: BusinessRecord,
    pub register: RegisterRecord,
}

impl ProfileView {
    pub fn address_lines(&self) -> Vec<String> {
        address_lines(&self.business.address)
    }
}

fn address_lines(address: &serde_json::Value) -> Vec<String> {
    address
        .get("lines")
        .and_then(|l| l.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

pub struct ConfigService {
    pub store: Arc<dyn LocalStore>,
    pub config: Arc<dyn ConfigQueries>,
}

fn not_found(what: &str) -> AppError {
    AppError::Validation(format!("that {what} does not exist"), ErrorCode::ValidationFailed)
}

fn stale(expected: u64, actual: u64) -> Result<(), AppError> {
    if expected == actual {
        Ok(())
    } else {
        Err(AppError::Validation(
            "it was changed meanwhile; look again".into(),
            ErrorCode::ConflictVersion,
        ))
    }
}

#[derive(Clone, Debug)]
pub struct RateInput {
    pub name: String,
    pub rate: RatePpm,
    pub from: Option<BusinessDate>,
    pub to: Option<BusinessDate>,
}

#[derive(Clone, Debug)]
pub struct TaxCategoryInput {
    pub name: String,
    pub rates: Vec<TaxRateId>,
    pub is_default: bool,
}

impl ConfigService {
    /// The business profile and this till's name (owner).
    pub async fn business_profile(&self, ctx: &CommandContext) -> Result<ProfileView, AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        self.profile(ctx).await
    }

    async fn profile(&self, ctx: &CommandContext) -> Result<ProfileView, AppError> {
        let business = self
            .config
            .business()
            .await?
            .ok_or_else(|| not_found("business"))?;
        let register = self
            .config
            .register(ctx.register.register_id)
            .await?
            .ok_or_else(|| not_found("till"))?;
        Ok(ProfileView { business, register })
    }

    /// Saves the profile, each part only if it changed: the business (`business.updated`,
    /// audited with the names of the fields that changed) and the till's name
    /// (`register.upserted`, audited `register.updated` from/to), in one transaction.
    /// Country, time zone, currency and tax mode are not editable here: past sales and
    /// prices were made under them.
    pub async fn save_business_profile(
        &self,
        ctx: &CommandContext,
        input: &BusinessProfile,
        business_version: u64,
        register_version: u64,
    ) -> Result<ProfileView, AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        let cur = self.profile(ctx).await?;
        stale(business_version, cur.business.version)?;
        stale(register_version, cur.register.version)?;
        let p = clean_profile(input)?;
        let old = &cur.business;
        let mut changed = Vec::new();
        for (field, differs) in [
            ("name", p.name != old.name),
            ("legalName", p.legal_name != old.legal_name),
            (
                "taxRegistrationNo",
                p.tax_registration_no != old.tax_registration_no,
            ),
            ("addressLines", p.address_lines != cur.address_lines()),
            ("phone", p.phone != old.phone),
            ("email", p.email != old.email),
        ] {
            if differs {
                changed.push(field);
            }
        }
        let rename = p.register_name != cur.register.name;
        if changed.is_empty() && !rename {
            return Ok(cur);
        }
        let ids = ctx.ids.as_ref();
        let now = ctx.now();
        let mut set = WriteSet::builder(ctx.write_context());
        if !changed.is_empty() {
            let mut address = match &old.address {
                serde_json::Value::Object(_) => old.address.clone(),
                _ => serde_json::json!({}),
            };
            address["lines"] = serde_json::json!(p.address_lines);
            let record = BusinessRecord {
                name: p.name.clone(),
                legal_name: p.legal_name.clone(),
                tax_registration_no: p.tax_registration_no.clone(),
                address,
                phone: p.phone.clone(),
                email: p.email.clone(),
                version: old.version + 1,
                updated_at: now,
                ..old.clone()
            };
            let entry = AuditEntry::new(ids, audit::BUSINESS_UPDATED, "business", record.business_id)
                .with_metadata(serde_json::json!({ "changed": changed }));
            set = set
                .outbox(OutboxDraft::business_updated(ids, &record)?)
                .put_business(record, Some(old.version))
                .audit(entry);
        }
        if rename {
            let record = RegisterRecord {
                name: p.register_name.clone(),
                version: cur.register.version + 1,
                updated_at: now,
                ..cur.register.clone()
            };
            let entry = AuditEntry::new(ids, audit::REGISTER_UPDATED, "register", record.register_id)
                .with_metadata(serde_json::json!({ "from": cur.register.name, "to": record.name }));
            set = set
                .outbox(OutboxDraft::register_upserted(ids, &record)?)
                .put_register(record, Some(cur.register.version))
                .audit(entry);
        }
        self.store.apply(set.build()?).await?;
        self.profile(ctx).await
    }

    // ---------- categories ----------

    pub async fn categories(&self, ctx: &CommandContext) -> Result<Vec<CategoryRecord>, AppError> {
        ctx.authorize(Permission::CatalogView)?;
        Ok(self.config.all_categories().await?)
    }

    fn tree(all: &[CategoryRecord]) -> Vec<CategoryNode> {
        all.iter()
            .filter(|c| !c.archived)
            .map(|c| CategoryNode {
                id: c.category_id,
                parent: c.parent_id,
            })
            .collect()
    }

    /// Creates (`id: None`) or changes a category; `base_version` must be current for a change.
    pub async fn save_category(
        &self,
        ctx: &CommandContext,
        id: Option<CategoryId>,
        name: &str,
        parent: Option<CategoryId>,
        base_version: Option<u64>,
    ) -> Result<CategoryRecord, AppError> {
        ctx.authorize(Permission::CatalogEdit)?;
        let name = clean_name(name)?;
        let all = self.config.all_categories().await?;
        let ids = ctx.ids.as_ref();
        let (record, expected) = match id {
            None => (
                CategoryRecord {
                    category_id: CategoryId::generate(ids),
                    business_id: ctx.register.business_id,
                    parent_id: parent,
                    name,
                    sort_order: 0,
                    archived: false,
                    version: 1,
                    created_at: ctx.now(),
                    updated_at: ctx.now(),
                },
                None,
            ),
            Some(id) => {
                let old = all
                    .iter()
                    .find(|c| c.category_id == id && !c.archived)
                    .ok_or_else(|| not_found("category"))?;
                stale(base_version.unwrap_or(0), old.version)?;
                (
                    CategoryRecord {
                        parent_id: parent,
                        name,
                        version: old.version + 1,
                        updated_at: ctx.now(),
                        ..old.clone()
                    },
                    Some(old.version),
                )
            }
        };
        check_category_parent(&Self::tree(&all), record.category_id, parent)?;
        let entry = AuditEntry::new(ids, audit::CATALOG_CATEGORY_SAVED, "category", record.category_id)
            .with_metadata(serde_json::json!({ "name": record.name, "parent": record.parent_id }));
        let set = WriteSet::builder(ctx.write_context())
            .outbox(OutboxDraft::category_upserted(ids, &record)?)
            .put_category(record.clone(), expected)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(record)
    }

    /// Archives a category that has no active subcategories (products keep pointing to it
    /// until edited).
    pub async fn archive_category(
        &self,
        ctx: &CommandContext,
        id: CategoryId,
        base_version: u64,
    ) -> Result<(), AppError> {
        ctx.authorize(Permission::CatalogEdit)?;
        let all = self.config.all_categories().await?;
        let old = all
            .iter()
            .find(|c| c.category_id == id && !c.archived)
            .ok_or_else(|| not_found("category"))?;
        stale(base_version, old.version)?;
        if all.iter().any(|c| c.parent_id == Some(id) && !c.archived) {
            return Err(AppError::Validation(
                "move or archive its subcategories first".into(),
                ErrorCode::ValidationFailed,
            ));
        }
        let record = CategoryRecord {
            archived: true,
            version: old.version + 1,
            updated_at: ctx.now(),
            ..old.clone()
        };
        let ids = ctx.ids.as_ref();
        let entry = AuditEntry::new(ids, audit::CATALOG_CATEGORY_SAVED, "category", id)
            .with_metadata(serde_json::json!({ "archived": true }));
        let set = WriteSet::builder(ctx.write_context())
            .outbox(OutboxDraft::category_upserted(ids, &record)?)
            .put_category(record, Some(old.version))
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }

    // ---------- tax ----------

    pub async fn tax_config(
        &self,
        ctx: &CommandContext,
    ) -> Result<(Vec<TaxRateRecord>, Vec<TaxCategoryRecord>), AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        Ok((
            self.config.all_tax_rates().await?,
            self.config.all_tax_categories().await?,
        ))
    }

    fn periods(rates: &[TaxRateRecord]) -> Vec<RatePeriod> {
        rates
            .iter()
            .filter(|r| !r.archived)
            .map(|r| RatePeriod {
                id: r.tax_rate_id,
                name: r.name.clone(),
                from: r.effective_from,
                to: r.effective_to,
            })
            .collect()
    }

    pub async fn save_rate(
        &self,
        ctx: &CommandContext,
        id: Option<TaxRateId>,
        input: RateInput,
        base_version: Option<u64>,
    ) -> Result<TaxRateRecord, AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        let name = clean_name(&input.name)?;
        let rates = self.config.all_tax_rates().await?;
        let ids = ctx.ids.as_ref();
        let (record, expected) = match id {
            None => (
                TaxRateRecord {
                    tax_rate_id: TaxRateId::generate(ids),
                    business_id: ctx.register.business_id,
                    name,
                    rate: input.rate,
                    effective_from: input.from,
                    effective_to: input.to,
                    archived: false,
                    version: 1,
                    created_at: ctx.now(),
                    updated_at: ctx.now(),
                },
                None,
            ),
            Some(id) => {
                let old = rates
                    .iter()
                    .find(|r| r.tax_rate_id == id && !r.archived)
                    .ok_or_else(|| not_found("tax rate"))?;
                stale(base_version.unwrap_or(0), old.version)?;
                (
                    TaxRateRecord {
                        name,
                        rate: input.rate,
                        effective_from: input.from,
                        effective_to: input.to,
                        version: old.version + 1,
                        updated_at: ctx.now(),
                        ..old.clone()
                    },
                    Some(old.version),
                )
            }
        };
        check_rate_period(
            &Self::periods(&rates),
            &RatePeriod {
                id: record.tax_rate_id,
                name: record.name.clone(),
                from: record.effective_from,
                to: record.effective_to,
            },
        )?;
        self.apply_tax(ctx, Some(&record), expected, "rate").await?;
        Ok(record)
    }

    /// Archives a rate that no active tax category uses (else the list of those categories).
    pub async fn archive_rate(
        &self,
        ctx: &CommandContext,
        id: TaxRateId,
        base_version: u64,
    ) -> Result<(), AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        let rates = self.config.all_tax_rates().await?;
        let old = rates
            .iter()
            .find(|r| r.tax_rate_id == id && !r.archived)
            .ok_or_else(|| not_found("tax rate"))?;
        stale(base_version, old.version)?;
        let users: Vec<(String, Vec<TaxRateId>)> = self
            .config
            .all_tax_categories()
            .await?
            .into_iter()
            .filter(|c| !c.archived)
            .map(|c| (c.name, c.rate_ids))
            .collect();
        check_rate_archivable(id, &users)?;
        let record = TaxRateRecord {
            archived: true,
            version: old.version + 1,
            updated_at: ctx.now(),
            ..old.clone()
        };
        self.apply_tax(ctx, Some(&record), Some(old.version), "rate archived")
            .await
    }

    /// Creates or changes a tax category. Making it the default takes the flag from the
    /// current default in the same transaction; the default cannot simply be switched off.
    pub async fn save_tax_category(
        &self,
        ctx: &CommandContext,
        id: Option<TaxCategoryId>,
        input: TaxCategoryInput,
        base_version: Option<u64>,
    ) -> Result<TaxCategoryRecord, AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        let name = clean_name(&input.name)?;
        let rates = self.config.all_tax_rates().await?;
        let mut rate_ids = input.rates.clone();
        rate_ids.sort();
        rate_ids.dedup();
        if rate_ids
            .iter()
            .any(|r| !rates.iter().any(|x| x.tax_rate_id == *r && !x.archived))
        {
            return Err(ConfigError::UnknownRate.into());
        }
        let categories = self.config.all_tax_categories().await?;
        let ids = ctx.ids.as_ref();
        let (record, expected) = match id {
            None => (
                TaxCategoryRecord {
                    tax_category_id: TaxCategoryId::generate(ids),
                    business_id: ctx.register.business_id,
                    name,
                    is_default: input.is_default,
                    rate_ids,
                    archived: false,
                    version: 1,
                    created_at: ctx.now(),
                    updated_at: ctx.now(),
                },
                None,
            ),
            Some(id) => {
                let old = categories
                    .iter()
                    .find(|c| c.tax_category_id == id && !c.archived)
                    .ok_or_else(|| not_found("tax category"))?;
                stale(base_version.unwrap_or(0), old.version)?;
                if old.is_default && !input.is_default {
                    return Err(ConfigError::DefaultRequired.into());
                }
                (
                    TaxCategoryRecord {
                        name,
                        is_default: input.is_default,
                        rate_ids,
                        version: old.version + 1,
                        updated_at: ctx.now(),
                        ..old.clone()
                    },
                    Some(old.version),
                )
            }
        };
        // Moving the default: the old default loses the flag first (one default at all times).
        let mut previous = Vec::new();
        if record.is_default
            && let Some(old_default) = categories
                .iter()
                .find(|c| c.is_default && !c.archived && c.tax_category_id != record.tax_category_id)
        {
            previous.push((
                TaxCategoryRecord {
                    is_default: false,
                    version: old_default.version + 1,
                    updated_at: ctx.now(),
                    ..old_default.clone()
                },
                old_default.version,
            ));
        }
        if !record.is_default && !categories.iter().any(|c| c.is_default && !c.archived) {
            return Err(ConfigError::DefaultRequired.into());
        }
        let mut list = previous;
        list.push((record.clone(), expected.unwrap_or(0)));
        self.apply_tax_categories(ctx, &list, expected.is_none()).await?;
        Ok(record)
    }

    async fn apply_tax(
        &self,
        ctx: &CommandContext,
        rate: Option<&TaxRateRecord>,
        expected: Option<u64>,
        what: &str,
    ) -> Result<(), AppError> {
        let ids = ctx.ids.as_ref();
        let mut b = WriteSet::builder(ctx.write_context());
        if let Some(r) = rate {
            let entry = AuditEntry::new(ids, audit::TAX_CONFIG_UPDATED, "tax_rate", r.tax_rate_id).with_metadata(
                serde_json::json!({ "change": what, "name": r.name, "ratePpm": r.rate.ppm(), "from": r.effective_from, "to": r.effective_to }),
            );
            b = b
                .outbox(OutboxDraft::tax_rate_upserted(ids, r)?)
                .put_tax_rate(r.clone(), expected)
                .audit(entry);
        }
        self.store.apply(b.build()?).await?;
        Ok(())
    }

    /// Writes tax categories in order (the old default first), each version-checked.
    async fn apply_tax_categories(
        &self,
        ctx: &CommandContext,
        list: &[(TaxCategoryRecord, u64)],
        last_is_new: bool,
    ) -> Result<(), AppError> {
        let ids = ctx.ids.as_ref();
        let mut b = WriteSet::builder(ctx.write_context());
        for (i, (c, base)) in list.iter().enumerate() {
            let expected = if last_is_new && i == list.len() - 1 {
                None
            } else {
                Some(*base)
            };
            let entry = AuditEntry::new(ids, audit::TAX_CONFIG_UPDATED, "tax_category", c.tax_category_id).with_metadata(
                serde_json::json!({ "change": "category", "name": c.name, "isDefault": c.is_default, "rates": c.rate_ids }),
            );
            b = b
                .outbox(OutboxDraft::tax_category_upserted(ids, c)?)
                .put_tax_category(c.clone(), expected)
                .audit(entry);
        }
        self.store.apply(b.build()?).await?;
        Ok(())
    }
}
