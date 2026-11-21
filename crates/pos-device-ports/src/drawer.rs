//! Cash drawer (E20-T05). Almost every drawer is opened by a pulse from the receipt
//! printer's kick connector (RJ11/RJ12); a few have their own USB or serial trigger.

use crate::DeviceError;

/// Which kick-connector pin drives the solenoid. Pin 2 is drawer 1 on most cables;
/// pin 5 is drawer 2 (or drawer 1 on some cables).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum DrawerPin {
    #[default]
    Pin2,
    Pin5,
}

#[async_trait::async_trait]
pub trait CashDrawerPort: Send + Sync {
    /// Fires the opening pulse. `Ok` means the pulse was sent, not that the drawer moved.
    async fn open(&self, pin: DrawerPin) -> Result<(), DeviceError>;

    /// The drawer-open sensor, where the drawer and connection report it.
    async fn is_open(&self) -> Result<Option<bool>, DeviceError> {
        Ok(None)
    }
}
