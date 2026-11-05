//! Virtual receipt printer: captures the exact ESC/POS byte stream to `<name>.bin` and a
//! human-readable decode to `<name>.txt` (PRD §12.4).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct VirtualPrinter {
    out_dir: PathBuf,
}

/// Files written for one captured job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedJob {
    pub bin: PathBuf,
    pub txt: PathBuf,
}

impl VirtualPrinter {
    /// Captures into `out_dir` (created if missing). Development builds use
    /// `target/virtual-printer/`; the app uses `<app data>/virtual-printer/`.
    pub fn new(out_dir: impl Into<PathBuf>) -> Self {
        Self {
            out_dir: out_dir.into(),
        }
    }

    pub fn out_dir(&self) -> &Path {
        &self.out_dir
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
}
