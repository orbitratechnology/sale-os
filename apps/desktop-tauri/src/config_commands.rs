//! Categories (CatalogEdit) and tax configuration (owner) (E06-T05); the business profile
//! (owner, E04-T03).

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

/// The business profile and this till's name (owner).
#[tauri::command]
#[specta::specta]
pub async fn business_profile_get(
    services: State<'_, AppServices>,
) -> Result<crate::dto::BusinessProfileDto, IpcError> {
    let ctx = services.context().await?;
    let v = services.app()?.config.business_profile(&ctx).await.map_err(err)?;
    crate::dto::BusinessProfileDto::from_view(&v)
}

/// Saves the profile with the versions it showed (`CONFLICT_VERSION` if either changed).
/// Only what changed is written; `business.updated` / `register.updated` are audited.
#[tauri::command]
#[specta::specta]
pub async fn business_profile_update(
    services: State<'_, AppServices>,
    input: crate::dto::BusinessProfileInputDto,
    business_version: u32,
    register_version: u32,
) -> Result<crate::dto::BusinessProfileDto, IpcError> {
    let ctx = services.context().await?;
    let v = services
        .app()?
        .config
        .save_business_profile(
            &ctx,
            &input.into_domain(),
            u64::from(business_version),
            u64::from(register_version),
        )
        .await
        .map_err(err)?;
    crate::dto::BusinessProfileDto::from_view(&v)
}

// ---------- the business logo (SET-001) ----------

/// The logo's file name in app data. Only this name is ever read: `logo_path` can arrive
/// from the cloud, and a path from elsewhere must not choose which file the till opens.
pub(crate) const LOGO_FILE: &str = "logo.png";

/// The stored logo when the business has one and its file is readable.
pub(crate) async fn stored_logo(services: &AppServices) -> Option<pos_receipts::logo::Logo> {
    let app = services.app().ok()?;
    let business = app.config.config.business().await.ok()??;
    if business.logo_path.as_deref() != Some(LOGO_FILE) {
        return None;
    }
    let bytes = std::fs::read(services.data_dir.join(LOGO_FILE)).ok()?;
    pos_receipts::logo::Logo::from_png(bytes).ok()
}

/// A logo the till cannot use: `details.field` is `logo` and `details.value` says why
/// (`tooLarge`, `notAnImage`, `unreadable`).
fn logo_error(why: &str, message: String) -> IpcError {
    let mut e = IpcError::coded(pos_domain::ErrorCode::ValidationFailed, message);
    e.details = Some(Box::new(crate::error::ErrorDetails {
        field: Some("logo".into()),
        value: Some(why.into()),
        ..crate::error::ErrorDetails::default()
    }));
    e
}

/// Asks for a PNG or JPEG (the dialog opens from Rust), shrinks it to at most 512 px and
/// keeps it as `logo.png` in app data, then records it on the business. `null` when the
/// owner cancels. Receipts saved as PDF show it at the top.
#[tauri::command]
#[specta::specta]
pub async fn business_logo_choose(
    services: State<'_, AppServices>,
    business_version: u32,
) -> Result<Option<crate::dto::BusinessProfileDto>, IpcError> {
    use pos_receipts::logo::{LogoError, MAX_INPUT_BYTES, prepare};
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::SettingsEdit).map_err(err)?;
    let chosen = services
        .file_dialog
        .open(("Images", &["png", "jpg", "jpeg"]))
        .await
        .ok()
        .flatten();
    let Some(path) = chosen else {
        return Ok(None);
    };
    let size = std::fs::metadata(&path).map_err(IpcError::internal)?.len();
    if size > MAX_INPUT_BYTES as u64 {
        return Err(logo_error("tooLarge", LogoError::TooLarge.to_string()));
    }
    let bytes = std::fs::read(&path).map_err(IpcError::internal)?;
    let logo = tokio::task::spawn_blocking(move || prepare(&bytes))
        .await
        .map_err(IpcError::internal)?
        .map_err(|e| match e {
            LogoError::TooLarge => logo_error("tooLarge", e.to_string()),
            LogoError::NotAnImage => logo_error("notAnImage", e.to_string()),
            LogoError::Unreadable(_) => logo_error("unreadable", e.to_string()),
        })?;
    // Written beside the current one, and swapped in only once the business records it.
    let staged = services.data_dir.join("logo.new.png");
    std::fs::write(&staged, &logo.png).map_err(IpcError::internal)?;
    let saved = services
        .app()?
        .config
        .set_logo(&ctx, Some(LOGO_FILE.into()), u64::from(business_version))
        .await;
    let view = match saved {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&staged);
            return Err(err(e));
        }
    };
    std::fs::rename(&staged, services.data_dir.join(LOGO_FILE)).map_err(IpcError::internal)?;
    crate::dto::BusinessProfileDto::from_view(&view).map(Some)
}

/// Removes the logo from the business and from app data.
#[tauri::command]
#[specta::specta]
pub async fn business_logo_clear(
    services: State<'_, AppServices>,
    business_version: u32,
) -> Result<crate::dto::BusinessProfileDto, IpcError> {
    let ctx = services.context().await?;
    let view = services
        .app()?
        .config
        .set_logo(&ctx, None, u64::from(business_version))
        .await
        .map_err(err)?;
    match std::fs::remove_file(services.data_dir.join(LOGO_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "the old logo file was not removed"),
    }
    crate::dto::BusinessProfileDto::from_view(&view)
}

/// The logo as a `data:image/png;base64,…` URL for the settings preview; `null` when none.
#[tauri::command]
#[specta::specta]
pub async fn business_logo_get(services: State<'_, AppServices>) -> Result<Option<String>, IpcError> {
    use base64::Engine as _;
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::SettingsEdit).map_err(err)?;
    Ok(stored_logo(&services).await.map(|l| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&l.png)
        )
    }))
}
