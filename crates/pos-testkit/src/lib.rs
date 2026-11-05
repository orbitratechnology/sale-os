//! Deterministic fakes for tests: clocks and ID generators (E03-T01).
//! This crate must only ever be a `[dev-dependencies]` entry (enforced by `cargo xtask check-deps`).

use pos_domain::{Clock, IdGenerator, Timestamp};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// A clock that returns a fixed instant until it is moved with [`FixedClock::set`] or
/// [`FixedClock::advance_ms`].
#[derive(Debug)]
pub struct FixedClock(AtomicI64);

impl FixedClock {
    pub fn new(at: Timestamp) -> Self {
        Self(AtomicI64::new(at.as_millis()))
    }
    pub fn set(&self, at: Timestamp) {
        self.0.store(at.as_millis(), Ordering::SeqCst);
    }
    pub fn advance_ms(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

/// A clock that moves forward by `step_ms` every time it is read.
#[derive(Debug)]
pub struct SteppingClock {
    next: AtomicI64,
    step_ms: i64,
}

impl SteppingClock {
    pub fn new(start: Timestamp, step_ms: i64) -> Self {
        Self {
            next: AtomicI64::new(start.as_millis()),
            step_ms,
        }
    }
}

impl Clock for SteppingClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.next.fetch_add(self.step_ms, Ordering::SeqCst))
    }
}

/// Deterministic, strictly increasing, valid UUIDv7 values: the n-th ID uses timestamp
/// `base_ms + n` and random bits derived from `n`. Two generators with the same base produce
/// identical sequences.
#[derive(Debug)]
pub struct SeqIdGenerator {
    base_ms: u64,
    counter: AtomicU64,
}

impl SeqIdGenerator {
    pub fn new(base_ms: u64) -> Self {
        Self {
            base_ms,
            counter: AtomicU64::new(0),
        }
    }
}

impl Default for SeqIdGenerator {
    fn default() -> Self {
        // 2026-01-01T00:00:00Z
        Self::new(1_767_225_600_000)
    }
}

impl IdGenerator for SeqIdGenerator {
    fn new_uuid_v7(&self) -> uuid::Uuid {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let mut random = [0u8; 10];
        random[2..].copy_from_slice(&n.to_be_bytes());
        uuid::Builder::from_unix_timestamp_millis(self.base_ms + n, &random).into_uuid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_moves_only_when_told() {
        let c = FixedClock::new(Timestamp::from_millis(1_000));
        assert_eq!(c.now(), c.now());
        c.advance_ms(500);
        assert_eq!(c.now().as_millis(), 1_500);
    }

    #[test]
    fn stepping_clock_steps() {
        let c = SteppingClock::new(Timestamp::from_millis(0), 10);
        assert_eq!([c.now(), c.now(), c.now()].map(|t| t.as_millis()), [0, 10, 20]);
    }

    #[test]
    fn seq_ids_are_deterministic_ordered_v7() {
        let a = SeqIdGenerator::default();
        let b = SeqIdGenerator::default();
        let xs: Vec<_> = (0..100).map(|_| a.new_uuid_v7()).collect();
        let ys: Vec<_> = (0..100).map(|_| b.new_uuid_v7()).collect();
        assert_eq!(xs, ys);
        assert!(xs.windows(2).all(|w| w[0] < w[1]));
        assert!(xs.iter().all(|u| u.get_version_num() == 7));
    }
}
