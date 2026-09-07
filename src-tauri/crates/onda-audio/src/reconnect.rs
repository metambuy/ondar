//! Reconnect policy for dropped streams. Exponential backoff, capped attempts, and a reset
//! once playback has been stable long enough that the next drop should count as a new
//! incident rather than a continuation of the last one.

use std::time::Duration;

pub const MAX_ATTEMPTS: u32 = 5;
const DELAYS_SECS: [u64; MAX_ATTEMPTS as usize] = [1, 2, 4, 8, 16];
/// Playing continuously for this long resets the attempt counter.
pub const STABLE_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Clone)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    /// Returns the delay before the next attempt, or `None` when attempts are exhausted.
    /// The returned attempt number is 1-based, for display.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<(u32, Duration)> {
        if self.attempt >= MAX_ATTEMPTS {
            return None;
        }
        let delay = Duration::from_secs(DELAYS_SECS[self.attempt as usize]);
        self.attempt += 1;
        Some((self.attempt, delay))
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_attempts_then_gives_up() {
        let mut b = Backoff::default();
        let delays: Vec<u64> = std::iter::from_fn(|| b.next())
            .map(|(_, d)| d.as_secs())
            .collect();
        assert_eq!(delays, vec![1, 2, 4, 8, 16]);
        assert!(b.next().is_none());
        b.reset();
        assert_eq!(b.next().map(|(a, _)| a), Some(1));
    }
}
