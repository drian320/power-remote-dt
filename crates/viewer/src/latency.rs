//! Per-frame M1 latency instrumentation for the viewer.
//!
//! Records timestamped events for each frame and emits periodic p50/p95/p99
//! reports. All timestamps come from `prdt_protocol::now_monotonic_us`, which
//! is a *per-process* monotonic epoch.
//!
//! The `decode` and `present` stages are measured viewer-locally: from the
//! frame's own `recv_us` (recv → decode, recv → present). These are correct
//! regardless of clock sync and are always non-zero once the pipeline runs.
//!
//! The `arrival` stage (`recv_us - host_ts_us`) is the one cross-clock delta.
//! On in-process loopback (the M2 scenario) the host and viewer share one
//! epoch, so it is accurate. On a cross-machine session the host's capture
//! epoch is unrelated to the viewer's — and because the host process has
//! usually been alive longer, `host_ts_us` exceeds the viewer's `now`, so the
//! subtraction saturates to 0. Turning `arrival` (and a true host_capture →
//! present glass-to-glass delta) into wall-clock numbers needs the Ping/Pong
//! clock-offset correction deferred to Plan 4 M3.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use prdt_protocol::now_monotonic_us;

/// How many samples to retain for p50/p95/p99 rolling stats.
const SAMPLE_WINDOW: usize = 240;

#[derive(Default)]
struct Inner {
    /// Per-frame record of stage timestamps, keyed by frame_seq.
    frames: HashMap<u64, FrameStages>,
    /// Rolling `recv_us - host_ts_us` samples (cross-clock; accurate only when
    /// host and viewer share an epoch, i.e. in-process loopback — see module
    /// docs). Saturates to 0 across machines until M3 clock-offset correction.
    arrival_lag_samples: VecDeque<u64>,
    /// Rolling `decode_done_us - recv_us` samples (viewer-local recv → decode).
    decode_done_samples: VecDeque<u64>,
    /// Rolling `present_us - recv_us` samples (viewer-local recv → present).
    present_samples: VecDeque<u64>,
}

#[derive(Default, Clone, Copy)]
struct FrameStages {
    host_ts_us: u64,
    recv_us: Option<u64>,
    /// Written for completeness/debugging; the decode latency is derived from
    /// `recv_us` at record time, so this field itself is not read back.
    #[allow(dead_code)]
    decode_done_us: Option<u64>,
}

/// Thread-safe latency probe. One instance is shared across the recv task
/// and the winit main thread via `Arc`.
pub struct LatencyProbe {
    inner: Mutex<Inner>,
}

impl Default for LatencyProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyProbe {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Record that frame `seq` arrived from the network. `host_ts_us` is the
    /// host-side capture timestamp carried in the frame.
    pub fn record_recv(&self, seq: u64, host_ts_us: u64) {
        let now = now_monotonic_us();
        let mut g = self.inner.lock().unwrap();
        let stages = g.frames.entry(seq).or_default();
        stages.host_ts_us = host_ts_us;
        stages.recv_us = Some(now);
        let lag = now.saturating_sub(host_ts_us);
        push_capped(&mut g.arrival_lag_samples, lag, SAMPLE_WINDOW);
    }

    /// Record that the decoder produced a texture for frame `seq`.
    pub fn record_decoded(&self, seq: u64) {
        let now = now_monotonic_us();
        let mut g = self.inner.lock().unwrap();
        let Some(stages) = g.frames.get_mut(&seq) else {
            return;
        };
        stages.decode_done_us = Some(now);
        // Measure decode latency on the viewer's OWN clock (recv → decode),
        // not against `host_ts_us`. The host capture timestamp lives on the
        // host process's monotonic epoch; on a cross-machine session that
        // epoch is unrelated to the viewer's, and (because the host process
        // has usually been alive far longer) `host_ts_us` exceeds the
        // viewer's `now`, so `now - host_ts_us` saturates to 0. Anchoring at
        // `recv_us` yields a real, non-zero viewer-local latency regardless
        // of clock sync.
        let recv_us = stages.recv_us;
        let lag = recv_us.map(|r| now.saturating_sub(r)).unwrap_or(0);
        push_capped(&mut g.decode_done_samples, lag, SAMPLE_WINDOW);
    }

    /// Record that the swapchain presented the frame tied to `host_ts_us`.
    /// Also prunes the per-frame map so old entries don't accumulate.
    pub fn record_present_for_host_ts(&self, host_ts_us: u64) {
        if host_ts_us == 0 {
            return;
        }
        let now = now_monotonic_us();
        let mut g = self.inner.lock().unwrap();
        // Measure present latency on the viewer's OWN clock (recv → present).
        // The render thread only knows the frame by its host capture stamp, so
        // join back to the frame's `recv_us` via `host_ts_us`. Subtracting the
        // host stamp directly would saturate to 0 across machines (see
        // `record_decoded`); anchoring at `recv_us` gives a real viewer-local
        // number. The full glass-to-glass (host_capture → present) delta needs
        // the Ping/Pong clock-offset correction deferred to Plan 4 M3.
        let recv_us = g
            .frames
            .values()
            .find(|s| s.host_ts_us == host_ts_us)
            .and_then(|s| s.recv_us);
        if let Some(recv_us) = recv_us {
            let lag = now.saturating_sub(recv_us);
            push_capped(&mut g.present_samples, lag, SAMPLE_WINDOW);
        }
        // Drop any entries whose host_ts is older than the one we just
        // presented — they're past-and-done.
        g.frames.retain(|_, s| s.host_ts_us > host_ts_us);
    }

    /// Snapshot the current rolling p50/p95/p99 for every stage. Returns
    /// None for stages that haven't collected any samples yet.
    pub fn snapshot(&self) -> LatencySnapshot {
        let g = self.inner.lock().unwrap();
        LatencySnapshot {
            arrival: stats(&g.arrival_lag_samples),
            decode_done: stats(&g.decode_done_samples),
            present: stats(&g.present_samples),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StageStats {
    pub samples: usize,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct LatencySnapshot {
    pub arrival: Option<StageStats>,
    pub decode_done: Option<StageStats>,
    pub present: Option<StageStats>,
}

fn push_capped(q: &mut VecDeque<u64>, v: u64, cap: usize) {
    if q.len() == cap {
        q.pop_front();
    }
    q.push_back(v);
}

fn stats(samples: &VecDeque<u64>) -> Option<StageStats> {
    if samples.is_empty() {
        return None;
    }
    let mut v: Vec<u64> = samples.iter().copied().collect();
    v.sort_unstable();
    let pick = |p: f64| -> u64 {
        let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
        v[idx]
    };
    Some(StageStats {
        samples: v.len(),
        p50_us: pick(0.50),
        p95_us: pick(0.95),
        p99_us: pick(0.99),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_returns_none_until_samples_recorded() {
        let probe = LatencyProbe::new();
        let snap = probe.snapshot();
        assert!(snap.arrival.is_none());
        assert!(snap.decode_done.is_none());
        assert!(snap.present.is_none());
    }

    #[test]
    fn percentiles_progress_as_samples_arrive() {
        let probe = LatencyProbe::new();
        // Record 10 frames with increasing host_ts (so arrival lag decreases
        // as host_ts approaches "now").
        let base = now_monotonic_us().saturating_add(1_000_000);
        for i in 0..10 {
            probe.record_recv(i, base.saturating_sub((10 - i) * 1_000));
        }
        let snap = probe.snapshot();
        let a = snap.arrival.expect("some samples");
        assert_eq!(a.samples, 10);
        assert!(a.p50_us >= a.p50_us.min(a.p95_us));
        assert!(a.p95_us <= a.p99_us);
    }

    #[test]
    fn rolling_window_caps_at_sample_window() {
        let probe = LatencyProbe::new();
        for i in 0..(SAMPLE_WINDOW as u64 + 50) {
            probe.record_recv(i, now_monotonic_us());
        }
        let a = probe.snapshot().arrival.unwrap();
        assert_eq!(a.samples, SAMPLE_WINDOW);
    }

    #[test]
    fn present_prunes_older_frame_entries() {
        let probe = LatencyProbe::new();
        // Anchor above any possible real `now_monotonic_us()` value so the
        // subtractions below can't underflow when the process epoch is
        // very fresh.
        let base = now_monotonic_us().saturating_add(1_000_000);
        probe.record_recv(1, base - 5_000);
        probe.record_recv(2, base - 3_000);
        probe.record_recv(3, base - 1_000);
        // Present frame 2's host_ts → should prune frames 1 and 2.
        probe.record_present_for_host_ts(base - 3_000);
        let g = probe.inner.lock().unwrap();
        assert!(!g.frames.contains_key(&1));
        assert!(!g.frames.contains_key(&2));
        assert!(g.frames.contains_key(&3));
    }

    #[test]
    fn decode_done_without_prior_recv_is_noop() {
        let probe = LatencyProbe::new();
        probe.record_decoded(999); // no record_recv for seq 999
        assert!(probe.snapshot().decode_done.is_none());
    }

    #[test]
    fn viewer_local_stages_nonzero_despite_unsynced_host_clock() {
        // Regression: field logs showed present/decode percentiles all 0µs
        // because the stages were measured against `host_ts_us`, which on a
        // cross-machine session sits on the host process's epoch and (the host
        // having run longer) exceeds the viewer's `now`, saturating every
        // delta to 0. Simulate that skew with a host stamp far in the future
        // and confirm the recv-anchored decode/present stages are non-zero.
        let probe = LatencyProbe::new();
        let host_ts = now_monotonic_us().saturating_add(10_000_000); // 10s ahead
        probe.record_recv(7, host_ts);
        // Real viewer-local time must elapse before decode + present.
        std::thread::sleep(std::time::Duration::from_millis(2));
        probe.record_decoded(7);
        probe.record_present_for_host_ts(host_ts);

        let snap = probe.snapshot();
        let d = snap.decode_done.expect("decode sample recorded");
        let p = snap.present.expect("present sample recorded");
        assert!(
            d.p50_us > 0,
            "decode latency must be viewer-local non-zero, got {}",
            d.p50_us
        );
        assert!(
            p.p50_us > 0,
            "present latency must be viewer-local non-zero, got {}",
            p.p50_us
        );
        // The cross-clock arrival stage still saturates to 0 under this skew
        // (documented; needs M3 clock-offset correction).
        assert_eq!(snap.arrival.map(|s| s.p50_us), Some(0));
    }
}
