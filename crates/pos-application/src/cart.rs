//! Draft cart use cases (E09-T01, POS-001..003, POS-011, BAR-005).

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, CatalogQueries, PricingSettings};
use crate::{LocalStore, WriteSet};
use pos_domain::cart::{AddVia, Cart, CartChange, CartLine, CartStatus};
use pos_domain::catalog::CatalogError;
use pos_domain::pricing::{Discount, PricedCart, PricingPolicy, price_cart};
use pos_domain::time::{business_date, parse_time_zone};
use pos_domain::{BusinessDate, CartLineId, Money, Permission, Quantity, VariantId};
use std::sync::Arc;

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
    pub store: Arc<dyn LocalStore>,
    pub carts: Arc<dyn CartQueries>,
    pub catalog: Arc<dyn CatalogQueries>,
}

impl CartService {
    fn today(&self, ctx: &CommandContext, settings: &PricingSettings) -> Result<BusinessDate, AppError> {
        let tz = parse_time_zone(&settings.timezone).map_err(|e| AppError::Internal(e.to_string()))?;
        business_date(ctx.now(), tz).map_err(|e| AppError::Internal(e.to_string()))
    }

    /// The open cart (re-read from storage and re-priced), or a new empty one.
    pub async fn current(&self, ctx: &CommandContext) -> Result<CartView, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let (cart, repriced) = self.load(ctx).await?;
        self.view(ctx, cart, repriced).await
    }

    async fn load(&self, ctx: &CommandContext) -> Result<(Cart, Vec<CartLineId>), AppError> {
        let settings = self.carts.pricing_settings().await?;
        let date = self.today(ctx, &settings)?;
        let Some(stored) = self.carts.open_cart(ctx.register.register_id).await? else {
            return Ok((Cart::new(ctx.ids.as_ref()), Vec::new()));
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
        Ok((cart, Vec::new()))
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
    async fn save(
        &self,
        ctx: &CommandContext,
        cart: Cart,
        audited: Option<(audit::AuditAction, serde_json::Value)>,
    ) -> Result<CartView, AppError> {
        let actor = ctx.actor()?.clone();
        // Validate the result prices before writing (e.g. a discount larger than the line).
        let view = self.view(ctx, cart.clone(), Vec::new()).await?;
        let mut b = WriteSet::builder(ctx.write_context()).put_cart(cart.clone(), actor.user_id);
        if let Some((action, metadata)) = audited {
            b = b.audit(AuditEntry::new(ctx.ids.as_ref(), action, "cart", cart.id).with_metadata(metadata));
        }
        self.store.apply(b.build()?).await?;
        Ok(view)
    }

    /// Adds by barcode or SKU (BAR-001/004/005). `None` when the code is unknown (BAR-006).
    pub async fn add_code(
        &self,
        ctx: &CommandContext,
        raw: &str,
        via: AddVia,
    ) -> Result<Option<(CartView, CartChange)>, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let Some(hit) = self.catalog.lookup_code(raw).await? else {
            return Ok(None);
        };
        let (view, change) = self
            .add_variant(ctx, hit.variant_id, Some(hit.scan_qty), via)
            .await?;
        Ok(Some((view, change)))
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
        let settings = self.carts.pricing_settings().await?;
        let date = self.today(ctx, &settings)?;
        let sellable = self
            .carts
            .sellable(variant, date)
            .await?
            .ok_or(CatalogError::NotFound)?;
        let (mut cart, _) = self.load(ctx).await?;
        let qty = qty.unwrap_or(Quantity::ONE);
        let change = cart.add(sellable, qty, via, ctx.ids.as_ref()).map_err(cart_err)?;
        Ok((self.save(ctx, cart, None).await?, change))
    }

    pub async fn set_quantity(
        &self,
        ctx: &CommandContext,
        line: CartLineId,
        qty: Quantity,
    ) -> Result<CartView, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let (mut cart, _) = self.load(ctx).await?;
        cart.set_quantity(line, qty).map_err(cart_err)?;
        self.save(ctx, cart, None).await
    }

    pub async fn remove_line(&self, ctx: &CommandContext, line: CartLineId) -> Result<CartView, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let (mut cart, _) = self.load(ctx).await?;
        cart.remove_line(line).map_err(cart_err)?;
        self.save(ctx, cart, None).await
    }

    /// Line discount. Permission thresholds and manager approval arrive with E08-T02.
    pub async fn set_line_discount(
        &self,
        ctx: &CommandContext,
        line: CartLineId,
        discount: Option<Discount>,
    ) -> Result<CartView, AppError> {
        ctx.authorize(Permission::CartLineDiscount)?;
        let (mut cart, _) = self.load(ctx).await?;
        cart.set_line_discount(line, discount.clone()).map_err(cart_err)?;
        let meta = serde_json::json!({ "line": line, "discount": discount });
        self.save(ctx, cart, Some((audit::CART_DISCOUNT_APPLIED, meta)))
            .await
    }

    pub async fn set_order_discount(
        &self,
        ctx: &CommandContext,
        discount: Option<Discount>,
    ) -> Result<CartView, AppError> {
        ctx.authorize(Permission::CartOrderDiscount)?;
        let (mut cart, _) = self.load(ctx).await?;
        cart.set_order_discount(discount.clone()).map_err(cart_err)?;
        self.save(
            ctx,
            cart,
            Some((
                audit::CART_DISCOUNT_APPLIED,
                serde_json::json!({ "order": discount }),
            )),
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
        let (mut cart, _) = self.load(ctx).await?;
        let before = cart
            .lines
            .iter()
            .find(|l| l.id == line)
            .map(|l| l.price_override.unwrap_or(l.variant.price));
        cart.override_price(line, price).map_err(cart_err)?;
        let meta = serde_json::json!({ "line": line, "fromMinor": before.map(|m| m.minor().to_string()), "toMinor": price.minor().to_string() });
        self.save(ctx, cart, Some((audit::CART_PRICE_OVERRIDDEN, meta)))
            .await
    }

    /// POS-003 with confirmation in the UI and a revision guard here.
    pub async fn clear(&self, ctx: &CommandContext, revision: u64) -> Result<CartView, AppError> {
        ctx.authorize(Permission::CartClear)?;
        let (mut cart, _) = self.load(ctx).await?;
        let change = cart.clear(revision).map_err(cart_err)?;
        let CartChange::Cleared { lines } = change else {
            unreachable!("clear returns Cleared")
        };
        self.save(
            ctx,
            cart,
            Some((audit::CART_CLEARED, serde_json::json!({ "lines": lines }))),
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
        let (mut cart, _) = self.load(ctx).await?;
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
            .put_cart(cart.clone(), actor.user_id)
            .audit(AuditEntry::new(ctx.ids.as_ref(), audit::CART_VOIDED, "cart", cart.id).with_metadata(meta))
            .build()?;
        self.store.apply(set).await?;
        self.current(ctx).await
    }
}

fn cart_err(e: pos_domain::cart::CartError) -> AppError {
    let code = pos_domain::HasErrorCode::error_code(&e);
    AppError::Validation(e.to_string(), code)
}
