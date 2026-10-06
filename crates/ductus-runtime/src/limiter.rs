//! Delivery rate limiting, shared by all of an interface's workers.

use ductus_core::config::{OverLimit, RateLimit, RateMode};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub enum Decision {
    Proceed,
    /// Over the limit: try again after this long.
    Wait(Duration),
    /// Over the limit and configured to drop.
    Drop,
}

pub struct Limiter {
    config: RateLimit,
    state: Mutex<State>,
}

enum State {
    /// Token bucket: refills at per_minute/60 tokens per second up to `capacity`.
    Average { tokens: f64, capacity: f64, last: Instant },
    /// Fixed one-minute windows.
    Hard { window_start: Instant, used: u32 },
}

impl Limiter {
    pub fn new(config: RateLimit, now: Instant) -> Self {
        let state = match config.mode {
            RateMode::Average => {
                let capacity = f64::from(config.burst.unwrap_or(config.per_minute));
                State::Average { tokens: capacity, capacity, last: now }
            }
            RateMode::Hard => State::Hard { window_start: now, used: 0 },
        };
        Self { config, state: Mutex::new(state) }
    }

    /// Ask to send one message at `now`.
    pub fn acquire(&self, now: Instant) -> Decision {
        let per_second = f64::from(self.config.per_minute) / 60.0;
        let mut state = self.state.lock().expect("limiter lock");
        let wait = match &mut *state {
            State::Average { tokens, capacity, last } => {
                let elapsed = now.saturating_duration_since(*last).as_secs_f64();
                *tokens = (*tokens + elapsed * per_second).min(*capacity);
                *last = now;
                if *tokens >= 1.0 {
                    *tokens -= 1.0;
                    return Decision::Proceed;
                }
                Duration::from_secs_f64((1.0 - *tokens) / per_second)
            }
            State::Hard { window_start, used } => {
                let minute = Duration::from_secs(60);
                let since = now.saturating_duration_since(*window_start);
                if since >= minute {
                    // Jump to the window containing `now`.
                    let windows = since.as_secs() / 60;
                    *window_start += minute * windows as u32;
                    *used = 0;
                }
                if *used < self.config.per_minute {
                    *used += 1;
                    return Decision::Proceed;
                }
                (*window_start + minute).saturating_duration_since(now)
            }
        };
        match self.config.over_limit {
            OverLimit::Wait => Decision::Wait(wait.max(Duration::from_millis(1))),
            OverLimit::Drop => Decision::Drop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(per_minute: u32, burst: Option<u32>, mode: RateMode, over_limit: OverLimit) -> RateLimit {
        RateLimit { per_minute, mode, burst, over_limit }
    }

    #[test]
    fn average_allows_a_burst_then_paces() {
        let t0 = Instant::now();
        let l = Limiter::new(limit(60, Some(3), RateMode::Average, OverLimit::Wait), t0);
        for _ in 0..3 {
            assert_eq!(l.acquire(t0), Decision::Proceed);
        }
        // 60/min = 1 per second: the next token is a second away.
        assert!(matches!(l.acquire(t0), Decision::Wait(d) if d > Duration::from_millis(990)));
        assert_eq!(l.acquire(t0 + Duration::from_secs(1)), Decision::Proceed);
        assert!(matches!(l.acquire(t0 + Duration::from_secs(1)), Decision::Wait(_)));
        // A long pause refills only up to the burst size.
        let later = t0 + Duration::from_secs(600);
        for _ in 0..3 {
            assert_eq!(l.acquire(later), Decision::Proceed);
        }
        assert!(matches!(l.acquire(later), Decision::Wait(_)));
    }

    #[test]
    fn hard_cap_counts_per_minute_and_can_drop() {
        let t0 = Instant::now();
        let l = Limiter::new(limit(2, None, RateMode::Hard, OverLimit::Drop), t0);
        assert_eq!(l.acquire(t0), Decision::Proceed);
        assert_eq!(l.acquire(t0 + Duration::from_secs(10)), Decision::Proceed);
        assert_eq!(l.acquire(t0 + Duration::from_secs(20)), Decision::Drop);
        assert_eq!(l.acquire(t0 + Duration::from_secs(61)), Decision::Proceed);
        assert_eq!(l.acquire(t0 + Duration::from_secs(185)), Decision::Proceed);
    }

    #[test]
    fn hard_cap_wait_lasts_until_the_next_minute() {
        let t0 = Instant::now();
        let l = Limiter::new(limit(1, None, RateMode::Hard, OverLimit::Wait), t0);
        assert_eq!(l.acquire(t0), Decision::Proceed);
        assert_eq!(l.acquire(t0 + Duration::from_secs(45)), Decision::Wait(Duration::from_secs(15)));
    }
}
