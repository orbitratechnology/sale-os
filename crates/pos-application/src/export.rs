//! CSV exports (E14-T04, REP-006, CAT-009): RFC 4180, UTF-8 with a BOM (Excel opens it
//! correctly), CRLF line ends, amounts as exact decimals with `.` and the currency's digits.
//! No value ever passes through floating point.

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::QueryError;
use crate::reports::{ReportQueries, TopBy};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::time::BusinessDate;
use pos_domain::{ErrorCode, Permission};
use std::sync::Arc;

/// One cell, typed so amounts and quantities are formatted exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    Text(String),
    /// Minor units; written with the currency's decimal digits.
    Money(i64),
    /// Milli-units; written without trailing zeros ("1.25", "2").
    Qty(i64),
    Int(i64),
    Empty,
}

impl From<&str> for Cell {
    fn from(s: &str) -> Self {
        Cell::Text(s.to_owned())
    }
}
impl From<String> for Cell {
    fn from(s: String) -> Self {
        Cell::Text(s)
    }
}
impl From<Option<String>> for Cell {
    fn from(s: Option<String>) -> Self {
        s.map_or(Cell::Empty, Cell::Text)
    }
}

/// `1234` with 2 digits → `12.34`; `-5` → `-0.05`; 0 digits → `1234`.
pub fn decimal(minor: i64, digits: u32) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    if digits == 0 {
        return format!("{sign}{abs}");
    }
    let scale = 10_u64.pow(digits);
    format!(
        "{sign}{}.{:0width$}",
        abs / scale,
        abs % scale,
        width = digits as usize
    )
}

/// Milli-units without trailing zeros: 1250 → `1.25`, 2000 → `2`.
pub fn quantity(milli: i64) -> String {
    let s = decimal(milli, 3);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s.to_owned()
    }
}

/// RFC 4180: quote a field holding a comma, quote, CR or LF, doubling quotes. A field that
/// starts with `=`, `+`, `-` or `@` and is text gets a leading `'` so a spreadsheet does not
/// run it as a formula (CSV injection).
fn field(cell: &Cell, money_digits: u32) -> String {
    let raw = match cell {
        Cell::Text(s) => {
            if s.starts_with(['=', '+', '-', '@']) {
                format!("'{s}")
            } else {
                s.clone()
            }
        }
        Cell::Money(m) => decimal(*m, money_digits),
        Cell::Qty(q) => quantity(*q),
        Cell::Int(i) => i.to_string(),
        Cell::Empty => String::new(),
    };
    if raw.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw
    }
}

/// A finished table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub header: Vec<&'static str>,
    pub rows: Vec<Vec<Cell>>,
}

impl Table {
    /// The file's bytes: BOM, header, rows, CRLF after every record.
    pub fn to_csv(&self, money_digits: u32) -> Vec<u8> {
        let mut out = String::from("\u{feff}");
        out.push_str(&self.header.join(","));
        out.push_str("\r\n");
        for r in &self.rows {
            let line: Vec<String> = r.iter().map(|c| field(c, money_digits)).collect();
            out.push_str(&line.join(","));
            out.push_str("\r\n");
        }
        out.into_bytes()
    }
}

/// What can be exported.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dataset {
    /// One row per sale line.
    Transactions,
    Tenders,
    /// One row per return line.
    Returns,
    /// The catalogue, one row per variant (the format the CSV import reads).
    Products,
    Stock,
    SalesSummary,
    TenderMix,
    TopProducts,
}

impl Dataset {
    pub const fn as_str(self) -> &'static str {
        match self {
            Dataset::Transactions => "transactions",
            Dataset::Tenders => "tenders",
            Dataset::Returns => "returns",
            Dataset::Products => "products",
            Dataset::Stock => "stock",
            Dataset::SalesSummary => "sales_summary",
            Dataset::TenderMix => "tender_mix",
            Dataset::TopProducts => "top_products",
        }
    }

    /// Whether the dataset is for a date range.
    pub const fn needs_range(self) -> bool {
        !matches!(self, Dataset::Products | Dataset::Stock)
    }

    pub const fn permission(self) -> Permission {
        match self {
            Dataset::Products | Dataset::Stock => Permission::CatalogExport,
            _ => Permission::ReportsExport,
        }
    }
}

/// Raw rows for the record datasets (transactions, tenders, returns, products, stock).
#[async_trait::async_trait]
pub trait ExportQueries: Send + Sync {
    async fn export_table(
        &self,
        dataset: Dataset,
        range: Option<(BusinessDate, BusinessDate)>,
    ) -> Result<Table, QueryError>;
}

pub struct ExportService {
    pub store: Arc<dyn LocalStore>,
    pub exports: Arc<dyn ExportQueries>,
    pub reports: Arc<dyn ReportQueries>,
}

/// A built export, not yet saved.
#[derive(Debug)]
pub struct Export {
    pub dataset: Dataset,
    pub bytes: Vec<u8>,
    pub rows: usize,
    /// e.g. `transactions_2026-01-01_2026-01-31.csv`.
    pub file_name: String,
}

impl ExportService {
    pub async fn build(
        &self,
        ctx: &CommandContext,
        dataset: Dataset,
        range: Option<(BusinessDate, BusinessDate)>,
    ) -> Result<Export, AppError> {
        ctx.authorize(dataset.permission())?;
        let range = match (dataset.needs_range(), range) {
            (true, None) => {
                return Err(AppError::Validation(
                    "choose the dates to export".into(),
                    ErrorCode::ValidationFailed,
                ));
            }
            (true, Some((f, t))) if f > t => {
                return Err(AppError::Validation(
                    "the start date is after the end date".into(),
                    ErrorCode::ValidationFailed,
                ));
            }
            (false, _) => None,
            (true, r) => r,
        };
        let (_, digits) = self.reports.report_settings().await?;
        let table = match dataset {
            Dataset::SalesSummary | Dataset::TenderMix | Dataset::TopProducts => {
                let (from, to) = range.expect("checked above");
                self.report_table(dataset, from, to).await?
            }
            _ => self.exports.export_table(dataset, range).await?,
        };
        let file_name = match range {
            Some((f, t)) => format!("{}_{f}_{t}.csv", dataset.as_str()),
            None => format!(
                "{}_{}.csv",
                dataset.as_str(),
                pos_domain::time::local_date_time(ctx.now(), "UTC")
                    .unwrap_or_default()
                    .get(..10)
                    .unwrap_or("")
            ),
        };
        Ok(Export {
            dataset,
            rows: table.rows.len(),
            bytes: table.to_csv(u32::from(digits)),
            file_name,
        })
    }

    /// Audits a saved export (`report.exported`), after the file was written.
    pub async fn record(&self, ctx: &CommandContext, export: &Export) -> Result<(), AppError> {
        let ids = ctx.ids.as_ref();
        let entry = AuditEntry::new(ids, audit::REPORT_EXPORTED, "export", export.dataset.as_str()).with_metadata(
            serde_json::json!({ "dataset": export.dataset.as_str(), "rows": export.rows, "file": export.file_name }),
        );
        let set = WriteSet::builder(ctx.write_context())
            .outbox(OutboxDraft::audit_recorded(ids, &entry)?)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }

    async fn report_table(
        &self,
        dataset: Dataset,
        from: BusinessDate,
        to: BusinessDate,
    ) -> Result<Table, AppError> {
        Ok(match dataset {
            Dataset::SalesSummary => {
                let days = self.reports.daily_figures(from, to).await?;
                Table {
                    header: vec![
                        "business_date",
                        "sales",
                        "gross",
                        "line_discounts",
                        "order_discounts",
                        "tax",
                        "cash_rounding",
                        "sales_total",
                        "returns",
                        "refunds",
                        "refunded_tax",
                        "net_incl_tax",
                        "net_excl_tax",
                    ],
                    rows: days
                        .iter()
                        .map(|d| {
                            let f = &d.figures;
                            vec![
                                Cell::Text(d.date.to_string()),
                                Cell::Int(i64::from(f.sales_count)),
                                Cell::Money(f.gross.minor()),
                                Cell::Money(f.line_discounts.minor()),
                                Cell::Money(f.order_discounts.minor()),
                                Cell::Money(f.tax.minor()),
                                Cell::Money(f.rounding.minor()),
                                Cell::Money(f.sales_total.minor()),
                                Cell::Int(i64::from(f.returns_count)),
                                Cell::Money(f.refunds.minor()),
                                Cell::Money(f.refunded_tax.minor()),
                                Cell::Money(f.net_incl_tax().minor()),
                                Cell::Money(f.net_excl_tax().minor()),
                            ]
                        })
                        .collect(),
                }
            }
            Dataset::TenderMix => Table {
                header: vec![
                    "tender",
                    "confirmed_by",
                    "count",
                    "taken",
                    "refund_count",
                    "refunded",
                    "net",
                ],
                rows: self
                    .reports
                    .tender_mix(from, to)
                    .await?
                    .iter()
                    .map(|r| {
                        vec![
                            Cell::from(r.kind.as_str()),
                            Cell::Text(r.confirmation_source.clone()),
                            Cell::Int(i64::from(r.count)),
                            Cell::Money(r.taken.minor()),
                            Cell::Int(i64::from(r.refund_count)),
                            Cell::Money(r.refunded.minor()),
                            Cell::Money(r.net().minor()),
                        ]
                    })
                    .collect(),
            },
            _ => Table {
                header: vec![
                    "rank",
                    "variant_id",
                    "name",
                    "variant",
                    "net_quantity",
                    "net_value",
                ],
                rows: self
                    .reports
                    .top_products(from, to, TopBy::Value, 1000)
                    .await?
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        vec![
                            Cell::Int(i as i64 + 1),
                            Cell::Text(r.variant_id.to_string()),
                            Cell::Text(r.name.clone()),
                            Cell::Text(r.label.clone()),
                            Cell::Qty(r.quantity.milli()),
                            Cell::Money(r.value.minor()),
                        ]
                    })
                    .collect(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_decimals_and_rfc_4180() {
        assert_eq!(decimal(1234, 2), "12.34");
        assert_eq!(decimal(-5, 2), "-0.05");
        assert_eq!(decimal(7, 0), "7");
        assert_eq!(decimal(1_000_001, 3), "1000.001");
        assert_eq!(decimal(i64::MIN, 2), "-92233720368547758.08");
        assert_eq!(
            (quantity(1250), quantity(2000), quantity(0), quantity(-500)),
            ("1.25".into(), "2".into(), "0".into(), "-0.5".into())
        );
        let t = Table {
            header: vec!["name", "price", "qty"],
            rows: vec![
                vec![Cell::from("Tea, green"), Cell::Money(250), Cell::Qty(1500)],
                vec![Cell::from("Say \"hi\"\nnow"), Cell::Money(-5), Cell::Empty],
                vec![Cell::from("=HYPERLINK(\"x\")"), Cell::Money(0), Cell::Int(3)],
                vec![Cell::from("Crème brûlée"), Cell::Money(99), Cell::Qty(1)],
            ],
        };
        let csv = String::from_utf8(t.to_csv(2)).unwrap();
        assert_eq!(
            csv,
            "\u{feff}name,price,qty\r\n\"Tea, green\",2.50,1.5\r\n\"Say \"\"hi\"\"\nnow\",-0.05,\r\n\"'=HYPERLINK(\"\"x\"\")\",0.00,3\r\nCrème brûlée,0.99,0.001\r\n"
        );
    }
}
