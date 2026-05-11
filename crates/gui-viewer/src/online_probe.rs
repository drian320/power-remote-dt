//! 30s background task that polls the signaling server for which saved hosts
//! are currently online. Runs only while the hosts_list view is open; cancelled
//! when the caller drops the returned `StopHandle` or calls `stop()`.
//!
//! TODO(P6 T8): wire result_sink into hosts_list rendering.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use tokio::time::{interval, Duration};
use url::Url;

/// Drop this (or call [`StopHandle::stop`]) to cancel the background poll.
pub struct StopHandle {
    tx: watch::Sender<bool>,
}

impl StopHandle {
    /// Signal the background task to stop. Equivalent to dropping the handle.
    pub fn stop(self) {
        let _ = self.tx.send(false);
    }
}

impl Drop for StopHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(false);
    }
}

/// Spawns a tokio task that polls the signaling server every 30 seconds.
///
/// # Arguments
/// * `signaling_url` – URL of the signaling server WebSocket endpoint.
/// * `host_ids` – shared list of host IDs to probe (read on each tick).
/// * `result_sink` – written on each tick: `host_id → true` if online.
///
/// Returns a [`StopHandle`]; dropping it cancels the background task.
pub fn spawn(
    signaling_url: Url,
    host_ids: Arc<Mutex<Vec<String>>>,
    result_sink: Arc<Mutex<HashMap<String, bool>>>,
) -> StopHandle {
    let (tx, mut rx) = watch::channel(true);

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(30));
        // MissedTickBehavior::Delay avoids burst catch-up probes if the task
        // wakes late (e.g. system sleep). The first tick resolves immediately
        // (tokio interval default), so the initial probe fires without delay.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = rx.changed() => {
                    if !*rx.borrow() { break; }
                }
                _ = ticker.tick() => {
                    let ids: Vec<String> = host_ids
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone();

                    if ids.is_empty() {
                        continue;
                    }

                    match prdt_signaling_client::probe_hosts(&signaling_url, ids.clone()).await {
                        Ok(online) => {
                            let mut sink = result_sink
                                .lock()
                                .unwrap_or_else(|p| p.into_inner());
                            for id in &ids {
                                sink.insert(id.clone(), online.contains(id));
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "online probe failed");
                        }
                    }
                }
            }
        }
    });

    StopHandle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_handle_drop_does_not_panic_with_dead_receiver() {
        // Simulate the spawned task having already exited (receiver dropped).
        // The `let _ =` in Drop must not panic even with no receiver.
        let (tx, rx) = watch::channel(true);
        drop(rx);
        let handle = StopHandle { tx };
        drop(handle); // must not panic
    }
}
