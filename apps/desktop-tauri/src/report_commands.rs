//! Device-local reports (E14). Definitions: docs/reporting-definitions.md.

use crate::app_commands::err;
use crate::dto::{DateRangeDto, SalesSummaryDto, StockReportDto, TenderMixDto, TopByDto, TopProductsDto};
use crate::{AppServices, IpcError};
use tauri::State;

/// Sales, discounts, tax, refunds and net sales per business day and for the range.
#[tauri::command]
#[specta::specta]
pub async fn reports_sales_summary(
    services: State<'_, AppServices>,
    range: DateRangeDto,
) -> Result<SalesSummaryDto, IpcError> {
    let ctx = services.context().await?;
    let (from, to) = range.parse()?;
    let s = services
        .app()?
        .reports
        .sales_summary(&ctx, from, to)
        .await
        .map_err(err)?;
    Ok(SalesSummaryDto::from_summary(&s, ctx.now()))
}

/// Payments taken and refunded per tender type, split by who confirmed them.
#[tauri::command]
#[specta::specta]
pub async fn reports_tender_mix(
    services: State<'_, AppServices>,
    range: DateRangeDto,
) -> Result<TenderMixDto, IpcError> {
    let ctx = services.context().await?;
    let (from, to) = range.parse()?;
    let rows = services
        .app()?
        .reports
        .tender_mix(&ctx, from, to)
        .await
        .map_err(err)?;
    Ok(TenderMixDto::from_rows(&rows, ctx.now()))
}

/// The best sellers in the range, returns netted off.
#[tauri::command]
#[specta::specta]
pub async fn reports_top_products(
    services: State<'_, AppServices>,
    range: DateRangeDto,
    by: TopByDto,
    limit: u32,
) -> Result<TopProductsDto, IpcError> {
    let ctx = services.context().await?;
    let (from, to) = range.parse()?;
    let by = match by {
        TopByDto::Quantity => pos_application::reports::TopBy::Quantity,
        TopByDto::Value => pos_application::reports::TopBy::Value,
    };
    let rows = services
        .app()?
        .reports
        .top_products(&ctx, from, to, by, limit)
        .await
        .map_err(err)?;
    Ok(TopProductsDto::from_rows(&rows, ctx.now()))
}

/// Current stock from the recorded movements; `lowOnly` keeps low and negative items.
#[tauri::command]
#[specta::specta]
pub async fn reports_stock(
    services: State<'_, AppServices>,
    low_only: bool,
) -> Result<StockReportDto, IpcError> {
    let ctx = services.context().await?;
    let rows = services.app()?.reports.stock(&ctx, low_only).await.map_err(err)?;
    Ok(StockReportDto::from_rows(&rows, ctx.now()))
}

/// Exports a dataset as CSV (RFC 4180, UTF-8 with BOM) into `<app data>/exports/`, then
/// audits it (`report.exported`). Dated datasets need `range`.
#[tauri::command]
#[specta::specta]
pub async fn export_csv(
    services: State<'_, AppServices>,
    dataset: crate::dto::ExportDatasetDto,
    range: Option<DateRangeDto>,
) -> Result<crate::dto::ExportResultDto, IpcError> {
    let ctx = services.context().await?;
    let range = range.map(|r| r.parse()).transpose()?;
    let app = services.app()?;
    let export = app
        .exports
        .build(&ctx, dataset.into_domain(), range)
        .await
        .map_err(err)?;
    let dir = services.data_dir.join("exports");
    let path = write_new_file(&dir, &export.file_name, &export.bytes).map_err(IpcError::internal)?;
    app.exports.record(&ctx, &export).await.map_err(err)?;
    Ok(crate::dto::ExportResultDto {
        path: path.display().to_string(),
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        rows: u32::try_from(export.rows).unwrap_or(u32::MAX),
    })
}

/// Writes `bytes` to a new file in `dir` (never overwriting: `name-2.csv`, `name-3.csv`…),
/// through a temporary file and a rename so a half-written file is never left behind.
fn write_new_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, "csv"));
    let mut n = 1;
    let target = loop {
        let candidate = if n == 1 {
            dir.join(name)
        } else {
            dir.join(format!("{stem}-{n}.{ext}"))
        };
        if !candidate.exists() {
            break candidate;
        }
        n += 1;
    };
    let tmp = dir.join(format!(
        ".{}.tmp",
        target.file_name().and_then(|f| f.to_str()).unwrap_or("export")
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

/// E15-T01 (owner): the audit log, newest first, filtered by action, user and dates.
#[tauri::command]
#[specta::specta]
pub async fn audit_list(
    services: State<'_, AppServices>,
    filter: crate::dto::AuditFilterDto,
    before: Option<String>,
    limit: Option<u32>,
) -> Result<crate::dto::AuditPageDto, IpcError> {
    let ctx = services.context().await?;
    let filter = filter.into_filter()?;
    let page = services
        .app()?
        .audit
        .list(&ctx, &filter, before.as_deref(), limit)
        .await
        .map_err(err)?;
    Ok(crate::dto::AuditPageDto::from_page(page))
}
