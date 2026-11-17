//! Sign-in rate limiting (E16-T03): per key (an IP address or an email), 5 attempts a
//! minute; beyond that the key is blocked for a minute, then 2, 4, 8… (at most an hour)
//! while it keeps going. In memory: a restart forgets, which only ever helps a real owner.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PER_WINDOW: u32 = 5;
pub const WINDOW: Duration = Duration::from_secs(60);
pub const MAX_BLOCK: Duration = Duration::from_secs(3600);

#[derive(Debug)]
struct Entry {
    window_start: Instant,
    count: u32,
    blocked_until: Option<Instant>,
    strikes: u32,
}

#[derive(Debug, Default)]
pub struct RateLimiter {
    entries: Mutex<HashMap<String, Entry>>,
}

impl RateLimiter {
    /// Counts an attempt for `key`. `Err(wait)` when it must wait that long first.
    pub fn attempt(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() > 50_000 {
            // Forget keys that are neither counting nor blocked.
            map.retain(|_, e| {
                now.duration_since(e.window_start) < WINDOW || e.blocked_until.is_some_and(|b| b > now)
            });
        }
        let e = map.entry(key.to_owned()).or_insert(Entry {
            window_start: now,
            count: 0,
            blocked_until: None,
            strikes: 0,
        });
        if let Some(until) = e.blocked_until {
            if until > now {
                return Err(until - now);
            }
            e.blocked_until = None;
        }
        if now.duration_since(e.window_start) >= WINDOW {
            // A quiet full window forgives earlier strikes.
            if e.count <= PER_WINDOW {
                e.strikes = 0;
            }
            e.window_start = now;
            e.count = 0;
        }
        e.count += 1;
        if e.count <= PER_WINDOW {
            return Ok(());
        }
        e.strikes += 1;
        let block = WINDOW
            .checked_mul(1 << (e.strikes - 1).min(10))
            .unwrap_or(MAX_BLOCK)
            .min(MAX_BLOCK);
        e.blocked_until = Some(now + block);
        e.window_start = now + block;
        e.count = 0;
        Err(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_a_minute_then_exponential_blocks() {
        let r = RateLimiter::default();
        let t0 = Instant::now();
        for i in 0..5 {
            assert_eq!(r.attempt("ip:1", t0 + Duration::from_secs(i)), Ok(()));
        }
        assert_eq!(
            r.attempt("ip:1", t0 + Duration::from_secs(5)),
            Err(Duration::from_secs(60))
        );
        // Still blocked: told how long is left.
        assert_eq!(
            r.attempt("ip:1", t0 + Duration::from_secs(35)),
            Err(Duration::from_secs(30))
        );
        // Other keys are not affected.
        assert_eq!(r.attempt("ip:2", t0 + Duration::from_secs(35)), Ok(()));
        // After the block, five more, then twice as long.
        let t1 = t0 + Duration::from_secs(65);
        for i in 0..5 {
            assert_eq!(r.attempt("ip:1", t1 + Duration::from_secs(i)), Ok(()));
        }
        assert_eq!(
            r.attempt("ip:1", t1 + Duration::from_secs(5)),
            Err(Duration::from_secs(120))
        );
        // Never longer than an hour.
        let mut t = t1 + Duration::from_secs(200);
        for _ in 0..12 {
            for _ in 0..5 {
                let _ = r.attempt("ip:1", t);
            }
            match r.attempt("ip:1", t) {
                Err(d) => {
                    assert!(d <= MAX_BLOCK);
                    t += d;
                }
                Ok(()) => t += Duration::from_secs(1),
            }
        }
    }
}
