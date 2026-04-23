//! Process-wide monotonic clock, in microseconds.
//!
//! Using a single shared epoch means producers (media-win), transport, and
//! viewer-side probes all emit timestamps on the same timeline. Critical for
//! in-process loopback latency measurement (M1/M2) where we want to compare
//! a capture timestamp from the producer with a present timestamp in the
//! viewer without going through a cross-process clock-offset estimate.

use std::sync::OnceLock;
use std::time::Instant;

/// Monotonic clock reading in microseconds since the first call (process-wide).
pub fn now_monotonic_us() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_monotonic_is_nondecreasing() {
        let a = now_monotonic_us();
        let b = now_monotonic_us();
        assert!(b >= a, "monotonic clock went backwards: {a} → {b}");
    }

    #[test]
    fn now_monotonic_advances() {
        let a = now_monotonic_us();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = now_monotonic_us();
        assert!(b > a, "no advance between 2ms sleeps: {a} → {b}");
    }
}
