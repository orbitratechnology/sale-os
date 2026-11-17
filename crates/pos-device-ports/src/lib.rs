//! Device port traits: barcode input, receipt output, cash drawer, payment
//! (docs/backlog/06-rust-interfaces.md §10). Receipt output lands with E13-T06; the others
//! with E07-T03, E10-T02 and Stage 5.

/// Why a printer did not print. Each maps to an actionable message in the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintFailure {
    PaperOut,
    CoverOpen,
    Disconnected,
    Rejected(String),
}

impl PrintFailure {
    /// Stored in `print_jobs.last_error_code`.
    pub fn code(&self) -> &'static str {
        match self {
            PrintFailure::PaperOut => "paper_out",
            PrintFailure::CoverOpen => "cover_open",
            PrintFailure::Disconnected => "disconnected",
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
}

/// A receipt printer (or the virtual one). `submit` may never return (a hung connection):
/// the caller bounds it with a timeout and then treats the outcome as **unknown**, never
/// as failed, because the paper may already be out of the printer (RCP-006).
#[async_trait::async_trait]
pub trait ReceiptOutputPort: Send + Sync {
    /// Which adapter this is, as stored in `print_jobs.adapter`.
    fn adapter(&self) -> &'static str;
    async fn submit(&self, job_name: &str, escpos: &[u8]) -> PrintResult;
}
