//! Real hardware adapters (Stage 5, ADR-024): receipt printers, cash drawers and serial
//! barcode scanners, behind the ports in `pos-device-ports`.
//!
//! - [`printer::HwPrinter`]: ESC/POS, Star Line Mode or plain-text printers, with
//!   real-time status where the link allows, and a cash drawer on the kick connector.
//! - [`transport`]: TCP raw (port 9100), serial (RS-232, USB COM, Bluetooth SPP), device
//!   files (`/dev/usb/lp0`, `LPT1`) and the OS spooler (winspool RAW, CUPS `lp -o raw`).
//! - [`scanner::SerialScanner`]: serial barcode scanners with auto-reconnect.
//! - [`profiles`]: model profiles (columns, code pages, cutter, drawer…), partly derived
//!   from escpos-printer-db (CC BY 4.0, see `profiles/LICENSE-escpos-printer-db`).
//! - [`discovery`]: serial ports with vendor hints, device files, spooler queues, network.
//! - [`config`]: the settings the app stores, their validation, and [`config::build_printer`].
//!
//! Resilience rules: every operation has a timeout; links are opened per exchange and never
//! kept; a job that may have reached the paper is reported `Uncertain`, never `Failed`, so it
//! is never printed twice (see the outcome table in [`printer`]).

pub mod config;
pub mod discovery;
pub mod escpos;
pub mod printer;
pub mod profiles;
pub mod scanner;
pub mod transport;

pub use config::{
    FieldError, PrinterConfig, PrinterConnection, ScannerConfig, SerialSettings, Terminator, build_printer,
};
pub use printer::{AsbTimings, DrawerOpenLevel, HwPrinter, IdentifiedPrinter, PrinterTimeouts, StatusSource};
pub use profiles::{FontChoice, PrinterProfile, match_identified, profile, profiles};
pub use scanner::SerialScanner;
pub use transport::{SendOutcome, Transport, TransportError};
