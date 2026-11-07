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
