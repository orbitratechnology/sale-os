//! Draft cart use cases (E09-T01, POS-001..003, POS-011, BAR-005).

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, CatalogQueries, PricingSettings};
use crate::{LocalStore, WriteSet};
use pos_domain::cart::{AddVia, Cart, CartChange, CartLine, CartStatus, SellableVariant};
use pos_domain::catalog::CatalogError;
use pos_domain::pricing::{Discount, PricedCart, PricingPolicy, price_cart};
use pos_domain::time::{business_date, parse_time_zone};
use pos_domain::{BusinessDate, CartLineId, Money, Permission, Quantity, ScanEventId, VariantId};
use std::sync::Arc;
use tokio::sync::Mutex;

/// The cart as the UI shows it: lines plus the totals Rust computed (ADR-018).
#[derive(Clone, Debug, PartialEq)]
pub struct CartView {
    pub cart: Cart,
    pub priced: PricedCart,
    pub settings: PricingSettings,
    /// Lines whose current catalogue price differs from what the cart was built with, for
    /// highlighting after recovery (POS-011). Always empty until E09-T02 stores the
    /// price-at-add snapshot needed to detect it.
    pub repriced_lines: Vec<CartLineId>,
}

pub struct CartService {
    store: Arc<dyn LocalStore>,
    carts: Arc<dyn CartQueries>,
    pub(crate) catalog: Arc<dyn CatalogQueries>,
    /// Serializes load → change → save. Tauri runs commands concurrently, and two fast scans
    /// must both count. The store's revision check is the backstop if this is bypassed.
    lock: Mutex<()>,
}

/// A cart as read from storage, with the revision a save must be based on.
pub(crate) struct Loaded {
    pub cart: Cart,
    /// `None`: never saved (a fresh empty cart).
    pub base: Option<u64>,
}

impl CartService {
    pub fn new(
        store: Arc<dyn LocalStore>,
        carts: Arc<dyn CartQueries>,
        catalog: Arc<dyn CatalogQueries>,
    ) -> Self {
        Self {
            store,
            carts,
            catalog,
            lock: Mutex::new(()),
        }
    }

    pub(crate) fn store(&self) -> &Arc<dyn LocalStore> {
        &self.store
    }

    /// The variant as it sells today: current price and resolved tax rates.
    pub(crate) async fn sellable(
        &self,
        ctx: &CommandContext,
        variant: VariantId,
    ) -> Result<SellableVariant, AppError> {
        let settings = self.carts.pricing_settings().await?;
        let date = self.today(ctx, &settings)?;
        Ok(self
            .carts
            .sellable(variant, date)
            .await?
            .ok_or(CatalogError::NotFound)?)
    }

    pub(crate) async fn exclusive(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock.lock().await
    }

    fn today(&self, ctx: &CommandContext, settings: &PricingSettings) -> Result<BusinessDate, AppError> {
        let tz = parse_time_zone(&settings.timezone).map_err(|e| AppError::Internal(e.to_string()))?;
        business_date(ctx.now(), tz).map_err(|e| AppError::Internal(e.to_string()))
    }

    /// The open cart (re-read from storage and re-priced), or a new empty one.
    pub async fn current(&self, ctx: &CommandContext) -> Result<CartView, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let loaded = self.load(ctx).await?;
        self.view(ctx, loaded.cart, Vec::new()).await
    }

    pub(crate) async fn load(&self, ctx: &CommandContext) -> Result<Loaded, AppError> {
        let settings = self.carts.pricing_settings().await?;
        let date = self.today(ctx, &settings)?;
        let Some(stored) = self.carts.open_cart(ctx.register.register_id).await? else {
            return Ok(Loaded {
                cart: Cart::new(ctx.ids.as_ref()),
                base: None,
            });
        };
        let mut lines = Vec::with_capacity(stored.lines.len());
        for l in stored.lines {
            let variant = self
                .carts
                .sellable(l.variant_id, date)
                .await?
                .ok_or(CatalogError::NotFound)?;
            lines.push(CartLine {
                id: l.line_id,
                variant,
                quantity: l.quantity,
                price_override: l.price_override,
                line_discount: l.line_discount,
                added_via: l.added_via,
            });
        }
        let cart = Cart {
            id: stored.cart_id,
            status: CartStatus::Open,
            lines,
            order_discount: stored.order_discount,
            revision: stored.revision,
            void_reason: None,
        };
        Ok(Loaded {
            cart,
            base: Some(stored.revision),
        })
    }

    async fn view(
        &self,
        ctx: &CommandContext,
        cart: Cart,
        repriced: Vec<CartLineId>,
    ) -> Result<CartView, AppError> {
        let settings = self.carts.pricing_settings().await?;
        let policy = PricingPolicy::new(settings.tax_mode);
        let priced = price_cart(&cart.pricing_lines(), cart.order_discount.as_ref(), &policy)
            .map_err(|e| AppError::Validation(e.to_string(), pos_domain::HasErrorCode::error_code(&e)))?;
        let _ = ctx;
        Ok(CartView {
            cart,
            priced,
            settings,
            repriced_lines: repriced,
        })
    }

    /// Persists the cart; audits the sensitive changes (clear, void, override, discount).
    /// `scan` is recorded in the same transaction, so a delivery counts exactly once.
    pub(crate) async fn save(
        &self,
        ctx: &CommandContext,
        cart: Cart,
        base: Option<u64>,
        audited: Option<(audit::AuditAction, serde_json::Value)>,
        scan: Option<ScanEventId>,
    ) -> Result<CartView, AppError> {
        let actor = ctx.actor()?.clone();
        // Validate the result prices before writing (e.g. a discount larger than the line).
        let view = self.view(ctx, cart.clone(), Vec::new()).await?;
        let mut b = WriteSet::builder(ctx.write_context()).put_cart(cart.clone(), actor.user_id, base);
        if let Some(id) = scan {
            b = b.record_scan(id);
        }
        if let Some((action, metadata)) = audited {
            b = b.audit(AuditEntry::new(ctx.ids.as_ref(), action, "cart", cart.id).with_metadata(metadata));
        }
        self.store.apply(b.build()?).await?;
        Ok(view)
    }

    /// Adds a variant (search / quick-select). `qty: None` uses the variant's scan unit.
    pub async fn add_variant(
        &self,
        ctx: &CommandContext,
        variant: VariantId,
        qty: Option<Quantity>,
        via: AddVia,
    ) -> Result<(CartView, CartChange), AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let sellable = self.sellable(ctx, variant).await?;
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        let qty = qty.unwrap_or(Quantity::ONE);
        let change = cart.add(sellable, qty, via, ctx.ids.as_ref()).map_err(cart_err)?;
        Ok((self.save(ctx, cart, base, None, None).await?, change))
    }

    pub async fn set_quantity(
        &self,
        ctx: &CommandContext,
        line: CartLineId,
        qty: Quantity,
    ) -> Result<CartView, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        cart.set_quantity(line, qty).map_err(cart_err)?;
        self.save(ctx, cart, base, None, None).await
    }

    pub async fn remove_line(&self, ctx: &CommandContext, line: CartLineId) -> Result<CartView, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        cart.remove_line(line).map_err(cart_err)?;
        self.save(ctx, cart, base, None, None).await
    }

    /// Line discount. Permission thresholds and manager approval arrive with E08-T02.
    pub async fn set_line_discount(
        &self,
        ctx: &CommandContext,
        line: CartLineId,
        discount: Option<Discount>,
    ) -> Result<CartView, AppError> {
        ctx.authorize(Permission::CartLineDiscount)?;
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        cart.set_line_discount(line, discount.clone()).map_err(cart_err)?;
        let meta = serde_json::json!({ "line": line, "discount": discount });
        self.save(ctx, cart, base, Some((audit::CART_DISCOUNT_APPLIED, meta)), None)
            .await
    }

    pub async fn set_order_discount(
        &self,
        ctx: &CommandContext,
        discount: Option<Discount>,
    ) -> Result<CartView, AppError> {
        ctx.authorize(Permission::CartOrderDiscount)?;
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        cart.set_order_discount(discount.clone()).map_err(cart_err)?;
        self.save(
            ctx,
            cart,
            base,
            Some((
                audit::CART_DISCOUNT_APPLIED,
                serde_json::json!({ "order": discount }),
            )),
            None,
        )
        .await
    }

    pub async fn override_price(
        &self,
        ctx: &CommandContext,
        line: CartLineId,
        price: Money,
    ) -> Result<CartView, AppError> {
        ctx.authorize(Permission::PriceOverride)?;
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        let before = cart
            .lines
            .iter()
            .find(|l| l.id == line)
            .map(|l| l.price_override.unwrap_or(l.variant.price));
        cart.override_price(line, price).map_err(cart_err)?;
        let meta = serde_json::json!({ "line": line, "fromMinor": before.map(|m| m.minor().to_string()), "toMinor": price.minor().to_string() });
        self.save(ctx, cart, base, Some((audit::CART_PRICE_OVERRIDDEN, meta)), None)
            .await
    }

    /// POS-003 with confirmation in the UI and a revision guard here.
    pub async fn clear(&self, ctx: &CommandContext, revision: u64) -> Result<CartView, AppError> {
        ctx.authorize(Permission::CartClear)?;
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        let change = cart.clear(revision).map_err(cart_err)?;
        let CartChange::Cleared { lines } = change else {
            unreachable!("clear returns Cleared")
        };
        self.save(
            ctx,
            cart,
            base,
            Some((audit::CART_CLEARED, serde_json::json!({ "lines": lines }))),
            None,
        )
        .await
    }

    /// RET-001: abandon the draft with a reason. Returns a fresh empty cart.
    pub async fn void(
        &self,
        ctx: &CommandContext,
        revision: u64,
        reason: &str,
    ) -> Result<CartView, AppError> {
        let guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        // An empty cart is simply closed; a cart with lines needs CartVoid (cashier: approval).
        if !cart.lines.is_empty() {
            ctx.authorize(Permission::CartVoid)?;
        } else {
            ctx.authorize(Permission::SaleComplete)?;
        }
        let lines = cart.lines.len();
        cart.void(revision, reason).map_err(cart_err)?;
        let meta = serde_json::json!({ "lines": lines, "reason": cart.void_reason });
        let actor = ctx.actor()?.clone();
        let set = WriteSet::builder(ctx.write_context())
            .put_cart(cart.clone(), actor.user_id, base)
            .audit(AuditEntry::new(ctx.ids.as_ref(), audit::CART_VOIDED, "cart", cart.id).with_metadata(meta))
            .build()?;
        self.store.apply(set).await?;
        drop(guard);
        self.current(ctx).await
    }
}

pub(crate) fn cart_err(e: pos_domain::cart::CartError) -> AppError {
    let code = pos_domain::HasErrorCode::error_code(&e);
    AppError::Validation(e.to_string(), code)
}
