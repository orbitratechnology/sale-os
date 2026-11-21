//! What the customer display shows, and when (the display faces the customer, often a 2×20
//! pole display):
//!
//! - an item added or changed: its name and line amount above, the running total below;
//! - at payment: the total due;
//! - after the sale: what was paid and the change, then the welcome line once the till
//!   has been idle for a while;
//! - idle: "WELCOME" and the shop's name.
//!
//! Nothing here waits for the device: [`CustomerDisplayPort::show`] queues a frame and
//! returns, so a slow, unplugged or missing display never slows down scanning or checkout,
//! and its problems are its own status, never a sale's error. Every call is a no-op while
//! no display is set up.

use crate::cart::CartView;
use pos_device_ports::{CustomerDisplayPort, DeviceError, DeviceHealth, DisplayFrame, DisplayLine};
use pos_domain::money::NumberFormat;
use pos_domain::sale::SaleSnapshot;
use pos_domain::{CartLineId, Currency, Money};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// How long the change stays up before the welcome line returns.
pub const IDLE_AFTER: Duration = Duration::from_secs(30);

fn money(currency: Currency, m: Money) -> String {
    currency.format(m, NumberFormat::EN_US)
}

/// "WELCOME" above the shop's name.
pub fn welcome_frame(shop: &str) -> DisplayFrame {
    DisplayFrame::new(
        DisplayLine::Centered("WELCOME".into()),
        DisplayLine::Centered(shop.trim().to_owned()),
    )
}

/// The line just added or changed (or else the last line), with the running total. `None`
/// for an empty cart.
pub fn cart_frame(view: &CartView, line: Option<CartLineId>) -> Option<DisplayFrame> {
    let lines = &view.cart.lines;
    let line = line
        .and_then(|id| lines.iter().find(|l| l.id == id))
        .or_else(|| lines.last())?;
    let amount = view
        .priced
        .lines
        .iter()
        .find(|p| p.line_id == line.id)
        .map(|p| p.total)?;
    let v = &line.variant;
    let name = if v.label.trim().is_empty() {
        v.name.clone()
    } else {
        format!("{} {}", v.name, v.label)
    };
    let currency = view.settings.currency;
    Some(DisplayFrame::new(
        DisplayLine::Pair(name, money(currency, amount)),
        DisplayLine::Pair("TOTAL".into(), money(currency, view.priced.total)),
    ))
}

/// At payment: the total due.
pub fn total_due_frame(view: &CartView) -> DisplayFrame {
    DisplayFrame::new(
        DisplayLine::Text("TOTAL DUE".into()),
        DisplayLine::Pair(String::new(), money(view.settings.currency, view.priced.total)),
    )
}

/// After the sale: what was paid and the change, or "THANK YOU" when there is none.
pub fn sale_done_frame(sale: &SaleSnapshot) -> DisplayFrame {
    let c = sale.currency;
    if sale.change.minor() > 0 {
        DisplayFrame::new(
            DisplayLine::Pair("PAID".into(), money(c, sale.tendered_total)),
            DisplayLine::Pair("CHANGE".into(), money(c, sale.change)),
        )
    } else {
        DisplayFrame::new(
            DisplayLine::Pair("TOTAL".into(), money(c, sale.total)),
            DisplayLine::Centered("THANK YOU".into()),
        )
    }
}

/// The test pattern of Settings → Devices: a title and a 20-column ruler.
pub fn test_frame() -> DisplayFrame {
    DisplayFrame::new(
        DisplayLine::Centered("SALE OS DISPLAY TEST".into()),
        DisplayLine::Text("12345678901234567890".into()),
    )
}

/// The till's customer display, swapped when its settings change.
pub struct CustomerDisplay {
    port: RwLock<Option<Arc<dyn CustomerDisplayPort>>>,
    /// The stored settings `port` was built from, so an unchanged display is kept.
    config: RwLock<Option<serde_json::Value>>,
    shop: RwLock<String>,
    /// Bumped by every frame, so a pending return to the welcome line knows when something
    /// newer was shown meanwhile.
    shown: Arc<AtomicU64>,
    idle_after: Duration,
}

impl Default for CustomerDisplay {
    fn default() -> Self {
        Self::new(IDLE_AFTER)
    }
}

impl CustomerDisplay {
    pub fn new(idle_after: Duration) -> Self {
        Self {
            port: RwLock::new(None),
            config: RwLock::new(None),
            shop: RwLock::new(String::new()),
            shown: Arc::new(AtomicU64::new(0)),
            idle_after,
        }
    }

    /// The display in use, if any.
    pub fn port(&self) -> Option<Arc<dyn CustomerDisplayPort>> {
        self.port
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Uses `port` from now on (or none), starting with the welcome line.
    pub fn set(&self, port: Option<Arc<dyn CustomerDisplayPort>>) {
        self.set_built_from(port, None);
    }

    /// As [`CustomerDisplay::set`], remembering the settings `port` was built from.
    pub fn set_built_from(
        &self,
        port: Option<Arc<dyn CustomerDisplayPort>>,
        config: Option<serde_json::Value>,
    ) {
        *self
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config;
        *self
            .port
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = port;
        self.show_welcome();
    }

    /// The settings the display in use was built from.
    pub fn built_from(&self) -> Option<serde_json::Value> {
        self.config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The shop's name, for the welcome line.
    pub fn set_shop(&self, name: &str) {
        *self
            .shop
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = name.to_owned();
    }

    /// How the display is doing; `None` when none is set up.
    pub fn health(&self) -> Option<DeviceHealth> {
        self.port().map(|p| p.health().borrow().clone())
    }

    fn show(&self, frame: DisplayFrame) -> u64 {
        let n = self.shown.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(port) = self.port() {
            port.show(frame);
        }
        n
    }

    /// Back to the welcome line after the idle time, unless something else is shown first.
    fn welcome_later(self: &Arc<Self>, shown: u64) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let me = Arc::downgrade(self);
        let after = self.idle_after;
        runtime.spawn(async move {
            tokio::time::sleep(after).await;
            if let Some(me) = me.upgrade()
                && me.shown.load(Ordering::SeqCst) == shown
            {
                me.show_welcome();
            }
        });
    }

    pub fn show_welcome(&self) {
        let shop = self
            .shop
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.show(welcome_frame(&shop));
    }

    /// The cart changed: the line `line` (or the last one) and the total; the welcome
    /// line when the cart is empty.
    pub fn show_cart(&self, view: &CartView, line: Option<CartLineId>) {
        match cart_frame(view, line) {
            Some(frame) => {
                self.show(frame);
            }
            None => self.show_welcome(),
        }
    }

    pub fn show_total_due(&self, view: &CartView) {
        self.show(total_due_frame(view));
    }

    /// The sale is done: the change, then the welcome line after the idle time.
    pub fn show_sale_done(self: &Arc<Self>, sale: &SaleSnapshot) {
        let shown = self.show(sale_done_frame(sale));
        self.welcome_later(shown);
    }

    /// Writes the test pattern now and says whether it reached the display; the welcome
    /// line returns after the idle time.
    pub async fn test(self: &Arc<Self>) -> Result<String, DeviceError> {
        let port = self.port().ok_or(DeviceError::NotConfigured)?;
        let shown = self.shown.fetch_add(1, Ordering::SeqCst) + 1;
        port.test(test_frame()).await?;
        self.welcome_later(shown);
        Ok(port.describe())
    }
}
