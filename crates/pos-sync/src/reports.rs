//! The consolidated sales report (E14-T05, REP-008 minimal, PRD §15):
//! `GET /v1/reports/sales?from=&to=&groupBy=day|register|tender`.
//!
//! Every figure is in minor units of the business currency, from live sales and returns the
//! cloud has received. `completeness` says, per register, whether the cloud can vouch for
//! having everything that register did: it drives the "Fully synchronized" versus
//! "Last synced HH:MM — N registers have unsynced data" label.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A register counts as up to date when the cloud heard from it within this long, and it
/// then held nothing the cloud had not accepted (PRD §15: "ack is current within 5 min").
pub const CURRENT_WITHIN_SECS: i64 = 300;

/// The widest range one request may ask for, in days (inclusive).
pub const MAX_RANGE_DAYS: i64 = 366;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    Day,
    Register,
    Tender,
}

/// One row. By day or register: `salesCount`/`grossMinor` are sales, `refundsMinor` the
/// refunds of returns made that day or at that register. By tender: the confirmed tenders
/// of that kind and the confirmed refunds paid back with it (`taxMinor` and
/// `discountMinor` are 0 there).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SalesReportRow {
    /// `YYYY-MM-DD`, the register id, or the tender kind (`cash`, `card_external`, …).
    pub key: String,
    /// What to show: the date, the register's code, or the tender kind.
    pub label: String,
    pub sales_count: i64,
    pub gross_minor: i64,
    pub tax_minor: i64,
    pub discount_minor: i64,
    pub returns_count: i64,
    pub refunds_minor: i64,
    /// `grossMinor - refundsMinor`.
    pub net_minor: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterCompleteness {
    pub register_id: Uuid,
    pub code: String,
    pub name: String,
    /// RFC 3339: the last time this register's state was confirmed, by a push the cloud
    /// accepted or a pull reporting what it still holds. `None`: never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ack_at: Option<String>,
    /// The register last said it holds events the cloud has not accepted.
    pub pending_known: bool,
    /// Confirmed within [`CURRENT_WITHIN_SECS`] of `asOf` with nothing pending.
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Completeness {
    /// The registers with an active device.
    pub registers: Vec<RegisterCompleteness>,
}

impl Completeness {
    /// Every register is current.
    pub fn fully_synchronized(&self) -> bool {
        self.registers.iter().all(|r| r.current)
    }

    /// How many registers may hold data the report is missing.
    pub fn unsynced(&self) -> usize {
        self.registers.iter().filter(|r| !r.current).count()
    }

    /// The oldest confirmation among the registers that are not current (what "Last
    /// synced" refers to); `None` when every one is current, or one never confirmed.
    pub fn last_synced(&self) -> Option<&str> {
        let behind: Vec<_> = self.registers.iter().filter(|r| !r.current).collect();
        if behind.is_empty() || behind.iter().any(|r| r.last_ack_at.is_none()) {
            return None;
        }
        behind.iter().filter_map(|r| r.last_ack_at.as_deref()).min()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SalesReport {
    pub group_by: GroupBy,
    pub from: String,
    pub to: String,
    pub currency_code: String,
    pub currency_exponent: u8,
    pub rows: Vec<SalesReportRow>,
    /// RFC 3339, the server's clock when the report was made.
    pub as_of: String,
    pub completeness: Completeness,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(current: bool, at: Option<&str>) -> RegisterCompleteness {
        RegisterCompleteness {
            register_id: Uuid::nil(),
            code: "R01".into(),
            name: "Till".into(),
            last_ack_at: at.map(str::to_owned),
            pending_known: !current,
            current,
        }
    }

    #[test]
    fn the_label_figures() {
        let all = Completeness {
            registers: vec![reg(true, Some("2026-11-17T10:00:00Z"))],
        };
        assert!(all.fully_synchronized());
        assert_eq!((all.unsynced(), all.last_synced()), (0, None));
        let some = Completeness {
            registers: vec![
                reg(true, Some("2026-11-17T10:00:00Z")),
                reg(false, Some("2026-11-17T09:40:00Z")),
                reg(false, Some("2026-11-17T09:10:00Z")),
            ],
        };
        assert!(!some.fully_synchronized());
        assert_eq!(some.unsynced(), 2);
        assert_eq!(some.last_synced(), Some("2026-11-17T09:10:00Z"));
        let never = Completeness {
            registers: vec![reg(false, None)],
        };
        assert_eq!(never.last_synced(), None);
    }
}
