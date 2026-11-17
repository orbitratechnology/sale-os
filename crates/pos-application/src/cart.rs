//! Draft cart use cases (E09-T01, POS-001..003, POS-011, BAR-005).

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, CatalogQueries, PricingSettings};
use crate::{LocalStore, WriteSet};
use pos_domain::cart::{AddVia, Cart, CartChange, CartLine, CartStatus, SellableVariant};
use pos_domain::catalog::CatalogError;
use pos_domain::pricing::{Discount, PricedCart, PricedLine, PricingPolicy, price_cart};
use pos_domain::time::{business_date, parse_time_zone};
use pos_domain::{BusinessDate, CartLineId, Money, Permission, Quantity, ScanEventId, UserId, VariantId};
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
    /// E11-T04: per stock-tracked variant whose quantity in the cart is more than the
    /// sellable stock on hand, that on-hand quantity ("Only 2 in stock"). A hint only: the
    /// negative-stock policy decides at completion.
    pub short_stock: std::collections::BTreeMap<pos_domain::VariantId, Quantity>,
}

pub struct CartService {
    store: Arc<dyn LocalStore>,
    carts: Arc<dyn CartQueries>,
    pub(crate) catalog: Arc<dyn CatalogQueries>,
    /// Stock on hand for the "only N in stock" hint; none means no hints.
    stock: Option<Arc<dyn crate::queries::StockQueries>>,
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
            stock: None,
            lock: Mutex::new(()),
        }
    }

    /// Shows "only N in stock" hints on lines that exceed the stock on hand (E11-T04).
    pub fn with_stock(mut self, stock: Arc<dyn crate::queries::StockQueries>) -> Self {
        self.stock = Some(stock);
        self
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

    pub(crate) fn today(
        &self,
        ctx: &CommandContext,
        settings: &PricingSettings,
    ) -> Result<BusinessDate, AppError> {
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

    pub(crate) async fn view(
        &self,
        ctx: &CommandContext,
        cart: Cart,
        repriced: Vec<CartLineId>,
    ) -> Result<CartView, AppError> {
        let settings = self.carts.pricing_settings().await?;
        let policy = PricingPolicy::new(settings.tax_mode);
        let priced = price_cart(&cart.pricing_lines(), cart.order_discount.as_ref(), &policy)
            .map_err(|e| AppError::Validation(e.to_string(), pos_domain::HasErrorCode::error_code(&e)))?;
        let mut short_stock = std::collections::BTreeMap::new();
        if let Some(stock) = &self.stock {
            // The cart's total per tracked variant (one item may be on several lines).
            let mut wanted: std::collections::BTreeMap<pos_domain::VariantId, i64> =
                std::collections::BTreeMap::new();
            for l in cart.lines.iter().filter(|l| l.variant.track_stock) {
                *wanted.entry(l.variant.variant_id).or_default() += l.quantity.milli();
            }
            if !wanted.is_empty() {
                let variants: Vec<_> = wanted.keys().copied().collect();
                let on_hand = stock.on_hand(ctx.register.location_id, &variants).await?;
                for (v, qty) in wanted {
                    let have = on_hand.get(&v).copied().unwrap_or(Quantity::ZERO);
                    if qty > have.milli() {
                        short_stock.insert(v, have);
                    }
                }
            }
        }
        Ok(CartView {
            cart,
            priced,
            settings,
            repriced_lines: repriced,
            short_stock,
        })
    }

    /// Persists the cart; audits the sensitive changes (clear, void, override, discount).
    /// `scan` is recorded in the same transaction, so a delivery counts exactly once.
    pub(crate) async fn save(
        &self,
        ctx: &CommandContext,
        cart: Cart,
        base: Option<u64>,
        audited: Option<Audited>,
        scan: Option<ScanEventId>,
    ) -> Result<CartView, AppError> {
        let actor = ctx.actor()?.clone();
        // Validate the result prices before writing (e.g. a discount larger than the line).
        let view = self.view(ctx, cart.clone(), Vec::new()).await?;
        let mut b = WriteSet::builder(ctx.write_context()).put_cart(cart.clone(), actor.user_id, base);
        if let Some(id) = scan {
            b = b.record_scan(id);
        }
        if let Some(a) = audited {
            let mut entry =
                AuditEntry::new(ctx.ids.as_ref(), a.action, "cart", cart.id).with_metadata(a.metadata);
            if let Some(approver) = a.approver {
                entry = entry.with_approver(approver);
            }
            b = b.audit(entry);
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
    /// A line discount (POS-004, E08-T02). Above the cashier limit it needs a reason and
    /// `DiscountAboveThreshold`: a cashier gets `APPROVAL_REQUIRED` for exactly that rate.
    /// The limit applies to the effective rate, so a fixed amount cannot get round it.
    pub async fn set_line_discount(
        &self,
        ctx: &CommandContext,
        line: CartLineId,
        discount: Option<Discount>,
    ) -> Result<CartView, AppError> {
        ctx.authorize(Permission::CartLineDiscount)?;
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        let before = self.view(ctx, cart.clone(), Vec::new()).await?;
        cart.set_line_discount(line, discount.clone()).map_err(cart_err)?;
        let after = self.view(ctx, cart.clone(), Vec::new()).await?;
        let (old, new) = (line_priced(&before, line)?, line_priced(&after, line)?);
        let rate = effective_ppm(new.line_discount, new.gross);
        let approver = discount_approval(ctx, &after, rate, discount.as_ref())?;
        if let Some(d) = cart
            .lines
            .iter_mut()
            .find(|l| l.id == line)
            .and_then(|l| l.line_discount.as_mut())
        {
            d.authorized_by = approver;
        }
        let metadata = serde_json::json!({
            "line": line,
            "discount": discount,
            "effectivePpm": rate,
            "beforeMinor": old.total.minor().to_string(),
            "afterMinor": new.total.minor().to_string(),
        });
        self.save(
            ctx,
            cart,
            base,
            Some(Audited::new(audit::CART_DISCOUNT_APPLIED, metadata, approver)),
            None,
        )
        .await
    }

    /// An order discount, allocated across lines by Rust (ADR-004). Same limit and
    /// approval rule as line discounts, on the rate of the order after line discounts.
    pub async fn set_order_discount(
        &self,
        ctx: &CommandContext,
        discount: Option<Discount>,
    ) -> Result<CartView, AppError> {
        ctx.authorize(Permission::CartOrderDiscount)?;
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        let before = self.view(ctx, cart.clone(), Vec::new()).await?;
        cart.set_order_discount(discount.clone()).map_err(cart_err)?;
        let after = self.view(ctx, cart.clone(), Vec::new()).await?;
        let p = &after.priced;
        let base_amount = p
            .subtotal
            .checked_sub(p.line_discount_total)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let rate = effective_ppm(p.order_discount_total, base_amount);
        let approver = discount_approval(ctx, &after, rate, discount.as_ref())?;
        if let Some(d) = cart.order_discount.as_mut() {
            d.authorized_by = approver;
        }
        let metadata = serde_json::json!({
            "order": discount,
            "effectivePpm": rate,
            "beforeMinor": before.priced.total.minor().to_string(),
            "afterMinor": after.priced.total.minor().to_string(),
        });
        self.save(
            ctx,
            cart,
            base,
            Some(Audited::new(audit::CART_DISCOUNT_APPLIED, metadata, approver)),
            None,
        )
        .await
    }

    /// Sets the unit price of a line. Needs `PriceOverride` (a cashier: approval) and a reason.
    pub async fn override_price(
        &self,
        ctx: &CommandContext,
        line: CartLineId,
        price: Money,
        reason: &str,
    ) -> Result<CartView, AppError> {
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base } = self.load(ctx).await?;
        let approver = ctx.authorize_with_approval(Permission::PriceOverride, cart.id, None)?;
        let reason = required_reason(reason, "a price change")?;
        let before = cart
            .lines
            .iter()
            .find(|l| l.id == line)
            .map(|l| l.price_override.unwrap_or(l.variant.price));
        cart.override_price(line, price).map_err(cart_err)?;
        let metadata = serde_json::json!({
            "line": line,
            "fromMinor": before.map(|m| m.minor().to_string()),
            "toMinor": price.minor().to_string(),
            "reason": reason,
        });
        self.save(
            ctx,
            cart,
            base,
            Some(Audited::new(audit::CART_PRICE_OVERRIDDEN, metadata, approver)),
            None,
        )
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
        let metadata = serde_json::json!({ "lines": lines });
        self.save(
            ctx,
            cart,
            base,
            Some(Audited::new(audit::CART_CLEARED, metadata, None)),
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
        let approver = if cart.lines.is_empty() {
            ctx.authorize(Permission::SaleComplete)?;
            None
        } else {
            ctx.authorize_with_approval(Permission::CartVoid, cart.id, None)?
        };
        let lines = cart.lines.len();
        cart.void(revision, reason).map_err(cart_err)?;
        let meta = serde_json::json!({ "lines": lines, "reason": cart.void_reason });
        let actor = ctx.actor()?.clone();
        let mut entry =
            AuditEntry::new(ctx.ids.as_ref(), audit::CART_VOIDED, "cart", cart.id).with_metadata(meta);
        if let Some(a) = approver {
            entry = entry.with_approver(a);
        }
        let set = WriteSet::builder(ctx.write_context())
            .put_cart(cart.clone(), actor.user_id, base)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        drop(guard);
        self.current(ctx).await
    }
}

/// An audit entry for a cart change.
pub(crate) struct Audited {
    action: audit::AuditAction,
    metadata: serde_json::Value,
    approver: Option<UserId>,
}

impl Audited {
    fn new(action: audit::AuditAction, metadata: serde_json::Value, approver: Option<UserId>) -> Self {
        Self {
            action,
            metadata,
            approver,
        }
    }
}

fn line_priced(view: &CartView, line: CartLineId) -> Result<&PricedLine, AppError> {
    view.priced
        .lines
        .iter()
        .find(|l| l.line_id == line)
        .ok_or_else(|| cart_err(pos_domain::cart::CartError::LineNotFound))
}

/// `discount / base` in parts per million, rounded up so a limit cannot be slipped under
/// by rounding. Integer arithmetic only (ADR-001).
pub fn effective_ppm(discount: Money, base: Money) -> u32 {
    let (d, b) = (i128::from(discount.minor()), i128::from(base.minor()));
    if d <= 0 || b <= 0 {
        return 0;
    }
    let ppm = (d * 1_000_000 + b - 1) / b;
    u32::try_from(ppm.min(1_000_000)).unwrap_or(1_000_000)
}

/// E08-T02: above the cashier limit, a reason is required and the actor needs
/// `DiscountAboveThreshold` (or an approval from someone who has it).
fn discount_approval(
    ctx: &CommandContext,
    view: &CartView,
    rate: u32,
    discount: Option<&Discount>,
) -> Result<Option<UserId>, AppError> {
    if rate <= view.settings.discount_limit_ppm {
        return Ok(None);
    }
    required_reason(
        discount.and_then(|d| d.reason.as_deref()).unwrap_or(""),
        "a discount above the limit",
    )?;
    ctx.authorize_with_approval(Permission::DiscountAboveThreshold, view.cart.id, Some(rate))
}

fn required_reason<'a>(reason: &'a str, what: &str) -> Result<&'a str, AppError> {
    let r = reason.trim();
    if r.is_empty() {
        return Err(AppError::Validation(
            format!("{what} needs a reason"),
            pos_domain::ErrorCode::ValidationFailed,
        ));
    }
    Ok(r)
}

pub(crate) fn cart_err(e: pos_domain::cart::CartError) -> AppError {
    let code = pos_domain::HasErrorCode::error_code(&e);
    AppError::Validation(e.to_string(), code)
}
