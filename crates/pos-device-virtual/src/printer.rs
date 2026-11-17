//! Virtual receipt printer (E13-T06, PRD §12.4): captures the exact ESC/POS byte stream to
//! `<name>.bin` and a human-readable decode to `<name>.txt`, and plays scripted outcomes
//! (accept, paper out, cover open, disconnected, timeout) for tests and the dev panel.

use pos_device_ports::{PrintFailure, PrintResult, ReceiptOutputPort};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// What the next submission does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scenario {
    Accept,
    Fail(PrintFailure),
    /// Never answers: the caller's timeout decides (the outcome is then unknown).
    Timeout,
}

#[derive(Debug)]
pub struct VirtualPrinter {
    out_dir: PathBuf,
    script: Mutex<VecDeque<Scenario>>,
    submissions: AtomicUsize,
}

/// Files written for one captured job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedJob {
    pub bin: PathBuf,
    pub txt: PathBuf,
}

impl VirtualPrinter {
    /// Captures into `out_dir` (created if missing). Accepts everything until scripted.
    pub fn new(out_dir: impl Into<PathBuf>) -> Self {
        Self {
            out_dir: out_dir.into(),
            script: Mutex::new(VecDeque::new()),
            submissions: AtomicUsize::new(0),
        }
    }

    pub fn out_dir(&self) -> &Path {
        &self.out_dir
    }

    /// Outcomes for the next submissions, in order; afterwards it accepts again.
    pub fn script(&self, outcomes: impl IntoIterator<Item = Scenario>) {
        self.lock().extend(outcomes);
    }

    /// How many times anything was submitted (accepted or not).
    pub fn submissions(&self) -> usize {
        self.submissions.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Scenario>> {
        self.script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Writes the job. `name` is sanitised to `[A-Za-z0-9._-]` so a receipt number cannot
    /// escape the capture directory.
    pub fn capture(&self, name: &str, bytes: &[u8]) -> std::io::Result<CapturedJob> {
        std::fs::create_dir_all(&self.out_dir)?;
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let safe = safe.trim_start_matches('.');
        let stem = if safe.is_empty() { "job" } else { safe };
        let bin = self.out_dir.join(format!("{stem}.bin"));
        let txt = self.out_dir.join(format!("{stem}.txt"));
        std::fs::write(&bin, bytes)?;
        std::fs::write(&txt, pos_receipts::escpos::decode(bytes))?;
        Ok(CapturedJob { bin, txt })
    }
}

#[async_trait::async_trait]
impl ReceiptOutputPort for VirtualPrinter {
    fn adapter(&self) -> &'static str {
        "virtual"
    }

    async fn submit(&self, job_name: &str, escpos: &[u8]) -> PrintResult {
        self.submissions.fetch_add(1, Ordering::SeqCst);
        let next = self.lock().pop_front().unwrap_or(Scenario::Accept);
        match next {
            Scenario::Accept => match self.capture(job_name, escpos) {
                Ok(_) => PrintResult::Printed,
                Err(e) => PrintResult::Failed(PrintFailure::Rejected(e.to_string())),
            },
            Scenario::Fail(f) => PrintResult::Failed(f),
            Scenario::Timeout => std::future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_receipts::escpos::{Cut, EscPos};

    #[test]
    fn captures_bytes_and_decode() {
        let dir = tempfile::tempdir().unwrap();
        let printer = VirtualPrinter::new(dir.path().join("captures"));
        let mut p = EscPos::new();
        p.line("HELLO").cut(Cut::Partial);
        let job = printer.capture("R01-000001", p.bytes()).unwrap();
        assert_eq!(std::fs::read(&job.bin).unwrap(), p.bytes());
        assert_eq!(
            std::fs::read_to_string(&job.txt).unwrap(),
            "[INIT]\n\"HELLO\"[LF]\n[CUT partial]\n"
        );
    }

    #[test]
    fn job_names_cannot_escape_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let printer = VirtualPrinter::new(dir.path());
        let job = printer.capture("../../etc/passwd", b"x").unwrap();
        assert_eq!(job.bin.parent().unwrap(), dir.path());
        assert_eq!(job.bin.file_name().unwrap(), "_.._etc_passwd.bin");
    }

    #[tokio::test]
    async fn plays_the_script_then_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let printer = VirtualPrinter::new(dir.path());
        printer.script([Scenario::Fail(PrintFailure::PaperOut)]);
        assert_eq!(
            printer.submit("a", b"x").await,
            PrintResult::Failed(PrintFailure::PaperOut)
        );
        assert_eq!(printer.submit("b", b"x").await, PrintResult::Printed);
        assert!(dir.path().join("b.bin").exists() && !dir.path().join("a.bin").exists());
        assert_eq!(printer.submissions(), 2);
    }
}
