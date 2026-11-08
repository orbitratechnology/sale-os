//! Categories (CatalogEdit) and tax configuration (owner) (E06-T05).

use crate::app_commands::err;
use crate::dto::{CategoryDto, TaxCategoryConfigDto, TaxConfigDto, TaxRateConfigDto, opt_date, rate_in};
use crate::{AppServices, IpcError};
use pos_domain::{CategoryId, TaxCategoryId, TaxRateId};
use tauri::State;

/// Every category, archived ones included (flagged).
#[tauri::command]
#[specta::specta]
pub async fn categories_list(services: State<'_, AppServices>) -> Result<Vec<CategoryDto>, IpcError> {
    let ctx = services.context().await?;
    let all = services.app()?.config.categories(&ctx).await.map_err(err)?;
    all.iter().map(CategoryDto::from_record).collect()
}

/// Creates (`categoryId: null`) or renames / moves a category; at most 3 levels, no cycles.
#[tauri::command]
#[specta::specta]
pub async fn category_save(
    services: State<'_, AppServices>,
    category_id: Option<CategoryId>,
    name: String,
    parent_id: Option<CategoryId>,
    base_version: Option<u32>,
) -> Result<CategoryDto, IpcError> {
    let ctx = services.context().await?;
    let r = services
        .app()?
        .config
        .save_category(&ctx, category_id, &name, parent_id, base_version.map(u64::from))
        .await
        .map_err(err)?;
    CategoryDto::from_record(&r)
}

/// Archives a category without active subcategories.
#[tauri::command]
#[specta::specta]
pub async fn category_archive(
    services: State<'_, AppServices>,
    category_id: CategoryId,
    base_version: u32,
) -> Result<(), IpcError> {
    let ctx = services.context().await?;
    services
        .app()?
        .config
        .archive_category(&ctx, category_id, u64::from(base_version))
        .await
        .map_err(err)
}

/// (owner) Every tax rate with its dates, and every tax category with its rates.
#[tauri::command]
#[specta::specta]
pub async fn tax_config_get(services: State<'_, AppServices>) -> Result<TaxConfigDto, IpcError> {
    let ctx = services.context().await?;
    let (rates, categories) = services.app()?.config.tax_config(&ctx).await.map_err(err)?;
    Ok(TaxConfigDto {
        rates: rates
            .iter()
            .map(TaxRateConfigDto::from_record)
            .collect::<Result<_, _>>()?,
        categories: categories
            .iter()
            .map(TaxCategoryConfigDto::from_record)
            .collect::<Result<_, _>>()?,
    })
}

/// (owner) Creates or changes a tax rate. `from` inclusive, `to` exclusive (business dates);
/// two active rates with the same name may not cover the same day.
#[tauri::command]
#[specta::specta]
pub async fn tax_rate_save(
    services: State<'_, AppServices>,
    tax_rate_id: Option<TaxRateId>,
    name: String,
    rate_ppm: u32,
    from: Option<String>,
    to: Option<String>,
    base_version: Option<u32>,
) -> Result<TaxRateConfigDto, IpcError> {
    let ctx = services.context().await?;
    let input = pos_application::config::RateInput {
        name,
        rate: rate_in("ratePpm", rate_ppm)?,
        from: opt_date("from", from)?,
        to: opt_date("to", to)?,
    };
    let r = services
        .app()?
        .config
        .save_rate(&ctx, tax_rate_id, input, base_version.map(u64::from))
        .await
        .map_err(err)?;
    TaxRateConfigDto::from_record(&r)
}

/// (owner) Archives a rate no active tax category uses; otherwise `VALIDATION_FAILED` with
/// the categories in `details.value`.
#[tauri::command]
#[specta::specta]
pub async fn tax_rate_archive(
    services: State<'_, AppServices>,
    tax_rate_id: TaxRateId,
    base_version: u32,
) -> Result<(), IpcError> {
    let ctx = services.context().await?;
    services
        .app()?
        .config
        .archive_rate(&ctx, tax_rate_id, u64::from(base_version))
        .await
        .map_err(err)
}

/// (owner) Creates or changes a tax category; making it the default moves the flag.
#[tauri::command]
#[specta::specta]
pub async fn tax_category_save(
    services: State<'_, AppServices>,
    tax_category_id: Option<TaxCategoryId>,
    name: String,
    rate_ids: Vec<TaxRateId>,
    is_default: bool,
    base_version: Option<u32>,
) -> Result<TaxCategoryConfigDto, IpcError> {
    let ctx = services.context().await?;
    let r = services
        .app()?
        .config
        .save_tax_category(
            &ctx,
            tax_category_id,
            pos_application::config::TaxCategoryInput {
                name,
                rates: rate_ids,
                is_default,
            },
            base_version.map(u64::from),
        )
        .await
        .map_err(err)?;
    TaxCategoryConfigDto::from_record(&r)
}
