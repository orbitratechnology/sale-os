//! Time and identity ports (ADR-006). The domain never reads the system clock directly:
//! use cases receive a [`Clock`] and an [`IdGenerator`] so tests can be deterministic.

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// An instant as UTC epoch milliseconds — the storage form of every `*_at` column.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }
    pub const fn as_millis(self) -> i64 {
        self.0
    }
    pub fn to_datetime(self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp_millis(self.0)
    }
    pub fn from_datetime(dt: DateTime<Utc>) -> Self {
        Self(dt.timestamp_millis())
    }
}

/// The trading day in the business time zone, computed once at commit time and never
/// recomputed (ADR-006). Serialised as `YYYY-MM-DD`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(transparent)]
pub struct BusinessDate(#[cfg_attr(feature = "specta", specta(type = String))] NaiveDate);

impl BusinessDate {
    pub const fn from_naive(d: NaiveDate) -> Self {
        Self(d)
    }
    pub const fn as_naive(self) -> NaiveDate {
        self.0
    }
}

impl std::fmt::Display for BusinessDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.format("%Y-%m-%d"))
    }
}

impl std::str::FromStr for BusinessDate {
    type Err = TimeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(Self)
            .map_err(|_| TimeError::InvalidDate(s.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("timestamp out of range: {0}")]
    OutOfRange(i64),
    #[error("unknown IANA time zone: {0:?}")]
    UnknownTimeZone(String),
    #[error("invalid date (expected YYYY-MM-DD): {0:?}")]
    InvalidDate(String),
}

impl crate::HasErrorCode for TimeError {
    fn error_code(&self) -> crate::ErrorCode {
        crate::ErrorCode::ValidationFailed
    }
}

/// Parses and validates an IANA time-zone name such as `"Asia/Kolkata"`.
pub fn parse_time_zone(name: &str) -> Result<Tz, TimeError> {
    name.parse::<Tz>()
        .map_err(|_| TimeError::UnknownTimeZone(name.to_owned()))
}

/// The business date of an instant in the business time zone.
pub fn business_date(at: Timestamp, tz: Tz) -> Result<BusinessDate, TimeError> {
    let utc = at.to_datetime().ok_or(TimeError::OutOfRange(at.as_millis()))?;
    Ok(BusinessDate(utc.with_timezone(&tz).date_naive()))
}

/// Source of "now". Inject a fake in tests (`pos-testkit`).
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// Source of new UUIDv7 identifiers. Inject a deterministic fake in tests.
pub trait IdGenerator: Send + Sync {
    fn new_uuid_v7(&self) -> uuid::Uuid;
}

/// Production clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp(Utc::now().timestamp_millis())
    }
}

/// Production ID generator: process-monotonic UUIDv7.
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidV7Generator;

impl IdGenerator for UuidV7Generator {
    fn new_uuid_v7(&self) -> uuid::Uuid {
        uuid::Uuid::now_v7()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(rfc3339: &str) -> Timestamp {
        Timestamp::from_datetime(DateTime::parse_from_rfc3339(rfc3339).unwrap().with_timezone(&Utc))
    }

    fn date(tz: &str, ts: &str) -> String {
        business_date(at(ts), parse_time_zone(tz).unwrap())
            .unwrap()
            .to_string()
    }

    #[test]
    fn kolkata_half_hour_offset_day_boundary() {
        // +05:30: 18:29:59.999Z is 23:59:59.999 local; 18:30Z is midnight next day.
        assert_eq!(date("Asia/Kolkata", "2026-11-05T18:29:59.999Z"), "2026-11-05");
        assert_eq!(date("Asia/Kolkata", "2026-11-05T18:30:00.000Z"), "2026-11-06");
    }

    #[test]
    fn chatham_quarter_hour_offset_in_daylight_time() {
        // Chatham observes +13:45 in November (southern-hemisphere DST).
        assert_eq!(date("Pacific/Chatham", "2026-11-05T10:14:59.999Z"), "2026-11-05");
        assert_eq!(date("Pacific/Chatham", "2026-11-05T10:15:00.000Z"), "2026-11-06");
    }

    #[test]
    fn london_dst_end() {
        // 2026-10-25 01:00 UTC BST ends. 23:00Z on the 24th is 00:00 BST on the 25th.
        assert_eq!(date("Europe/London", "2026-10-24T22:59:59.999Z"), "2026-10-24");
        assert_eq!(date("Europe/London", "2026-10-24T23:00:00.000Z"), "2026-10-25");
        // After the change, local = UTC.
        assert_eq!(date("Europe/London", "2026-10-25T23:59:59.999Z"), "2026-10-25");
    }

    #[test]
    fn new_york_dst_start() {
        // 2026-03-08 clocks jump 02:00 -> 03:00 EST->EDT. Midnight local is 05:00Z (EST).
        assert_eq!(date("America/New_York", "2026-03-08T04:59:59.999Z"), "2026-03-07");
        assert_eq!(date("America/New_York", "2026-03-08T05:00:00.000Z"), "2026-03-08");
        // Midnight on the 9th is 04:00Z (EDT).
        assert_eq!(date("America/New_York", "2026-03-09T03:59:59.999Z"), "2026-03-08");
        assert_eq!(date("America/New_York", "2026-03-09T04:00:00.000Z"), "2026-03-09");
    }

    #[test]
    fn unknown_zone_rejected() {
        assert!(matches!(
            parse_time_zone("Mars/Olympus"),
            Err(TimeError::UnknownTimeZone(_))
        ));
    }

    #[test]
    fn business_date_text_round_trip() {
        let d: BusinessDate = "2026-02-28".parse().unwrap();
        assert_eq!(d.to_string(), "2026-02-28");
        assert!("2026-02-30".parse::<BusinessDate>().is_err());
        assert_eq!(serde_json_roundtrip(d), "\"2026-02-28\"");
    }

    fn serde_json_roundtrip(d: BusinessDate) -> String {
        serde_json::to_string(&d).unwrap()
    }

    #[test]
    fn timestamp_out_of_range() {
        let tz = parse_time_zone("UTC").unwrap();
        assert!(business_date(Timestamp::from_millis(i64::MAX), tz).is_err());
        let ok = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(
            business_date(Timestamp::from_datetime(ok), tz)
                .unwrap()
                .to_string(),
            "2026-01-01"
        );
    }
}
