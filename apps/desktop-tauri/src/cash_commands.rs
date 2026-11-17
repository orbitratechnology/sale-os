//! Cash session commands (E15-T02).

use crate::app_commands::err;
use crate::dto::{CashStateDto, ClosedCashDto, PrintJobDto, ReceiptDto, money_in};
use crate::{AppServices, IpcError};
use tauri::State;

/// The register's drawer: the open session (if any) and whether sales need one.
#[tauri::command]
#[specta::specta]
pub async fn cash_session_current(services: State<'_, AppServices>) -> Result<CashStateDto, IpcError> {
    let ctx = services.context().await?;
    let s = services.app()?.cash.current(&ctx).await.map_err(err)?;
    Ok(CashStateDto::from_state(&s))
}

/// Opens the drawer with a float (minor units).
#[tauri::command]
#[specta::specta]
pub async fn cash_session_open(
    services: State<'_, AppServices>,
    float_minor: String,
) -> Result<CashStateDto, IpcError> {
    let ctx = services.context().await?;
    let float = money_in("floatMinor", &float_minor)?;
    let app = services.app()?;
    app.cash.open(&ctx, float).await.map_err(err)?;
    let s = app.cash.current(&ctx).await.map_err(err)?;
    Ok(CashStateDto::from_state(&s))
}

/// Closes the drawer with the counted cash. Final. A cashier's count is blind: the reply
/// has no expected cash or variance.
#[tauri::command]
#[specta::specta]
pub async fn cash_session_close(
    services: State<'_, AppServices>,
    counted_minor: String,
    note: Option<String>,
) -> Result<ClosedCashDto, IpcError> {
    let ctx = services.context().await?;
    let counted = money_in("countedMinor", &counted_minor)?;
    let closed = services
        .app()?
        .cash
        .close(&ctx, counted, note)
        .await
        .map_err(err)?;
    Ok(ClosedCashDto::from_closed(&closed))
}

/// A closed session's Z summary as stored (`sessionId: null`: the last one on this till).
#[tauri::command]
#[specta::specta]
pub async fn cash_session_z_get(
    services: State<'_, AppServices>,
    session_id: Option<pos_domain::CashSessionId>,
    width_mm: Option<u32>,
) -> Result<ReceiptDto, IpcError> {
    let ctx = services.context().await?;
    let r = services
        .app()?
        .cash
        .z_summary(&ctx, session_id, width_mm)
        .await
        .map_err(err)?;
    Ok(ReceiptDto::from_rendered(&r))
}

/// Prints a closed session's Z summary (later copies are reprints).
#[tauri::command]
#[specta::specta]
pub async fn cash_session_z_print(
    services: State<'_, AppServices>,
    session_id: Option<pos_domain::CashSessionId>,
) -> Result<PrintJobDto, IpcError> {
    let ctx = services.context().await?;
    let app = services.app()?;
    let (receipt, _) = app.cash.z_receipt(&ctx, session_id).await.map_err(err)?;
    let job = app.prints.print_receipt(&ctx, receipt).await.map_err(err)?;
    Ok(PrintJobDto::from_record(&job))
}
