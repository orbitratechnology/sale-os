//! Weighing at the till (E20-T06, CAT-007): items sold by weight (unit `kg` or `g`) take
//! their quantity from the scale.
//!
//! Rules:
//!
//! - only a **stable** reading is sold: the till asks until the scale settles, for at most
//!   [`SETTLE_TIMEOUT`], and refuses a weight still moving after that;
//! - zero, negative, over- and under-range readings are refused, each with its own reason
//!   ([`WeighRefusal`]), and nothing is added;
//! - the weight becomes the line's quantity exactly: grams are the milli-units of a `kg`
//!   item, milligrams those of a `g` item. A `kg` item weighed finer than a gram is rounded
//!   to the gram half away from zero, like every amount in ADR-004;
//! - a weighed line is a line of its own (never merged into an earlier one of the same item);
//!   its quantity can still be typed over, which is also the fallback without a scale.
//!
//! The price follows from the quantity through the pricing engine as for any line
//! (`gross = unit price × quantity`, rounded per ADR-004).

use crate::cart::{CartService, CartView, Loaded, cart_err};
use crate::context::{AppError, CommandContext};
use pos_device_ports::{ScalePort, ScaleReading, Weight};
use pos_domain::cart::{AddVia, CartChange};
use pos_domain::catalog::Unit;
use pos_domain::{CartLineId, Permission, Quantity, VariantId};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// How long the till waits for the scale to settle.
pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(3);
/// How often it asks meanwhile.
pub const SETTLE_POLL: Duration = Duration::from_millis(150);

/// Why a weight was not used. The cashier sees the reason; nothing was added or changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WeighRefusal {
    /// No scale is set up on this till.
    NoScale,
    /// Still moving after the settle time.
    Unstable,
    /// Nothing on the scale.
    Zero,
    /// Below zero: the scale needs zeroing (or the tare is wrong).
    Negative,
    Overload,
    Underload,
    /// The scale could not be read: unplugged, no answer, unreadable answer.
    Failed(String),
}

impl WeighRefusal {
    /// A stable code for the UI: `no_scale`, `unstable`, `zero`, `negative`, `overload`,
    /// `underload` or `failed`.
    pub fn code(&self) -> &'static str {
        match self {
            WeighRefusal::NoScale => "no_scale",
            WeighRefusal::Unstable => "unstable",
            WeighRefusal::Zero => "zero",
            WeighRefusal::Negative => "negative",
            WeighRefusal::Overload => "overload",
            WeighRefusal::Underload => "underload",
            WeighRefusal::Failed(_) => "failed",
        }
    }

    /// What to tell the cashier (the UI translates by [`WeighRefusal::code`]).
    pub fn message(&self) -> String {
        match self {
            WeighRefusal::NoScale => "no scale is set up on this till".into(),
            WeighRefusal::Unstable => "the weight did not settle; hold the item still and weigh again".into(),
            WeighRefusal::Zero => "nothing is on the scale".into(),
            WeighRefusal::Negative => "the scale reads below zero; zero it and weigh again".into(),
            WeighRefusal::Overload => "too heavy for the scale".into(),
            WeighRefusal::Underload => "the scale reads below its range; zero it and weigh again".into(),
            WeighRefusal::Failed(why) => format!("the scale could not be read: {why}"),
        }
    }
}

/// Waits for a stable reading and checks it can be sold.
pub async fn weigh_stable(
    port: &dyn ScalePort,
    settle: Duration,
    poll: Duration,
) -> Result<Weight, WeighRefusal> {
    let deadline = tokio::time::Instant::now() + settle;
    loop {
        match port.read().await {
            Ok(ScaleReading::Stable(w)) if w.is_negative() => return Err(WeighRefusal::Negative),
            Ok(ScaleReading::Stable(w)) if !w.is_positive() => return Err(WeighRefusal::Zero),
            Ok(ScaleReading::Stable(w)) => return Ok(w),
            Ok(ScaleReading::Overload) => return Err(WeighRefusal::Overload),
            Ok(ScaleReading::Underload) => return Err(WeighRefusal::Underload),
            Ok(ScaleReading::Moving(_)) => {
                if tokio::time::Instant::now() + poll > deadline {
                    return Err(WeighRefusal::Unstable);
                }
                tokio::time::sleep(poll).await;
            }
            Err(e) => return Err(WeighRefusal::Failed(e.to_string())),
        }
    }
}

/// Whether items in `unit` are sold by weight (and so can be weighed).
pub const fn sold_by_weight(unit: Unit) -> bool {
    matches!(unit, Unit::Kg | Unit::G)
}

/// The quantity `weight` is for an item sold in `unit`: exact for `g` items; for `kg` items
/// to the gram, half away from zero (ADR-004). `None` for an item not sold by weight.
pub fn quantity_for(weight: Weight, unit: Unit) -> Option<Quantity> {
    let mg = weight.milligrams();
    match unit {
        // 1 kg = 1,000 milli-units: one milli-unit is a gram.
        Unit::Kg => {
            let (g, rest) = (mg / 1000, mg % 1000);
            let g = if rest.abs() >= 500 { g + rest.signum() } else { g };
            Some(Quantity::from_milli(g))
        }
        // 1 g = 1,000 milli-units: one milli-unit is a milligram.
        Unit::G => Some(Quantity::from_milli(mg)),
        _ => None,
    }
}

/// The till's scale, swapped when its settings change.
#[derive(Default)]
pub struct Scale {
    port: RwLock<Option<Arc<dyn ScalePort>>>,
    /// The stored settings `port` was built from, so an unchanged scale is kept.
    config: RwLock<Option<serde_json::Value>>,
}

impl Scale {
    /// The scale in use, if any.
    pub fn port(&self) -> Option<Arc<dyn ScalePort>> {
        self.port
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn is_configured(&self) -> bool {
        self.port().is_some()
    }

    pub fn set(&self, port: Option<Arc<dyn ScalePort>>) {
        self.set_built_from(port, None);
    }

    pub fn set_built_from(&self, port: Option<Arc<dyn ScalePort>>, config: Option<serde_json::Value>) {
        *self
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config;
        *self
            .port
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = port;
    }

    pub fn built_from(&self) -> Option<serde_json::Value> {
        self.config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// A stable, sellable weight from the scale.
    pub async fn weigh(&self) -> Result<Weight, WeighRefusal> {
        let port = self.port().ok_or(WeighRefusal::NoScale)?;
        weigh_stable(port.as_ref(), SETTLE_TIMEOUT, SETTLE_POLL).await
    }
}

/// What a weighing did: the cart with the weighed line, or why nothing changed.
#[derive(Clone, Debug, PartialEq)]
pub enum Weighed {
    Done {
        view: Box<CartView>,
        line: CartLineId,
        weight: Weight,
    },
    Refused(WeighRefusal),
}

fn not_by_weight(unit: Unit) -> AppError {
    AppError::Validation(
        format!("this item is sold by {}, not by weight", unit.code()),
        pos_domain::ErrorCode::ValidationFailed,
    )
}

impl CartService {
    /// Uses `scale` for weighed items, and scans of items sold by weight ask for a weighing
    /// while it is set up (E20-T06).
    pub fn with_scale(mut self, scale: Arc<Scale>) -> Self {
        self.scale = Some(scale);
        self
    }

    /// Whether a scale is set up, so items sold by weight are weighed.
    pub fn weighs(&self) -> bool {
        self.scale.as_ref().is_some_and(|s| s.is_configured())
    }

    async fn weigh_now(&self) -> Result<Weight, WeighRefusal> {
        match &self.scale {
            Some(s) => s.weigh().await,
            None => Err(WeighRefusal::NoScale),
        }
    }

    /// Weighs an item sold by weight and adds it as a line of its own with the weight as
    /// its quantity. Nothing is added when the weight is refused. The weighing happens
    /// before the cart is locked, so a slow scale never holds up other cart commands.
    pub async fn add_weighed(
        &self,
        ctx: &CommandContext,
        variant: VariantId,
        via: AddVia,
    ) -> Result<Weighed, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let sellable = self.sellable(ctx, variant).await?;
        if !sold_by_weight(sellable.unit) {
            return Err(not_by_weight(sellable.unit));
        }
        let weight = match self.weigh_now().await {
            Ok(w) => w,
            Err(r) => return Ok(Weighed::Refused(r)),
        };
        self.add_weight(ctx, variant, weight, via).await
    }

    /// Adds `weight` of an item sold by weight as a line of its own (the scale's reading,
    /// already checked stable and positive).
    pub async fn add_weight(
        &self,
        ctx: &CommandContext,
        variant: VariantId,
        weight: Weight,
        via: AddVia,
    ) -> Result<Weighed, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let sellable = self.sellable(ctx, variant).await?;
        let qty = quantity_for(weight, sellable.unit).ok_or_else(|| not_by_weight(sellable.unit))?;
        if !qty.is_positive() {
            return Ok(Weighed::Refused(WeighRefusal::Zero));
        }
        let _guard = self.exclusive().await;
        let Loaded { mut cart, base, .. } = self.load(ctx).await?;
        let change = cart
            .add_separate(sellable, qty, via, ctx.ids.as_ref())
            .map_err(cart_err)?;
        let CartChange::LineAdded(line) = change else {
            return Err(AppError::Validation(
                "the weighed line was not added".into(),
                pos_domain::ErrorCode::ValidationFailed,
            ));
        };
        let view = Box::new(self.save(ctx, cart, base, None, None).await?);
        Ok(Weighed::Done { view, line, weight })
    }

    /// The fallback: a weight typed in instead of weighed (as the quantity, thousandths of
    /// the item's unit). Like a weighing it makes a line of its own.
    pub async fn add_typed_weight(
        &self,
        ctx: &CommandContext,
        variant: VariantId,
        qty: Quantity,
        via: AddVia,
    ) -> Result<Weighed, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let sellable = self.sellable(ctx, variant).await?;
        let weight = match sellable.unit {
            Unit::Kg => qty.milli().checked_mul(1000),
            Unit::G => Some(qty.milli()),
            other => return Err(not_by_weight(other)),
        }
        .map(Weight::from_milligrams)
        .ok_or_else(|| {
            AppError::Validation(
                "that weight is out of range".into(),
                pos_domain::ErrorCode::ValidationFailed,
            )
        })?;
        if !qty.is_positive() {
            return Err(AppError::Validation(
                "enter a weight above zero".into(),
                pos_domain::ErrorCode::ValidationFailed,
            ));
        }
        self.add_weight(ctx, variant, weight, via).await
    }

    /// Weighs again for an existing line of an item sold by weight and sets its quantity
    /// (the "Weigh" key on the selected line).
    pub async fn weigh_line(&self, ctx: &CommandContext, line: CartLineId) -> Result<Weighed, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let unit = {
            let Loaded { cart, .. } = self.load(ctx).await?;
            let l = cart
                .lines
                .iter()
                .find(|l| l.id == line)
                .ok_or_else(|| cart_err(pos_domain::cart::CartError::LineNotFound))?;
            l.variant.unit
        };
        if !sold_by_weight(unit) {
            return Err(not_by_weight(unit));
        }
        let weight = match self.weigh_now().await {
            Ok(w) => w,
            Err(r) => return Ok(Weighed::Refused(r)),
        };
        let qty = quantity_for(weight, unit).ok_or_else(|| not_by_weight(unit))?;
        if !qty.is_positive() {
            return Ok(Weighed::Refused(WeighRefusal::Zero));
        }
        let view = Box::new(self.set_quantity(ctx, line, qty).await?);
        Ok(Weighed::Done { view, line, weight })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_device_ports::{DeviceError, DeviceHealth};
    use std::sync::Mutex;

    struct Scripted(Mutex<Vec<Result<ScaleReading, DeviceError>>>);

    #[async_trait::async_trait]
    impl ScalePort for Scripted {
        fn describe(&self) -> String {
            "test".into()
        }
        async fn read(&self) -> Result<ScaleReading, DeviceError> {
            let mut q = self.0.lock().unwrap();
            if q.len() > 1 { q.remove(0) } else { q[0].clone() }
        }
        fn health(&self) -> tokio::sync::watch::Receiver<DeviceHealth> {
            tokio::sync::watch::channel(DeviceHealth::Ready).1
        }
    }

    fn mg(n: i64) -> Weight {
        Weight::from_milligrams(n)
    }

    async fn weigh(script: Vec<Result<ScaleReading, DeviceError>>) -> Result<Weight, WeighRefusal> {
        let s = Scripted(Mutex::new(script));
        weigh_stable(&s, Duration::from_millis(100), Duration::from_millis(10)).await
    }

    #[tokio::test]
    async fn only_a_stable_positive_weight_is_used() {
        use ScaleReading::*;
        assert_eq!(weigh(vec![Ok(Stable(mg(1_234_000)))]).await, Ok(mg(1_234_000)));
        // Settles after moving.
        assert_eq!(
            weigh(vec![
                Ok(Moving(Some(mg(900_000)))),
                Ok(Moving(None)),
                Ok(Stable(mg(1_000_000)))
            ])
            .await,
            Ok(mg(1_000_000))
        );
        assert_eq!(
            weigh(vec![Ok(Moving(Some(mg(5))))]).await,
            Err(WeighRefusal::Unstable)
        );
        assert_eq!(
            weigh(vec![Ok(Stable(Weight::ZERO))]).await,
            Err(WeighRefusal::Zero)
        );
        assert_eq!(
            weigh(vec![Ok(Stable(mg(-12_000)))]).await,
            Err(WeighRefusal::Negative)
        );
        assert_eq!(weigh(vec![Ok(Overload)]).await, Err(WeighRefusal::Overload));
        assert_eq!(weigh(vec![Ok(Underload)]).await, Err(WeighRefusal::Underload));
        assert_eq!(
            weigh(vec![Err(DeviceError::Timeout)]).await,
            Err(WeighRefusal::Failed("the device did not answer in time".into()))
        );
    }

    #[test]
    fn weights_become_exact_quantities() {
        assert_eq!(
            quantity_for(mg(1_234_000), Unit::Kg),
            Some(Quantity::from_milli(1_234))
        );
        assert_eq!(
            quantity_for(mg(1_234_000), Unit::G),
            Some(Quantity::from_milli(1_234_000))
        );
        assert_eq!(quantity_for(mg(500), Unit::G), Some(Quantity::from_milli(500)));
        // Finer than a gram on a kg item: to the gram, half away from zero.
        assert_eq!(
            quantity_for(mg(1_234_499), Unit::Kg),
            Some(Quantity::from_milli(1_234))
        );
        assert_eq!(
            quantity_for(mg(1_234_500), Unit::Kg),
            Some(Quantity::from_milli(1_235))
        );
        assert_eq!(quantity_for(mg(400), Unit::Kg), Some(Quantity::from_milli(0)));
        assert_eq!(quantity_for(mg(1_000), Unit::Ea), None);
        assert_eq!(quantity_for(mg(1_000), Unit::L), None);
    }
}
