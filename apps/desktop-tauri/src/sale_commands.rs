//! Catalogue, scan and cart commands (E06-T02/T04, E07-T02, E09-T01).
//! Thin adapters: parse, call the service, map. Authorization happens in the services.

use crate::app_commands::err;
use crate::dto::{
    AddViaDto, CartViewDto, CatalogHitDto, DiscountDto, ProductDto, ProductInputDto, ScanOutcomeDto,
    ScanSourceDto, TaxCategoryDto, money_in, qty_in,
};
use crate::{AppServices, IpcError};
use pos_application::scan::{ScanOutcome, ScanSource};
use pos_domain::cart::{AddVia, CartChange};
use pos_domain::{CartLineId, ProductId, ScanEventId, VariantId};
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn tax_list(services: State<'_, AppServices>) -> Result<Vec<TaxCategoryDto>, IpcError> {
    let ctx = services.context().await?;
    let cats = services.app()?.catalog.tax_categories(&ctx).await.map_err(err)?;
    Ok(cats.into_iter().map(Into::into).collect())
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

/// `discount: null` removes it.
#[tauri::command]
#[specta::specta]
pub async fn cart_set_line_discount(
    services: State<'_, AppServices>,
    line_id: CartLineId,
    discount: Option<DiscountDto>,
) -> Result<CartViewDto, IpcError> {
    let ctx = services.context().await?;
    let discount = discount.map(DiscountDto::into_domain).transpose()?;
    let view = services
        .app()?
        .carts
        .set_line_discount(&ctx, line_id, discount)
        .await
        .map_err(err)?;
    CartViewDto::from_view(&view)
}

#[tauri::command]
#[specta::specta]
pub async fn cart_set_order_discount(
    services: State<'_, AppServices>,
    discount: Option<DiscountDto>,
) -> Result<CartViewDto, IpcError> {
    let ctx = services.context().await?;
    let discount = discount.map(DiscountDto::into_domain).transpose()?;
    let view = services
        .app()?
        .carts
        .set_order_discount(&ctx, discount)
        .await
        .map_err(err)?;
    CartViewDto::from_view(&view)
}

#[tauri::command]
#[specta::specta]
pub async fn cart_override_price(
    services: State<'_, AppServices>,
    line_id: CartLineId,
    unit_price_minor: String,
) -> Result<CartViewDto, IpcError> {
    let ctx = services.context().await?;
    let price = money_in("unitPriceMinor", &unit_price_minor)?;
    let view = services
        .app()?
        .carts
        .override_price(&ctx, line_id, price)
        .await
        .map_err(err)?;
    CartViewDto::from_view(&view)
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

/// Abandons the cart with a reason and returns the next (empty) one.
#[tauri::command]
#[specta::specta]
pub async fn cart_void(
    services: State<'_, AppServices>,
    revision: u32,
    reason: String,
) -> Result<CartViewDto, IpcError> {
    let ctx = services.context().await?;
    let view = services
        .app()?
        .carts
        .void(&ctx, u64::from(revision), &reason)
        .await
        .map_err(err)?;
    CartViewDto::from_view(&view)
}
