//! Selection policy: hard filter + soft score → ranked candidate list.
//!
//! The policy is deterministic given (candidates, context, history). All
//! mutable state lives in `HistoryTable`; the policy itself is `&self`.

use crate::capability::{BackendKind, Codec, EncoderCapability};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct PolicyContext {
    pub target_resolution: (u32, u32),
    pub target_fps: u32,
    pub target_bitrate_bps: u32,
    pub codec: Codec,
    /// Strict override: if set, only this backend is considered, no failover.
    pub user_override: Option<BackendKind>,
    /// Soft hint: +0.5 score bump, failover still allowed.
    pub user_hint: Option<BackendKind>,
    /// Equivalent to `user_override = Some(Openh264)` for the filter; left as
    /// a separate flag so CLI semantics are clear.
    pub force_sw: bool,
}

#[derive(Debug, Default, Clone)]
pub struct BackendStats {
    pub successes: u32,
    pub failures: u32,
    pub last_failure_at: Option<Instant>,
    pub cooldown_until: Option<Instant>,
    /// Snapshot of HealthMonitor's encode p95 EMA, in microseconds.
    /// `None` ⇒ never run on this backend (cold start).
    pub recent_encode_p95_us: Option<u64>,
}

#[derive(Debug, Default)]
pub struct HistoryTable {
    counts: HashMap<BackendKind, BackendStats>,
}

impl HistoryTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self, backend: BackendKind) -> BackendStats {
        self.counts.get(&backend).cloned().unwrap_or_default()
    }

    pub fn successes(&self, backend: BackendKind) -> u32 {
        self.counts.get(&backend).map(|s| s.successes).unwrap_or(0)
    }
    pub fn failures(&self, backend: BackendKind) -> u32 {
        self.counts.get(&backend).map(|s| s.failures).unwrap_or(0)
    }
    pub fn recent_encode_p95_us(&self, backend: BackendKind) -> Option<u64> {
        self.counts
            .get(&backend)
            .and_then(|s| s.recent_encode_p95_us)
    }
    pub fn cooldown_remaining(&self, backend: BackendKind, now: Instant) -> Duration {
        self.counts
            .get(&backend)
            .and_then(|s| s.cooldown_until)
            .map(|t| t.saturating_duration_since(now))
            .unwrap_or(Duration::ZERO)
    }

    pub fn record_success(&mut self, backend: BackendKind) {
        self.counts.entry(backend).or_default().successes += 1;
    }
    pub fn record_failure(&mut self, backend: BackendKind, now: Instant) {
        let s = self.counts.entry(backend).or_default();
        // Read previous cooldown LENGTH before mutating last_failure_at.
        // Sequence: failure → cooldown of duration D from that failure → next failure
        // doubles D. Cap at 300s.
        let prev = match (s.cooldown_until, s.last_failure_at) {
            (Some(c), Some(prev_failure)) => c
                .checked_duration_since(prev_failure)
                .unwrap_or(Duration::from_secs(5)),
            _ => Duration::from_secs(5),
        };
        s.failures += 1;
        s.last_failure_at = Some(now);
        let next = (prev * 2).min(Duration::from_secs(300));
        s.cooldown_until = Some(now + next.max(Duration::from_secs(10)));
    }
    pub fn update_encode_p95(&mut self, backend: BackendKind, p95_us: u64) {
        self.counts.entry(backend).or_default().recent_encode_p95_us = Some(p95_us);
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScoringWeights {
    pub priority: f64,
    pub zero_copy: f64,
    pub latency_fit: f64,
    pub reliability: f64,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            priority: 0.45,
            zero_copy: 0.20,
            latency_fit: 0.25,
            reliability: 0.10,
        }
    }
}

pub trait SelectionPolicy: Send + Sync {
    fn rank(
        &self,
        candidates: &[EncoderCapability],
        ctx: &PolicyContext,
        history: &HistoryTable,
    ) -> Vec<BackendKind>;
}

pub struct ScoringPolicy {
    pub weights: ScoringWeights,
}

impl ScoringPolicy {
    pub fn new(weights: ScoringWeights) -> Self {
        Self { weights }
    }

    /// Reads `dirs::config_dir()/prdt/policy.toml` if present; falls back to
    /// defaults on any read/parse error. No CLI flag override in P5A.
    pub fn load_default_or_fallback() -> Self {
        let Some(path) = dirs::config_dir().map(|d| d.join("prdt").join("policy.toml")) else {
            return Self {
                weights: ScoringWeights::default(),
            };
        };
        let weights = match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str::<ScoringWeights>(&s) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "policy.toml parse failed; using default weights"
                    );
                    ScoringWeights::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ScoringWeights::default(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "policy.toml read failed; using default weights"
                );
                ScoringWeights::default()
            }
        };
        Self { weights }
    }
}

fn beta_posterior(s: u32, f: u32) -> f64 {
    // Beta(1,1) prior smoothing; cold start ⇒ 0.5
    (s as f64 + 1.0) / (s as f64 + f as f64 + 2.0)
}

/// Stable discriminant for tie-breaking in sort; keeps ordering deterministic.
fn backend_discriminant(b: BackendKind) -> u8 {
    match b {
        BackendKind::Nvenc => 0,
        BackendKind::MfHevc => 1,
        BackendKind::Openh264 => 2,
    }
}

impl SelectionPolicy for ScoringPolicy {
    fn rank(
        &self,
        candidates: &[EncoderCapability],
        ctx: &PolicyContext,
        history: &HistoryTable,
    ) -> Vec<BackendKind> {
        let now = Instant::now();
        let frame_budget_us = (1_000_000_u64 / ctx.target_fps.max(1) as u64).max(1);

        // 1. Hard filter
        let mut filtered: Vec<&EncoderCapability> = candidates
            .iter()
            .filter(|cap| {
                cap.codec == ctx.codec
                    && cap.max_resolution.0 >= ctx.target_resolution.0
                    && cap.max_resolution.1 >= ctx.target_resolution.1
                    && cap.max_fps >= ctx.target_fps
                    && (!ctx.force_sw || matches!(cap.backend, BackendKind::Openh264))
                    && history.cooldown_remaining(cap.backend, now).is_zero()
            })
            .collect();

        // 2. user_override = Strict mode: only that backend, if it survived the filter.
        if let Some(forced) = ctx.user_override {
            filtered.retain(|c| c.backend == forced);
            return filtered.into_iter().map(|c| c.backend).collect();
        }

        // 3. Soft score
        let w = &self.weights;
        let mut scored: Vec<(BackendKind, f64)> = filtered
            .iter()
            .map(|cap| {
                let priority_norm = (cap.priority as f64 / 100.0).clamp(0.0, 1.0);
                let zero_copy_bonus = if cap.zero_copy { 1.0 } else { 0.0 };
                let runtime_p95_us = history
                    .recent_encode_p95_us(cap.backend)
                    .unwrap_or(frame_budget_us / 2) as f64;
                let latency_fit = (frame_budget_us as f64 / runtime_p95_us.max(1.0)).min(1.0);
                let reliability = beta_posterior(
                    history.successes(cap.backend),
                    history.failures(cap.backend),
                );
                let mut score = w.priority * priority_norm
                    + w.zero_copy * zero_copy_bonus
                    + w.latency_fit * latency_fit
                    + w.reliability * reliability;
                if Some(cap.backend) == ctx.user_hint {
                    score += 0.5; // soft hint bump
                }
                (cap.backend, score)
            })
            .collect();

        // Stable sort: descending by score, tie-break by BackendKind discriminant
        // (explicit mapping keeps ordering deterministic regardless of enum repr).
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| backend_discriminant(a.0).cmp(&backend_discriminant(b.0)))
        });
        scored.into_iter().map(|(k, _)| k).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(backend: BackendKind, codec: Codec, prio: i32, zc: bool) -> EncoderCapability {
        EncoderCapability {
            backend,
            codec,
            max_resolution: (3840, 2160),
            max_fps: 60,
            zero_copy: zc,
            priority: prio,
        }
    }

    fn ctx_h265_1080p60() -> PolicyContext {
        PolicyContext {
            target_resolution: (1920, 1080),
            target_fps: 60,
            target_bitrate_bps: 8_000_000,
            codec: Codec::H265,
            user_override: None,
            user_hint: None,
            force_sw: false,
        }
    }

    #[test]
    fn rank_prefers_high_priority_zero_copy_backend() {
        let candidates = vec![
            cap(BackendKind::Nvenc, Codec::H265, 100, true),
            cap(BackendKind::MfHevc, Codec::H265, 80, true),
            cap(BackendKind::Openh264, Codec::H265, 10, false),
        ];
        let p = ScoringPolicy::new(ScoringWeights::default());
        let ranked = p.rank(&candidates, &ctx_h265_1080p60(), &HistoryTable::new());
        assert_eq!(ranked[0], BackendKind::Nvenc);
        assert_eq!(ranked[1], BackendKind::MfHevc);
        assert_eq!(ranked[2], BackendKind::Openh264);
    }

    #[test]
    fn rank_filters_codec_mismatch() {
        let candidates = vec![
            cap(BackendKind::Nvenc, Codec::H264, 100, true), // wrong codec
            cap(BackendKind::Openh264, Codec::H265, 10, false),
        ];
        let p = ScoringPolicy::new(ScoringWeights::default());
        let ranked = p.rank(&candidates, &ctx_h265_1080p60(), &HistoryTable::new());
        assert_eq!(ranked, vec![BackendKind::Openh264]);
    }

    #[test]
    fn rank_force_sw_keeps_only_openh264() {
        let candidates = vec![
            cap(BackendKind::Nvenc, Codec::H265, 100, true),
            cap(BackendKind::MfHevc, Codec::H265, 80, true),
            cap(BackendKind::Openh264, Codec::H265, 10, false),
        ];
        let p = ScoringPolicy::new(ScoringWeights::default());
        let mut ctx = ctx_h265_1080p60();
        ctx.force_sw = true;
        let ranked = p.rank(&candidates, &ctx, &HistoryTable::new());
        assert_eq!(ranked, vec![BackendKind::Openh264]);
    }

    #[test]
    fn rank_user_override_strict_returns_only_that_backend() {
        let candidates = vec![
            cap(BackendKind::Nvenc, Codec::H265, 100, true),
            cap(BackendKind::MfHevc, Codec::H265, 80, true),
            cap(BackendKind::Openh264, Codec::H265, 10, false),
        ];
        let p = ScoringPolicy::new(ScoringWeights::default());
        let mut ctx = ctx_h265_1080p60();
        ctx.user_override = Some(BackendKind::MfHevc);
        let ranked = p.rank(&candidates, &ctx, &HistoryTable::new());
        assert_eq!(ranked, vec![BackendKind::MfHevc]);
    }

    #[test]
    fn rank_user_hint_promotes_chosen_backend_above_higher_priority() {
        // MfHevc (priority 80, zero_copy=false) gets a +0.5 hint bump; should
        // beat NVENC (priority 100, zero_copy=false).
        //
        // Scores without hint (weights: priority=0.45, zero_copy=0.20,
        //   latency_fit=0.25, reliability=0.10):
        //   NVENC:  0.45*1.0 + 0.20*0 + 0.25*1.0 + 0.10*0.5 = 0.75
        //   MfHevc: 0.45*0.8 + 0.20*0 + 0.25*1.0 + 0.10*0.5 = 0.66
        // With +0.5 bump on MfHevc: 0.66 + 0.5 = 1.16 > 0.75 ✓
        let candidates = vec![
            cap(BackendKind::Nvenc, Codec::H265, 100, false),
            cap(BackendKind::MfHevc, Codec::H265, 80, false),
        ];
        let p = ScoringPolicy::new(ScoringWeights::default());
        let mut ctx = ctx_h265_1080p60();
        ctx.user_hint = Some(BackendKind::MfHevc);
        let ranked = p.rank(&candidates, &ctx, &HistoryTable::new());
        assert_eq!(ranked[0], BackendKind::MfHevc);
        assert_eq!(ranked[1], BackendKind::Nvenc);
    }

    #[test]
    fn cooldown_excludes_recently_failed_backend() {
        let candidates = vec![
            cap(BackendKind::Nvenc, Codec::H265, 100, true),
            cap(BackendKind::Openh264, Codec::H265, 10, false),
        ];
        let mut history = HistoryTable::new();
        history.record_failure(BackendKind::Nvenc, Instant::now());
        let p = ScoringPolicy::new(ScoringWeights::default());
        let ranked = p.rank(&candidates, &ctx_h265_1080p60(), &history);
        assert_eq!(ranked, vec![BackendKind::Openh264]);
    }

    #[test]
    fn record_failure_doubles_cooldown_on_repeat() {
        let mut h = HistoryTable::new();
        let t0 = Instant::now();
        // First failure: cooldown should be 10s (5s default × 2 = 10s, clamped to min 10s).
        h.record_failure(BackendKind::Nvenc, t0);
        let cd1 = h.stats(BackendKind::Nvenc).cooldown_until.unwrap();
        let dur1 = cd1.duration_since(t0);
        assert_eq!(
            dur1,
            Duration::from_secs(10),
            "first cooldown should be 10s"
        );

        // Second failure AT cooldown end (so prev = 10s exactly): next = 20s.
        let t1 = cd1; // exactly when first cooldown expires
        h.record_failure(BackendKind::Nvenc, t1);
        let cd2 = h.stats(BackendKind::Nvenc).cooldown_until.unwrap();
        let dur2 = cd2.duration_since(t1);
        assert_eq!(
            dur2,
            Duration::from_secs(20),
            "second cooldown should double to 20s"
        );

        // Third failure: next = 40s.
        let t2 = cd2;
        h.record_failure(BackendKind::Nvenc, t2);
        let cd3 = h.stats(BackendKind::Nvenc).cooldown_until.unwrap();
        let dur3 = cd3.duration_since(t2);
        assert_eq!(
            dur3,
            Duration::from_secs(40),
            "third cooldown should double to 40s"
        );
    }

    #[test]
    fn beta_posterior_cold_start_is_half() {
        assert!((beta_posterior(0, 0) - 0.5).abs() < 1e-9);
    }

    proptest::proptest! {
        /// Property: reversing the input candidate list does not change the
        /// rank output. A focused regression for the deterministic tie-break
        /// — a fuller permutation test would require a proper shuffle strategy
        /// (deferred; see follow-up).
        #[test]
        fn rank_reversed_input_yields_same_ordering(
            seed in 0u64..1000,
        ) {
            use proptest::prelude::*;
            let _ = seed;
            let mut candidates = vec![
                cap(BackendKind::Nvenc, Codec::H265, 100, true),
                cap(BackendKind::MfHevc, Codec::H265, 80, true),
                cap(BackendKind::Openh264, Codec::H265, 10, false),
            ];
            let p = ScoringPolicy::new(ScoringWeights::default());
            let baseline = p.rank(&candidates, &ctx_h265_1080p60(), &HistoryTable::new());
            candidates.reverse();
            let reversed = p.rank(&candidates, &ctx_h265_1080p60(), &HistoryTable::new());
            prop_assert_eq!(baseline, reversed);
        }
    }
}
