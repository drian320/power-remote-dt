//! Encode-side health state machine.
//!
//! Drives transitions Healthy → Degraded → Failing → Lost based on:
//!   - encode latency p95 EMA vs frame budget (Codex: 1.5× and 1.2×)
//!   - consecutive failure count
//!   - time since last successful frame
//!   - explicit `ProducerError::DeviceLost`
//!
//! Returns a `HealthAction` whenever a transition fires; `PolicyDriven`
//! carries out the action (reconfigure bitrate or swap backend).

use prdt_protocol::ProducerError;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Failing,
    Lost,
}

#[derive(Debug, Clone)]
pub enum FailoverReason {
    LatencyDegradation {
        encode_p95_us: u64,
        frame_budget_us: u64,
    },
    ConsecutiveFailures {
        count: u32,
    },
    NoSuccessTimeout {
        idle_ms: u64,
    },
    DeviceLost {
        backend: String,
        reason: String,
    },
}

#[derive(Debug)]
pub enum HealthAction {
    /// Stay on current backend; ask it to scale bitrate by `factor` (e.g.
    /// 0.85) and request an IDR. Triggered on Healthy → Degraded.
    ReconfigureBitrate { factor: f32 },
    /// Drop current backend; ask SelectionPolicy to pick a new one in the
    /// same codec. Triggered on Degraded → Failing or any → Lost.
    Failover { reason: FailoverReason },
}

#[derive(Debug)]
pub struct HealthMonitor {
    state: HealthState,
    encode_p95_ema: f64, // microseconds
    consecutive_failures: u32,
    last_success_at: Instant,
    frame_budget_us: u64,
    deg_threshold_factor: f64,      // default 1.5
    rec_threshold_factor: f64,      // default 1.2
    deg_window_count_required: u32, // default 3
    rec_window_count_required: u32, // default 5
    consecutive_deg_windows: u32,
    consecutive_rec_windows: u32,
    failure_threshold: u32,       // default 3
    no_success_timeout: Duration, // default 500ms
    /// Number of frames since last "window" boundary; window = 30 frames.
    frames_in_current_window: u32,
    window_size_frames: u32,
    /// EMA alpha. 1/(N+1) where N=window_size_frames.
    ema_alpha: f64,
}

impl HealthMonitor {
    pub fn new(target_fps: u32) -> Self {
        let frame_budget_us = (1_000_000_u64 / target_fps.max(1) as u64).max(1);
        let window_size_frames = 30;
        Self {
            state: HealthState::Healthy,
            encode_p95_ema: 0.0,
            consecutive_failures: 0,
            last_success_at: Instant::now(),
            frame_budget_us,
            deg_threshold_factor: 1.5,
            rec_threshold_factor: 1.2,
            deg_window_count_required: 3,
            rec_window_count_required: 5,
            consecutive_deg_windows: 0,
            consecutive_rec_windows: 0,
            failure_threshold: 3,
            no_success_timeout: Duration::from_millis(500),
            frames_in_current_window: 0,
            window_size_frames,
            ema_alpha: 1.0 / (window_size_frames as f64 + 1.0),
        }
    }

    pub fn current_state(&self) -> HealthState {
        self.state
    }
    pub fn encode_p95_ema(&self) -> u64 {
        self.encode_p95_ema as u64
    }
    pub fn frame_budget_us(&self) -> u64 {
        self.frame_budget_us
    }

    /// Reset state when a new backend takes over. Called by PolicyDriven
    /// after a successful failover swap.
    pub fn reset_for_new_backend(&mut self) {
        self.state = HealthState::Healthy;
        self.encode_p95_ema = 0.0;
        self.consecutive_failures = 0;
        self.last_success_at = Instant::now();
        self.consecutive_deg_windows = 0;
        self.consecutive_rec_windows = 0;
        self.frames_in_current_window = 0;
    }

    /// Record one successful encode. Returns an action if the state changed.
    pub fn record_encode(&mut self, encode_us: u64) -> Option<HealthAction> {
        self.consecutive_failures = 0;
        self.last_success_at = Instant::now();

        // EMA update
        let x = encode_us as f64;
        if self.encode_p95_ema == 0.0 {
            self.encode_p95_ema = x;
        } else {
            self.encode_p95_ema = self.ema_alpha * x + (1.0 - self.ema_alpha) * self.encode_p95_ema;
        }

        self.frames_in_current_window += 1;
        if self.frames_in_current_window < self.window_size_frames {
            return None;
        }
        // Window boundary: evaluate transition.
        self.frames_in_current_window = 0;

        let deg_thresh = self.frame_budget_us as f64 * self.deg_threshold_factor;
        let rec_thresh = self.frame_budget_us as f64 * self.rec_threshold_factor;

        if self.encode_p95_ema > deg_thresh {
            self.consecutive_deg_windows += 1;
            self.consecutive_rec_windows = 0;
        } else if self.encode_p95_ema < rec_thresh {
            self.consecutive_rec_windows += 1;
            self.consecutive_deg_windows = 0;
        } else {
            self.consecutive_deg_windows = 0;
            self.consecutive_rec_windows = 0;
        }

        match self.state {
            HealthState::Healthy => {
                if self.consecutive_deg_windows >= self.deg_window_count_required {
                    self.state = HealthState::Degraded;
                    self.consecutive_deg_windows = 0;
                    return Some(HealthAction::ReconfigureBitrate { factor: 0.85 });
                }
            }
            HealthState::Degraded => {
                if self.consecutive_rec_windows >= self.rec_window_count_required {
                    self.state = HealthState::Healthy;
                    self.consecutive_rec_windows = 0;
                    // Returning to Healthy is an info-level event, not an action.
                    return None;
                }
            }
            HealthState::Failing | HealthState::Lost => {
                // Stay in terminal-ish state until reset_for_new_backend().
            }
        }
        None
    }

    /// Record one error. Returns an action if the state changed.
    pub fn record_failure(&mut self, err: &ProducerError) -> Option<HealthAction> {
        // DeviceLost is immediate → Lost regardless of prior state.
        if let ProducerError::DeviceLost { backend, reason } = err {
            self.state = HealthState::Lost;
            return Some(HealthAction::Failover {
                reason: FailoverReason::DeviceLost {
                    backend: backend.clone(),
                    reason: reason.clone(),
                },
            });
        }

        self.consecutive_failures += 1;

        if self.consecutive_failures >= self.failure_threshold {
            // Promote to Failing → caller should swap backend.
            let count = self.consecutive_failures;
            self.state = HealthState::Failing;
            return Some(HealthAction::Failover {
                reason: FailoverReason::ConsecutiveFailures { count },
            });
        }

        // Check no-success timeout (only meaningful when we've been running
        // long enough for last_success_at to be old).
        let idle = self.last_success_at.elapsed();
        if idle > self.no_success_timeout && self.consecutive_failures > 0 {
            let idle_ms = idle.as_millis() as u64;
            self.state = HealthState::Failing;
            return Some(HealthAction::Failover {
                reason: FailoverReason::NoSuccessTimeout { idle_ms },
            });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget_60fps() -> u64 {
        1_000_000 / 60
    } // 16,666 us

    #[test]
    fn fresh_monitor_is_healthy() {
        let m = HealthMonitor::new(60);
        assert_eq!(m.current_state(), HealthState::Healthy);
        assert_eq!(m.frame_budget_us(), budget_60fps());
    }

    #[test]
    fn three_consecutive_overbudget_windows_trigger_degraded() {
        let mut m = HealthMonitor::new(60);
        // 30 frames per window × 3 windows of overbudget (e.g. 30ms = 30,000us).
        let mut last_action = None;
        for _ in 0..(30 * 3) {
            last_action = m.record_encode(30_000);
        }
        assert_eq!(m.current_state(), HealthState::Degraded);
        match last_action {
            Some(HealthAction::ReconfigureBitrate { factor }) => {
                assert!((factor - 0.85).abs() < 1e-6);
            }
            other => panic!("expected ReconfigureBitrate, got {:?}", other),
        }
    }

    #[test]
    fn five_consecutive_underbudget_windows_return_to_healthy() {
        let mut m = HealthMonitor::new(60);
        // First push to Degraded.
        for _ in 0..(30 * 3) {
            m.record_encode(30_000);
        }
        assert_eq!(m.current_state(), HealthState::Degraded);
        // Then 5 under-rec_threshold (1.2× budget = 20_000us; 10_000 is well under) windows.
        for _ in 0..(30 * 5) {
            m.record_encode(10_000);
        }
        assert_eq!(m.current_state(), HealthState::Healthy);
    }

    #[test]
    fn three_consecutive_failures_trigger_failing() {
        let mut m = HealthMonitor::new(60);
        let err = ProducerError::Encode("boom".into());
        let r1 = m.record_failure(&err);
        let r2 = m.record_failure(&err);
        let r3 = m.record_failure(&err);
        assert!(r1.is_none());
        assert!(r2.is_none());
        match r3 {
            Some(HealthAction::Failover {
                reason: FailoverReason::ConsecutiveFailures { count },
            }) => {
                assert_eq!(count, 3);
            }
            other => panic!("expected Failover ConsecutiveFailures, got {:?}", other),
        }
        assert_eq!(m.current_state(), HealthState::Failing);
    }

    #[test]
    fn device_lost_immediately_triggers_failover_lost() {
        let mut m = HealthMonitor::new(60);
        let err = ProducerError::DeviceLost {
            backend: "nvenc-h265".into(),
            reason: "DXGI_ERROR_DEVICE_REMOVED".into(),
        };
        let action = m.record_failure(&err);
        assert_eq!(m.current_state(), HealthState::Lost);
        match action {
            Some(HealthAction::Failover {
                reason: FailoverReason::DeviceLost { backend, reason },
            }) => {
                assert_eq!(backend, "nvenc-h265");
                assert_eq!(reason, "DXGI_ERROR_DEVICE_REMOVED");
            }
            other => panic!("expected Failover DeviceLost, got {:?}", other),
        }
    }

    #[test]
    fn reset_for_new_backend_clears_state() {
        let mut m = HealthMonitor::new(60);
        let err = ProducerError::DeviceLost {
            backend: "nvenc-h265".into(),
            reason: "x".into(),
        };
        m.record_failure(&err);
        assert_eq!(m.current_state(), HealthState::Lost);
        m.reset_for_new_backend();
        assert_eq!(m.current_state(), HealthState::Healthy);
    }

    #[test]
    fn successful_encode_resets_consecutive_failures() {
        let mut m = HealthMonitor::new(60);
        let err = ProducerError::Encode("transient".into());
        m.record_failure(&err);
        m.record_failure(&err);
        // Two failures, but a success in between would clear the counter.
        m.record_encode(5_000);
        let r3 = m.record_failure(&err);
        // Counter was reset to 0 by record_encode, so this is failure #1, not #3.
        assert!(r3.is_none(), "should not yet be Failing");
    }
}
