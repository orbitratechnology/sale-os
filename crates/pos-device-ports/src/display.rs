//! Customer displays: the small display (often a 2×20 VFD pole display) that faces the
//! customer and shows the item just scanned, the total due and the change.
//!
//! The port takes what to show in words, not bytes: each adapter fits it to its own width
//! and character set. Showing never blocks the till: [`CustomerDisplayPort::show`] queues
//! the frame and returns, the adapter writes it on its own task, and when several frames
//! queue up only the latest is written. Problems go to [`CustomerDisplayPort::health`],
//! never to the sale.

use crate::{DeviceError, DeviceHealth};

/// One line of the display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayLine {
    /// Text from the left.
    Text(String),
    /// Text in the middle (the welcome line).
    Centered(String),
    /// A label on the left and an amount on the right; the label gives way when both do
    /// not fit.
    Pair(String, String),
}

impl DisplayLine {
    pub fn blank() -> Self {
        DisplayLine::Text(String::new())
    }
}

/// Everything on the display at once: two lines on a 2×20 pole display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayFrame {
    pub upper: DisplayLine,
    pub lower: DisplayLine,
}

impl DisplayFrame {
    pub fn new(upper: DisplayLine, lower: DisplayLine) -> Self {
        Self { upper, lower }
    }
}

#[async_trait::async_trait]
pub trait CustomerDisplayPort: Send + Sync {
    /// Which display, in words ("/dev/ttyUSB0 @ 9600, ESC/POS"), for the settings screen.
    fn describe(&self) -> String;

    /// Queues `frame` and returns at once. Only the latest queued frame is written.
    fn show(&self, frame: DisplayFrame);

    /// Writes `frame` now and says whether it reached the display (Settings → Test display).
    async fn test(&self, frame: DisplayFrame) -> Result<(), DeviceError>;

    /// How the display is doing: `Unknown` until the first write, then `Ready` or
    /// `Offline` with the reason.
    fn health(&self) -> tokio::sync::watch::Receiver<DeviceHealth>;
}
