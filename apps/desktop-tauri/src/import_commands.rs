//! Products CSV import (E06-T07, CatalogImport). Rust opens the file the user picks and
//! saves the error report where they choose; the WebView never names a path.

use crate::app_commands::err;
use crate::dto::{
    ImportCommitDto, ImportPreviewDto, ImportSummaryDto, MappingEntryDto, OnDuplicateDto, SavedFileDto,
};
use crate::{AppServices, IpcError};
use pos_domain::{ImportJobId, Permission};
use tauri::State;

/// Asks for a CSV file, reads it and stages it as an import job. `null` when the user
/// cancels the dialog. The catalogue does not change.
#[tauri::command]
#[specta::specta]
pub async fn import_products_open(
    services: State<'_, AppServices>,
) -> Result<Option<ImportPreviewDto>, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(Permission::CatalogImport).map_err(err)?;
    let chosen = services
        .file_dialog
        .open(("CSV", &["csv", "txt"]))
        .await
        .ok()
        .flatten();
    let Some(path) = chosen else {
        return Ok(None);
    };
    let size = std::fs::metadata(&path).map_err(IpcError::internal)?.len();
    if size > pos_domain::catalog::import::MAX_FILE_BYTES as u64 {
        return Err(err(pos_application::context::AppError::Validation(
            pos_domain::catalog::import::FileError::TooLarge.to_string(),
            pos_domain::ErrorCode::ValidationFailed,
        )));
    }
    let bytes = std::fs::read(&path).map_err(IpcError::internal)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "import.csv".into());
    let preview = services
        .app()?
        .imports
        .start(&ctx, &name, &bytes)
        .await
        .map_err(err)?;
    Ok(Some(ImportPreviewDto::from_preview(preview)))
}

/// Checks every row with the chosen columns and duplicate policy. Repeatable until committed.
#[tauri::command]
#[specta::specta]
pub async fn import_products_validate(
    services: State<'_, AppServices>,
    job_id: ImportJobId,
    mapping: Vec<MappingEntryDto>,
    on_duplicate: OnDuplicateDto,
) -> Result<ImportSummaryDto, IpcError> {
    let ctx = services.context().await?;
    let s = services
        .app()?
        .imports
        .validate(
            &ctx,
            job_id,
            crate::dto::mapping_in(mapping),
            on_duplicate.into_domain(),
        )
        .await
        .map_err(err)?;
    Ok(ImportSummaryDto::from_summary(s))
}

/// Creates and updates the products, 500 per transaction. After an interruption, run it
/// again: rows already committed are not imported twice.
#[tauri::command]
#[specta::specta]
pub async fn import_products_commit(
    services: State<'_, AppServices>,
    job_id: ImportJobId,
) -> Result<ImportCommitDto, IpcError> {
    let ctx = services.context().await?;
    let s = services.app()?.imports.commit(&ctx, job_id).await.map_err(err)?;
    Ok(ImportCommitDto {
        products_created: s.products_created,
        products_updated: s.products_updated,
        rows_committed: s.rows_committed,
    })
}

/// Saves the rows that were not imported (with an `error` column) where the user chooses.
#[tauri::command]
#[specta::specta]
pub async fn import_products_error_report(
    services: State<'_, AppServices>,
    job_id: ImportJobId,
) -> Result<Option<SavedFileDto>, IpcError> {
    let ctx = services.context().await?;
    let (name, bytes) = services
        .app()?
        .imports
        .error_report(&ctx, job_id)
        .await
        .map_err(err)?;
    let chosen = services
        .file_dialog
        .ask(&name, ("CSV", &["csv"]))
        .await
        .ok()
        .flatten();
    let Some(path) = chosen else {
        return Ok(None);
    };
    let path = if path.extension().is_some() {
        path
    } else {
        path.with_extension("csv")
    };
    crate::sale_commands::write_replacing(&path, &bytes).map_err(IpcError::internal)?;
    Ok(Some(SavedFileDto {
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: path.display().to_string(),
    }))
}

/// Abandons an import that has not started committing.
#[tauri::command]
#[specta::specta]
pub async fn import_products_cancel(
    services: State<'_, AppServices>,
    job_id: ImportJobId,
) -> Result<(), IpcError> {
    let ctx = services.context().await?;
    services.app()?.imports.cancel(&ctx, job_id).await.map_err(err)
}
