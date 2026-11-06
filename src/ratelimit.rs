//! A small in-memory limiter: at most `limit` events per key within a
//! sliding `window`. Used for failed sign-ins (threat model F1) and for the
//! assistant and MCP quotas (F16). It lives in one process; a restart or a
//! second instance starts again from zero, which is acceptable for a brake.

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

/// Keys kept at most; beyond this the oldest idle ones are dropped.
const MAX_KEYS: usize = 20_000;

#[derive(Debug)]
pub struct RateLimiter {
    limit: u32,
    window: Duration,
    events: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    /// `limit == 0` switches the limiter off.
    pub fn new(limit: u32, window: Duration) -> RateLimiter {
        RateLimiter {
            limit,
            window,
            events: Mutex::new(HashMap::new()),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut HashMap<String, VecDeque<Instant>>) -> T) -> T {
        let mut guard = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    fn prune(&self, queue: &mut VecDeque<Instant>, now: Instant) {
        while queue
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            queue.pop_front();
        }
    }

    /// `Err(seconds to wait)` when the key is over its limit; counts nothing.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        if self.limit == 0 {
            return Ok(());
        }
        let now = Instant::now();
        self.with(|map| {
            let Some(queue) = map.get_mut(key) else {
                return Ok(());
            };
            self.prune(queue, now);
            if queue.len() >= self.limit as usize {
                let oldest = queue.front().copied().unwrap_or(now);
                let wait = self.window.saturating_sub(now.duration_since(oldest));
                return Err(wait.as_secs().max(1));
            }
            Ok(())
        })
    }

    /// Counts one event for the key.
    pub fn record(&self, key: &str) {
        if self.limit == 0 {
            return;
        }
        let now = Instant::now();
        self.with(|map| {
            if map.len() >= MAX_KEYS && !map.contains_key(key) {
                map.retain(|_, q| {
                    q.back()
                        .is_some_and(|t| now.duration_since(*t) < self.window)
                });
            }
            if map.len() >= MAX_KEYS && !map.contains_key(key) {
                return;
            }
            let queue = map.entry(key.to_string()).or_default();
            self.prune(queue, now);
            queue.push_back(now);
        });
    }

    /// Checks and counts in one step (quotas).
    pub fn hit(&self, key: &str) -> Result<(), u64> {
        self.check(key)?;
        self.record(key);
        Ok(())
    }

    /// Forgets the key (after a successful sign-in).
    pub fn clear(&self, key: &str) {
        self.with(|map| {
            map.remove(key);
        });
    }
}

/// The limiters of one server.
#[derive(Debug)]
pub struct Limits {
    /// Failed password checks per user name / user / desktop exchange.
    pub failed_logins: RateLimiter,
    /// Assistant questions per user.
    pub assistant: RateLimiter,
    /// MCP requests per user.
    pub mcp: RateLimiter,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_the_limit_and_forgets() {
        let l = RateLimiter::new(2, Duration::from_secs(60));
        assert!(l.hit("a").is_ok());
        assert!(l.hit("a").is_ok());
        let wait = l.hit("a").unwrap_err();
        assert!((1..=60).contains(&wait));
        assert!(l.hit("b").is_ok(), "keys are independent");
        l.clear("a");
        assert!(l.hit("a").is_ok());
    }

    #[test]
    fn check_alone_counts_nothing_and_zero_disables() {
        let l = RateLimiter::new(1, Duration::from_secs(60));
        for _ in 0..5 {
            assert!(l.check("a").is_ok());
        }
        l.record("a");
        assert!(l.check("a").is_err());
        let off = RateLimiter::new(0, Duration::from_secs(60));
        for _ in 0..100 {
            assert!(off.hit("a").is_ok());
        }
    }

    #[test]
    fn events_expire() {
        let l = RateLimiter::new(1, Duration::from_millis(20));
        l.record("a");
        assert!(l.check("a").is_err());
        std::thread::sleep(Duration::from_millis(40));
        assert!(l.check("a").is_ok());
    }
}
