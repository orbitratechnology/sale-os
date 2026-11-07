//! Catalogue, scan and cart commands (E06-T02/T04, E07-T02, E09-T01).
//! Thin adapters: parse, call the service, map. Authorization happens in the services.

use crate::app_commands::err;
use crate::dto::{
    AddViaDto, CartViewDto, CatalogHitDto, DiscountDto, ProductDto, ProductFilterDto, ProductInputDto,
    ProductPageDto, ScanOutcomeDto, ScanSourceDto, TaxCategoryDto, money_in, qty_in,
};
use crate::dto::{CompletedSaleDto, TenderInputDto};
use crate::{AppServices, IpcError};
use pos_application::scan::{ScanOutcome, ScanSource};
use pos_domain::cart::{AddVia, CartChange};
use pos_domain::{ApprovalId, CartLineId, ProductId, ScanEventId, VariantId};
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn tax_list(services: State<'_, AppServices>) -> Result<Vec<TaxCategoryDto>, IpcError> {
    let ctx = services.context().await?;
    let cats = services.app()?.catalog.tax_categories(&ctx).await.map_err(err)?;
    Ok(cats.into_iter().map(Into::into).collect())
}

/// The product list (E06-T06a): one page, by name, with summary columns.
#[tauri::command]
#[specta::specta]
pub async fn product_list(
    services: State<'_, AppServices>,
    filter: ProductFilterDto,
    offset: u32,
    limit: u32,
) -> Result<ProductPageDto, IpcError> {
    let ctx = services.context().await?;
    let page = services
        .app()?
        .catalog
        .list(&ctx, &filter.into(), offset, limit)
        .await
        .map_err(err)?;
    ProductPageDto::from_page(page)
}

#[tauri::command]
#[specta::specta]
pub async fn product_get(
    services: State<'_, AppServices>,
    product_id: ProductId,
) -> Result<ProductDto, IpcError> {
    let ctx = services.context().await?;
    let p = services.app()?.catalog.get(&ctx, product_id).await.map_err(err)?;
    ProductDto::from_product(&p)
}

#[tauri::command]
#[specta::specta]
pub async fn product_create(
    services: State<'_, AppServices>,
    product: ProductInputDto,
) -> Result<ProductDto, IpcError> {
    let ctx = services.context().await?;
    let p = services
        .app()?
        .catalog
        .create(&ctx, product.into_domain()?)
        .await
        .map_err(err)?;
    ProductDto::from_product(&p)
}

/// `baseVersion` is the version the edit started from; a stale edit gives `CONFLICT_VERSION`.
#[tauri::command]
#[specta::specta]
pub async fn product_update(
    services: State<'_, AppServices>,
    product_id: ProductId,
    base_version: u32,
    product: ProductInputDto,
) -> Result<ProductDto, IpcError> {
    let ctx = services.context().await?;
    let p = services
        .app()?
        .catalog
        .update(&ctx, product_id, u64::from(base_version), product.into_domain()?)
        .await
        .map_err(err)?;
    ProductDto::from_product(&p)
}

#[tauri::command]
#[specta::specta]
pub async fn product_archive(
    services: State<'_, AppServices>,
    product_id: ProductId,
    base_version: u32,
) -> Result<ProductDto, IpcError> {
    let ctx = services.context().await?;
    let p = services
        .app()?
        .catalog
        .archive(&ctx, product_id, u64::from(base_version))
        .await
        .map_err(err)?;
    ProductDto::from_product(&p)
}

/// Barcode first, then SKU (BAR-004). `null` when nothing matches.
#[tauri::command]
#[specta::specta]
pub async fn catalog_lookup_code(
    services: State<'_, AppServices>,
    code: String,
) -> Result<Option<CatalogHitDto>, IpcError> {
    let ctx = services.context().await?;
    let hit = services
        .app()?
        .catalog
        .lookup_code(&ctx, &code)
        .await
        .map_err(err)?;
    Ok(hit.map(Into::into))
}

#[tauri::command]
#[specta::specta]
pub async fn catalog_search(
    services: State<'_, AppServices>,
    query: String,
    limit: u32,
) -> Result<Vec<CatalogHitDto>, IpcError> {
    let ctx = services.context().await?;
    let hits = services
        .app()?
        .catalog
        .search(&ctx, &query, limit)
        .await
        .map_err(err)?;
    Ok(hits.into_iter().map(Into::into).collect())
}

/// Every barcode delivery and every typed code (E07-T02). `scanEventId` is minted by the UI
/// per delivery: a repeated delivery is reported as `duplicate` and changes nothing.
#[tauri::command]
#[specta::specta]
pub async fn scan_submit(
    services: State<'_, AppServices>,
    scan_event_id: ScanEventId,
    raw: String,
    source: ScanSourceDto,
) -> Result<ScanOutcomeDto, IpcError> {
    let ctx = services.context().await?;
    let source = match source {
        ScanSourceDto::Scanner => ScanSource::Scanner,
        ScanSourceDto::Manual => ScanSource::Manual,
    };
    let outcome = services
        .app()?
        .carts
        .scan(&ctx, &raw, scan_event_id, source)
        .await
        .map_err(err)?;
    Ok(match outcome {
        ScanOutcome::Added { view, change } => {
            let cart = CartViewDto::from_view(&view)?;
            match change {
                CartChange::QuantityIncreased { line, .. } => {
                    ScanOutcomeDto::Incremented { cart, line_id: line }
                }
                CartChange::LineAdded(line) => ScanOutcomeDto::Added { cart, line_id: line },
                other => return Err(IpcError::internal(format!("unexpected scan change {other:?}"))),
            }
        }
        ScanOutcome::Duplicate { view } => ScanOutcomeDto::Duplicate {
            cart: CartViewDto::from_view(&view)?,
        },
        ScanOutcome::NotFound { code } => ScanOutcomeDto::NotFound { code },
    })
}

/// The open cart, priced (also after a restart: POS-011).
#[tauri::command]
#[specta::specta]
pub async fn cart_current(services: State<'_, AppServices>) -> Result<CartViewDto, IpcError> {
    let ctx = services.context().await?;
    let view = services.app()?.carts.current(&ctx).await.map_err(err)?;
    CartViewDto::from_view(&view)
}

/// From search or quick-select. `qtyMilli` defaults to the variant's scan quantity.
#[tauri::command]
#[specta::specta]
pub async fn cart_add_variant(
    services: State<'_, AppServices>,
    variant_id: VariantId,
    qty_milli: Option<String>,
    via: AddViaDto,
) -> Result<CartViewDto, IpcError> {
    let ctx = services.context().await?;
    let qty = qty_milli.as_deref().map(|q| qty_in("qtyMilli", q)).transpose()?;
    let via = match via {
        AddViaDto::Search => AddVia::Search,
        AddViaDto::QuickSelect => AddVia::QuickSelect,
    };
    let (view, _) = services
        .app()?
        .carts
        .add_variant(&ctx, variant_id, qty, via)
        .await
        .map_err(err)?;
    CartViewDto::from_view(&view)
}

/// Zero is refused: use `cart_remove_line`.
#[tauri::command]
#[specta::specta]
pub async fn cart_set_quantity(
    services: State<'_, AppServices>,
    line_id: CartLineId,
    qty_milli: String,
) -> Result<CartViewDto, IpcError> {
    let ctx = services.context().await?;
    let qty = qty_in("qtyMilli", &qty_milli)?;
    let view = services
        .app()?
        .carts
        .set_quantity(&ctx, line_id, qty)
        .await
        .map_err(err)?;
    CartViewDto::from_view(&view)
}

#[tauri::command]
#[specta::specta]
pub async fn cart_remove_line(
    services: State<'_, AppServices>,
    line_id: CartLineId,
) -> Result<CartViewDto, IpcError> {
    let ctx = services.context().await?;
    let view = services
        .app()?
        .carts
        .remove_line(&ctx, line_id)
        .await
        .map_err(err)?;
    CartViewDto::from_view(&view)
}

/// `discount: null` removes it. Above the cashier limit: a reason, and for a cashier an
/// approval (`APPROVAL_REQUIRED` carries the scope; retry with `approvalTokenId`).
#[tauri::command]
#[specta::specta]
pub async fn cart_set_line_discount(
    services: State<'_, AppServices>,
    line_id: CartLineId,
    discount: Option<DiscountDto>,
    approval_token_id: Option<ApprovalId>,
) -> Result<CartViewDto, IpcError> {
    let discount = discount.map(DiscountDto::into_domain).transpose()?;
    let (ctx, token) = services.context_with_approval(approval_token_id).await?;
    let res = match services
        .app()?
        .carts
        .set_line_discount(&ctx, line_id, discount)
        .await
    {
        Ok(view) => CartViewDto::from_view(&view),
        Err(e) => Err(err(e)),
    };
    services.settle(token, &res);
    res
}

/// As `cart_set_line_discount`, for the whole order.
#[tauri::command]
#[specta::specta]
pub async fn cart_set_order_discount(
    services: State<'_, AppServices>,
    discount: Option<DiscountDto>,
    approval_token_id: Option<ApprovalId>,
) -> Result<CartViewDto, IpcError> {
    let discount = discount.map(DiscountDto::into_domain).transpose()?;
    let (ctx, token) = services.context_with_approval(approval_token_id).await?;
    let res = match services.app()?.carts.set_order_discount(&ctx, discount).await {
        Ok(view) => CartViewDto::from_view(&view),
        Err(e) => Err(err(e)),
    };
    services.settle(token, &res);
    res
}

/// A new unit price for a line, with a reason. A cashier needs an approval.
#[tauri::command]
#[specta::specta]
pub async fn cart_override_price(
    services: State<'_, AppServices>,
    line_id: CartLineId,
    unit_price_minor: String,
    reason: String,
    approval_token_id: Option<ApprovalId>,
) -> Result<CartViewDto, IpcError> {
    let price = money_in("unitPriceMinor", &unit_price_minor)?;
    let (ctx, token) = services.context_with_approval(approval_token_id).await?;
    let res = match services
        .app()?
        .carts
        .override_price(&ctx, line_id, price, &reason)
        .await
    {
        Ok(view) => CartViewDto::from_view(&view),
        Err(e) => Err(err(e)),
    };
    services.settle(token, &res);
    res
}

/// POS-003: `revision` must be the one the cashier saw when confirming.
#[tauri::command]
#[specta::specta]
pub async fn cart_clear(services: State<'_, AppServices>, revision: u32) -> Result<CartViewDto, IpcError> {
    let ctx = services.context().await?;
    let view = services
        .app()?
        .carts
        .clear(&ctx, u64::from(revision))
        .await
        .map_err(err)?;
    CartViewDto::from_view(&view)
}

/// Abandons the cart with a reason and returns the next (empty) one. A cashier needs an
/// approval when the cart has lines.
#[tauri::command]
#[specta::specta]
pub async fn cart_void(
    services: State<'_, AppServices>,
    revision: u32,
    reason: String,
    approval_token_id: Option<ApprovalId>,
) -> Result<CartViewDto, IpcError> {
    let (ctx, token) = services.context_with_approval(approval_token_id).await?;
    let res = match services
        .app()?
        .carts
        .void(&ctx, u64::from(revision), &reason)
        .await
    {
        Ok(view) => CartViewDto::from_view(&view),
        Err(e) => Err(err(e)),
    };
    services.settle(token, &res);
    res
}

/// Completes the sale (E09-T08): one transaction for the receipt number, sale, tenders,
/// stock and audit, then a fresh cart. The UI shows "completed" only when this returns.
#[tauri::command]
#[specta::specta]
pub async fn checkout_complete(
    services: State<'_, AppServices>,
    cart_id: pos_domain::CartId,
    revision: u32,
    tenders: Vec<TenderInputDto>,
) -> Result<CompletedSaleDto, IpcError> {
    let ctx = services.context().await?;
    let tenders = tenders
        .into_iter()
        .map(TenderInputDto::into_domain)
        .collect::<Result<Vec<_>, _>>()?;
    let done = services
        .app()?
        .checkout
        .complete_sale(
            &ctx,
            pos_application::checkout::CompleteSale {
                cart_id,
                revision: u64::from(revision),
                tenders,
            },
        )
        .await
        .map_err(err)?;
    Ok(CompletedSaleDto::from_completed(&done))
}

/// A sale's receipt as stored at completion, rendered for 58 or 80 mm paper (E13-T03).
/// `saleId: null` is the last sale on this till. `widthMm: null` uses the setting.
#[tauri::command]
#[specta::specta]
pub async fn receipt_get(
    services: State<'_, AppServices>,
    sale_id: Option<pos_domain::SaleId>,
    width_mm: Option<u32>,
) -> Result<crate::dto::ReceiptDto, IpcError> {
    let ctx = services.context().await?;
    let r = services
        .app()?
        .receipts
        .for_sale(&ctx, sale_id, width_mm)
        .await
        .map_err(err)?;
    Ok(crate::dto::ReceiptDto::from_rendered(&r))
}

/// Prints a sale's receipt (the last sale's for `saleId: null`). The first copy is the
/// original; later copies are reprints. Runs after the sale is committed: a printer problem
/// never affects the sale.
#[tauri::command]
#[specta::specta]
pub async fn receipt_print(
    services: State<'_, AppServices>,
    sale_id: Option<pos_domain::SaleId>,
) -> Result<crate::dto::PrintJobDto, IpcError> {
    let ctx = services.context().await?;
    let job = services
        .app()?
        .prints
        .print_sale(&ctx, sale_id)
        .await
        .map_err(err)?;
    Ok(crate::dto::PrintJobDto::from_record(&job))
}

/// Tries a failed print job again (the printer refused it, so nothing was printed).
#[tauri::command]
#[specta::specta]
pub async fn print_job_retry(
    services: State<'_, AppServices>,
    job_id: pos_domain::PrintJobId,
) -> Result<crate::dto::PrintJobDto, IpcError> {
    let ctx = services.context().await?;
    let job = services.app()?.prints.retry(&ctx, job_id).await.map_err(err)?;
    Ok(crate::dto::PrintJobDto::from_record(&job))
}

/// The operator's answer to "did it print?" for a job with an unknown outcome. `printed:
/// false` prints a new copy marked REPRINT.
#[tauri::command]
#[specta::specta]
pub async fn print_job_resolve(
    services: State<'_, AppServices>,
    job_id: pos_domain::PrintJobId,
    printed: bool,
) -> Result<crate::dto::PrintJobDto, IpcError> {
    let ctx = services.context().await?;
    let job = services
        .app()?
        .prints
        .resolve_unknown(&ctx, job_id, printed)
        .await
        .map_err(err)?;
    Ok(crate::dto::PrintJobDto::from_record(&job))
}

// ---------- returns (E12) ----------

/// Finds a sale by the number on its receipt (typed, or scanned from the receipt's
/// barcode), with what can still be returned per line.
#[tauri::command]
#[specta::specta]
pub async fn return_find(
    services: State<'_, AppServices>,
    receipt_number: String,
) -> Result<crate::dto::ReturnableSaleDto, IpcError> {
    let ctx = services.context().await?;
    let sale = services
        .app()?
        .returns
        .find(&ctx, &receipt_number)
        .await
        .map_err(err)?;
    Ok(crate::dto::ReturnableSaleDto::from_sale(&sale))
}

/// What returning `lines` would refund. Nothing is written.
#[tauri::command]
#[specta::specta]
pub async fn return_preview(
    services: State<'_, AppServices>,
    sale_id: pos_domain::SaleId,
    lines: Vec<crate::dto::ReturnLineInputDto>,
) -> Result<crate::dto::ReturnPlanDto, IpcError> {
    let ctx = services.context().await?;
    let lines = lines
        .into_iter()
        .map(crate::dto::ReturnLineInputDto::into_domain)
        .collect::<Result<Vec<_>, _>>()?;
    let plan = services
        .app()?
        .returns
        .preview(&ctx, sale_id, &lines)
        .await
        .map_err(err)?;
    Ok(crate::dto::ReturnPlanDto::from_plan(&plan))
}

/// Makes the return (E12-T02): one transaction for the return, refunds, stock, refund
/// receipt and audit. A cashier needs a manager's approval for this sale (APPROVAL_REQUIRED
/// carries the scope) unless the shop turned that off.
#[tauri::command]
#[specta::specta]
pub async fn return_create(
    services: State<'_, AppServices>,
    sale_id: pos_domain::SaleId,
    lines: Vec<crate::dto::ReturnLineInputDto>,
    reason: pos_domain::returns::ReturnReason,
    note: Option<String>,
    refunds: Vec<TenderInputDto>,
    approval_token_id: Option<ApprovalId>,
) -> Result<crate::dto::CompletedReturnDto, IpcError> {
    let lines = lines
        .into_iter()
        .map(crate::dto::ReturnLineInputDto::into_domain)
        .collect::<Result<Vec<_>, _>>()?;
    let refunds = refunds
        .into_iter()
        .map(TenderInputDto::into_domain)
        .collect::<Result<Vec<_>, _>>()?;
    let (ctx, token) = services.context_with_approval(approval_token_id).await?;
    let res = services
        .app()?
        .returns
        .create(
            &ctx,
            pos_application::returns::CreateReturn {
                sale_id,
                lines,
                reason,
                note,
                refunds,
            },
        )
        .await
        .map(|done| crate::dto::CompletedReturnDto::from_completed(&done))
        .map_err(err);
    services.settle(token, &res);
    res
}

/// A return's refund receipt as stored, rendered for 58 or 80 mm paper.
#[tauri::command]
#[specta::specta]
pub async fn return_receipt_get(
    services: State<'_, AppServices>,
    return_id: pos_domain::ReturnId,
    width_mm: Option<u32>,
) -> Result<crate::dto::ReceiptDto, IpcError> {
    let ctx = services.context().await?;
    let r = services
        .app()?
        .receipts
        .for_return(&ctx, return_id, width_mm)
        .await
        .map_err(err)?;
    Ok(crate::dto::ReceiptDto::from_rendered(&r))
}

/// Prints a return's refund receipt (later copies are reprints).
#[tauri::command]
#[specta::specta]
pub async fn return_receipt_print(
    services: State<'_, AppServices>,
    return_id: pos_domain::ReturnId,
) -> Result<crate::dto::PrintJobDto, IpcError> {
    let ctx = services.context().await?;
    let job = services
        .app()?
        .prints
        .print_return(&ctx, return_id)
        .await
        .map_err(err)?;
    Ok(crate::dto::PrintJobDto::from_record(&job))
}

// ---------- sales history (E12-T03) ----------

/// Completed sales on this device, newest first, a page at a time. `after` is the previous
/// page's `next`; `limit` defaults to 50 (at most 200).
#[tauri::command]
#[specta::specta]
pub async fn sales_list(
    services: State<'_, AppServices>,
    filter: crate::dto::SalesFilterDto,
    after: Option<String>,
    limit: Option<u32>,
) -> Result<crate::dto::SalesPageDto, IpcError> {
    let ctx = services.context().await?;
    let filter = filter.into_filter()?;
    let page = services
        .app()?
        .sales
        .list(&ctx, &filter, after.as_deref(), limit)
        .await
        .map_err(err)?;
    Ok(crate::dto::SalesPageDto::from_page(&page))
}

/// One completed sale as recorded: lines, discounts, tax, tenders and the returns made since.
#[tauri::command]
#[specta::specta]
pub async fn sales_get(
    services: State<'_, AppServices>,
    sale_id: pos_domain::SaleId,
) -> Result<crate::dto::SaleDetailDto, IpcError> {
    let ctx = services.context().await?;
    let d = services.app()?.sales.get(&ctx, sale_id).await.map_err(err)?;
    Ok(crate::dto::SaleDetailDto::from_detail(&d))
}

// ---------- inventory documents (E11-T02/T03) ----------

/// Posts an opening-stock, goods-received, adjustment or write-off document in one
/// transaction. Receiving needs `InventoryReceive`; adjusting and writing off need
/// `InventoryAdjust`. Posted documents cannot be edited: post a correcting one.
#[tauri::command]
#[specta::specta]
pub async fn inventory_post_document(
    services: State<'_, AppServices>,
    draft: crate::dto::DocumentDraftDto,
) -> Result<crate::dto::PostedDocumentDto, IpcError> {
    let ctx = services.context().await?;
    let draft = draft.into_domain()?;
    let posted = services.app()?.stock.post(&ctx, draft).await.map_err(err)?;
    Ok(crate::dto::PostedDocumentDto::from_posted(&posted))
}
