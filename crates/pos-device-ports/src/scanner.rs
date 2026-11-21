//! Barcode input for scanners that are not keyboard wedges (serial RS-232, USB virtual COM,
//! Bluetooth SPP). Keyboard-wedge scans are captured in the UI (ADR-022) and reach the cart
//! through the same scan command, so both paths end in one place.

use crate::{DeviceError, DeviceHealth};
use pos_domain::catalog::Symbology;
use pos_domain::{ScanEventId, Timestamp};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanSource {
    KeyboardWedge,
    Virtual,
    /// A serial, USB-COM or Bluetooth-SPP scanner, by its port name.
    Serial(String),
}

/// One physical scan. `id` is unique per scan; a repeated *delivery* of the same scan keeps
/// its id so the cart counts it once (BAR-007).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanEvent {
    pub id: ScanEventId,
    pub source: ScanSource,
    /// Exactly what the scanner sent, without its terminator. May contain GS (FNC1).
    pub raw: String,
    pub symbology_hint: Option<Symbology>,
    pub received_at: Timestamp,
}

#[async_trait::async_trait]
pub trait BarcodeInputPort: Send + Sync {
    /// Opens the device and keeps it open, reconnecting after unplug/replug.
    async fn start(&self) -> Result<(), DeviceError>;
    async fn stop(&self) -> Result<(), DeviceError>;
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ScanEvent>;
    fn health(&self) -> tokio::sync::watch::Receiver<DeviceHealth>;
}
