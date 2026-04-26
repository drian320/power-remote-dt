//! Heartbeat watchdog for host session liveness.
//!
//! Spawned once per session by `main.rs`. Polls `last_keepalive` every
//! second and fires the supplied `CancellationToken` if no `KeepAlive`
//! has arrived for `KEEPALIVE_TIMEOUT`. The control task elsewhere is
//! responsible for storing a fresh timestamp into `last_keepalive` on
//! each `ControlMessage::KeepAlive` receipt.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use prdt_transport::now_monotonic_us;

/// Threshold beyond which the viewer is considered dead.
pub const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Watchdog tick cadence.
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Spawn the watchdog. Cancels `cancel` when no KeepAlive has been
/// observed for `KEEPALIVE_TIMEOUT`.
///
/// # Ordering contract
///
/// Writers (the control task that handles `ControlMessage::KeepAlive`)
/// MUST store with `Ordering::Relaxed` to match the watchdog's read
/// ordering. There is no other shared state synchronized through this
/// atomic, so `Relaxed` is sufficient — we only require that fresh
/// stores eventually become visible to the polling watchdog.
pub fn spawn_watchdog(cancel: CancellationToken, last_keepalive: Arc<AtomicU64>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let last = last_keepalive.load(Ordering::Relaxed);
                    let now = now_monotonic_us();
                    let silence_us = now.saturating_sub(last);
                    if silence_us > KEEPALIVE_TIMEOUT.as_micros() as u64 {
                        warn!(
                            silence_us,
                            "viewer silent > {}s; canceling session",
                            KEEPALIVE_TIMEOUT.as_secs(),
                        );
                        cancel.cancel();
                        break;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn watchdog_fires_on_silence() {
        let cancel = CancellationToken::new();
        // `now_monotonic_us` uses `std::time::Instant` (real wall clock), which
        // is NOT affected by tokio's paused virtual clock. Initialize
        // `last_keepalive` to 0 so that `now - 0` is always >> KEEPALIVE_TIMEOUT
        // (process has been running at least a few ms by test time). The watchdog
        // fires on its very first tick after we advance virtual time past 1 s.
        let last_ka = Arc::new(AtomicU64::new(0u64));
        let handle = spawn_watchdog(cancel.clone(), Arc::clone(&last_ka));

        // Advance virtual time past the first 1-second tick interval so the
        // interval future wakes up.
        tokio::time::advance(Duration::from_secs(2)).await;

        // Await the handle directly: the watchdog task breaks out of its loop
        // immediately after calling cancel.cancel(), so joining it proves the
        // task ran and cancelled before we assert.
        handle.await.unwrap();

        // The test's last_keepalive=0 sentinel relies on the real
        // monotonic clock having advanced past KEEPALIVE_TIMEOUT by the
        // time the watchdog ticks. If the clock is ever replaced with a
        // virtual one, this guard fails loudly instead of silently
        // making the test vacuous.
        assert!(
            now_monotonic_us() > KEEPALIVE_TIMEOUT.as_micros() as u64,
            "process epoch too young for last_keepalive=0 sentinel; \
             switch to a deterministic clock injection if this fires",
        );
        assert!(cancel.is_cancelled(), "watchdog should have cancelled");
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_quiet_with_recent_keepalive() {
        let cancel = CancellationToken::new();
        let last_ka = Arc::new(AtomicU64::new(now_monotonic_us()));
        let handle = spawn_watchdog(cancel.clone(), Arc::clone(&last_ka));

        // Simulate 10 keepalives at 900ms cadence. Each one refreshes
        // last_ka so the watchdog never sees more than 1s of silence.
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(900)).await;
            last_ka.store(now_monotonic_us(), Ordering::Relaxed);
            // Yield so the watchdog task gets one scheduler turn to poll
            // its ticker between our store + advance steps. Without this
            // the watchdog might not observe the heartbeat refresh on
            // time. (Single-threaded current_thread runtime — yield is
            // sufficient.)
            tokio::task::yield_now().await;
        }

        assert!(
            !cancel.is_cancelled(),
            "watchdog must not fire while heartbeat present"
        );
        // Manual cleanup so the JoinHandle resolves.
        cancel.cancel();
        handle.await.unwrap();
    }
}
