//! Receipt printers (E13-T06, E20-T03/T04, ADR-023).

use crate::DeviceError;

/// Why a printer did not print. Each maps to an actionable message in the UI. A failure
/// means **nothing was printed**, so trying again is safe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintFailure {
    PaperOut,
    CoverOpen,
    /// Unreachable: unplugged, switched off, wrong address, port in use.
    Disconnected,
    /// Reachable but reports itself offline (error state, feed button held, cutter jam).
    Offline,
    Rejected(String),
}

impl PrintFailure {
    /// Stored in `print_jobs.last_error_code`.
    pub fn code(&self) -> &'static str {
        match self {
            PrintFailure::PaperOut => "paper_out",
            PrintFailure::CoverOpen => "cover_open",
            PrintFailure::Disconnected => "disconnected",
            PrintFailure::Offline => "offline",
            PrintFailure::Rejected(_) => "rejected",
        }
    }
}

/// What a printer reported for one submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintResult {
    /// The printer accepted and printed the job.
    Printed,
    /// The printer refused it: nothing was printed, so trying again is safe.
    Failed(PrintFailure),
    /// Some or all of the job may have reached the paper, but the printer did not confirm
    /// (the connection broke mid-job, or the printer ran out of paper while printing). The
    /// job becomes **unknown** and is never resubmitted automatically (RCP-007).
    Uncertain(String),
}

/// The command language a printer speaks.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CommandDialect {
    /// Epson ESC/POS and the many compatibles (Bixolon, Citizen, Xprinter, Rongta, Star in
    /// ESC/POS emulation, …).
    EscPos,
    /// Star Line Mode (Star TSP/SP/mC-Print in their native mode).
    StarLine,
    /// Plain text with line feeds only: anything else (line printers, "Generic / Text Only").
    PlainText,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CutSupport {
    None,
    Partial,
    Full,
}

/// What a printer can do. Built from the model profile and the operator's choices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterCapabilities {
    pub dialect: CommandDialect,
    /// Characters per line in the normal font. `None`: use the receipt paper-width setting.
    pub columns: Option<u16>,
    /// Code pages the printer has, in order of preference: (name such as "CP858", the
    /// number that selects it with the dialect's select-code-page command). Empty: ASCII only.
    pub code_pages: Vec<(String, u8)>,
    pub cut: CutSupport,
    /// Feed this many lines before cutting (or instead of cutting).
    pub feed_before_cut: u8,
    pub drawer_kick: bool,
    /// Can print 1D barcodes (CODE128) natively.
    pub barcode: bool,
    /// Can print QR codes natively.
    pub qr: bool,
    /// Answers real-time status queries over this connection.
    pub status_query: bool,
}

impl Default for PrinterCapabilities {
    /// The conservative profile the encoder has always produced: ESC/POS, ASCII, partial
    /// cut after three lines, no barcode, no drawer.
    fn default() -> Self {
        Self {
            dialect: CommandDialect::EscPos,
            columns: None,
            code_pages: Vec::new(),
            cut: CutSupport::Partial,
            feed_before_cut: 3,
            drawer_kick: false,
            barcode: false,
            qr: false,
            status_query: false,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PaperLevel {
    Ok,
    NearEnd,
    Out,
    Unknown,
}

/// A printer's real-time state, as far as it can report it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterStatus {
    pub online: bool,
    pub paper: PaperLevel,
    pub cover_open: bool,
    /// The drawer-open sensor on the kick connector, where wired.
    pub drawer_open: Option<bool>,
    /// A recoverable or unrecoverable error the printer reported (cutter jam, head hot…).
    pub error: Option<String>,
}

impl PrinterStatus {
    /// What a printer that cannot report status looks like.
    pub fn unknown() -> Self {
        Self {
            online: true,
            paper: PaperLevel::Unknown,
            cover_open: false,
            drawer_open: None,
            error: None,
        }
    }

    /// The failure this state means for a job about to be sent, if any. Nothing has been
    /// sent yet, so these are safe to report as failed.
    pub fn blocking_failure(&self) -> Option<PrintFailure> {
        if self.paper == PaperLevel::Out {
            Some(PrintFailure::PaperOut)
        } else if self.cover_open {
            Some(PrintFailure::CoverOpen)
        } else if !self.online || self.error.is_some() {
            Some(PrintFailure::Offline)
        } else {
            None
        }
    }
}

/// A receipt printer (or the virtual one). `submit` may never return (a hung connection):
/// the caller bounds it with a timeout and then treats the outcome as **unknown**, never
/// as failed, because the paper may already be out of the printer (RCP-006).
#[async_trait::async_trait]
pub trait ReceiptOutputPort: Send + Sync {
    /// Which adapter this is, as stored in `print_jobs.adapter`.
    fn adapter(&self) -> &'static str;

    /// Which printer, in words ("192.168.1.50:9100", "COM3 @ 9600"), for the job history.
    fn target(&self) -> Option<String> {
        None
    }

    /// What the encoder may use. The default is the conservative ESC/POS profile.
    fn capabilities(&self) -> PrinterCapabilities {
        PrinterCapabilities::default()
    }

    /// Sends one complete job, already encoded for `capabilities().dialect`.
    async fn submit(&self, job_name: &str, bytes: &[u8]) -> PrintResult;

    /// The printer's current state. Printers that cannot report give `unknown()`.
    async fn status(&self) -> Result<PrinterStatus, DeviceError> {
        Ok(PrinterStatus::unknown())
    }
}
