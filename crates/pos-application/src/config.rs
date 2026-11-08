//! Categories and tax configuration (E06-T05). Rules: pos_domain::catalog::config.
//!
//! Categories: anyone who edits the catalogue (`CatalogEdit`). Tax rates and tax categories
//! change what customers pay, so they are the owner's (`SettingsEdit`).

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::QueryError;
use crate::records::{CategoryRecord, TaxCategoryRecord, TaxRateRecord};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::catalog::config::{
    CategoryNode, ConfigError, RatePeriod, check_category_parent, check_rate_archivable, check_rate_period,
    clean_name,
};
use pos_domain::time::BusinessDate;
use pos_domain::{CategoryId, ErrorCode, Permission, RatePpm, TaxCategoryId, TaxRateId};
use std::sync::Arc;

#[async_trait::async_trait]
pub trait ConfigQueries: Send + Sync {
    async fn all_categories(&self) -> Result<Vec<CategoryRecord>, QueryError>;
    async fn all_tax_rates(&self) -> Result<Vec<TaxRateRecord>, QueryError>;
    async fn all_tax_categories(&self) -> Result<Vec<TaxCategoryRecord>, QueryError>;
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
