//! Adaptive bitrate controller (viewer-side AIMD).
//!
//! Observes `purge_assembler()` frame loss and `LatencyProbe::snapshot()`
//! totals at 1 Hz, computes a target bitrate via Additive-Increase /
//! Multiplicative-Decrease (TCP NewReno-style), and tells the caller via
//! `should_send()` when a `ControlMessage::SetBitrate` is worth sending
//! to the host. See `docs/superpowers/specs/2026-05-11-l3-adaptive-bitrate-design.md`
//! for parameter rationale.

use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct BitrateControllerConfig {
    pub initial_bps: u32,
    pub min_bps: u32,
    pub max_bps: u32,
    pub loss_high: f32,
    pub loss_low: f32,
    pub md_factor: f32,
    pub ai_step_bps: u32,
    pub send_threshold_pct: f32,
    pub cooldown_after_md: Duration,
    pub enabled: bool,
}

impl BitrateControllerConfig {
    pub fn new_for_max(max_bps: u32) -> Self {
        Self {
            initial_bps: max_bps,
            min_bps: 1_000_000,
            max_bps,
            loss_high: 0.02,
            loss_low: 0.005,
            md_factor: 0.7,
            ai_step_bps: 200_000,
            send_threshold_pct: 0.05,
            cooldown_after_md: Duration::from_secs(2),
            enabled: true,
        }
    }
}

pub struct BitrateController {
    cfg: BitrateControllerConfig,
    target_bps: u32,
    last_md_at: Option<Instant>,
    last_sent_bps: u32,
    rolling_lost: u64,
    rolling_total: u64,
}

impl BitrateController {
    pub fn new(cfg: BitrateControllerConfig) -> Self {
        let target = cfg.initial_bps.clamp(cfg.min_bps, cfg.max_bps);
        Self {
            target_bps: target,
            last_sent_bps: target,
            cfg,
            last_md_at: None,
            rolling_lost: 0,
            rolling_total: 0,
        }
    }

    pub fn observe(&mut self, lost: u64, total: u64) {
        self.rolling_lost = self.rolling_lost.saturating_add(lost);
        self.rolling_total = self.rolling_total.saturating_add(total);
    }

    pub fn aimd_step(&mut self, now: Instant) {
        if !self.cfg.enabled {
            self.target_bps = self.cfg.max_bps;
            return;
        }
        let total = self.rolling_total.max(1);
        let loss = (self.rolling_lost as f32) / (total as f32);
        if loss > self.cfg.loss_high {
            let next = ((self.target_bps as f32) * self.cfg.md_factor) as u32;
            self.target_bps = next.max(self.cfg.min_bps);
            self.last_md_at = Some(now);
        } else if loss < self.cfg.loss_low {
            let cooldown_ok = match self.last_md_at {
                None => true,
                Some(t) => now.saturating_duration_since(t) >= self.cfg.cooldown_after_md,
            };
            if cooldown_ok {
                let next = self.target_bps.saturating_add(self.cfg.ai_step_bps);
                self.target_bps = next.min(self.cfg.max_bps);
            }
        }
    }

    pub fn target_bps(&self) -> u32 {
        self.target_bps
    }

    pub fn should_send(&self) -> bool {
        if !self.cfg.enabled {
            return false;
        }
        let last = self.last_sent_bps.max(1) as f32;
        let curr = self.target_bps as f32;
        let delta = (curr - last).abs() / last;
        delta > self.cfg.send_threshold_pct
    }

    pub fn mark_sent(&mut self) {
        self.last_sent_bps = self.target_bps;
    }

    pub fn reset_window(&mut self) {
        self.rolling_lost = 0;
        self.rolling_total = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_max(bps: u32) -> BitrateControllerConfig {
        BitrateControllerConfig::new_for_max(bps)
    }

    #[test]
    fn aimd_md_on_high_loss() {
        let mut c = BitrateController::new(cfg_max(10_000_000));
        c.observe(50, 1000); // 5% loss
        c.aimd_step(Instant::now());
        // 10M * 0.7 = 7_000_000; allow off-by-one for f32 rounding
        let bps = c.target_bps() as i64;
        assert!((bps - 7_000_000).abs() <= 1, "got {bps}");
    }

    #[test]
    fn aimd_ai_on_low_loss() {
        let mut cfg = cfg_max(10_000_000);
        cfg.initial_bps = 5_000_000;
        let mut c = BitrateController::new(cfg);
        c.observe(1, 1000); // 0.1% loss
        c.aimd_step(Instant::now());
        assert_eq!(c.target_bps(), 5_200_000); // +200kbps
    }

    #[test]
    fn aimd_hold_in_band() {
        let mut c = BitrateController::new(cfg_max(10_000_000));
        c.observe(15, 1000); // 1.5%, between low (0.5%) and high (2%)
        c.aimd_step(Instant::now());
        assert_eq!(c.target_bps(), 10_000_000); // unchanged
    }

    #[test]
    fn aimd_md_clamps_to_min() {
        let mut c = BitrateController::new(cfg_max(10_000_000));
        let now = Instant::now();
        for _ in 0..50 {
            c.observe(100, 1000); // 10% loss
            c.aimd_step(now);
            c.reset_window();
        }
        assert_eq!(c.target_bps(), 1_000_000); // min_bps
    }

    #[test]
    fn aimd_ai_clamps_to_max() {
        let mut cfg = cfg_max(2_000_000);
        cfg.initial_bps = 1_000_000;
        let mut c = BitrateController::new(cfg);
        let now = Instant::now();
        for _ in 0..20 {
            c.observe(0, 1000); // 0% loss
            c.aimd_step(now);
            c.reset_window();
        }
        assert_eq!(c.target_bps(), 2_000_000); // max_bps clamp
    }

    #[test]
    fn aimd_cooldown_after_md() {
        let mut cfg = cfg_max(10_000_000);
        cfg.initial_bps = 5_000_000;
        let mut c = BitrateController::new(cfg);
        let t0 = Instant::now();
        // MD trigger
        c.observe(50, 1000);
        c.aimd_step(t0);
        let after_md = c.target_bps();
        c.reset_window();
        // 1s later: try AI — must be suppressed (cooldown is 2s)
        c.observe(0, 1000);
        c.aimd_step(t0 + Duration::from_secs(1));
        assert_eq!(c.target_bps(), after_md, "AI suppressed during cooldown");
        c.reset_window();
        // 3s later: cooldown elapsed, AI permitted
        c.observe(0, 1000);
        c.aimd_step(t0 + Duration::from_secs(3));
        assert!(c.target_bps() > after_md, "AI allowed after cooldown");
    }

    #[test]
    fn hysteresis_filters_small_changes() {
        let mut cfg = cfg_max(10_000_000);
        cfg.initial_bps = 5_000_000;
        let mut c = BitrateController::new(cfg);
        // 4% bump: don't send. AI gives +200kbps → target 5_200_000 (+4%);
        // delta < 5% threshold so should_send is false.
        c.observe(0, 1000);
        c.aimd_step(Instant::now());
        assert!(!c.should_send(), "4% change suppressed");
        // Bump to 6%: send
        c.target_bps = 5_300_000;
        assert!(c.should_send(), "6% change passes");
    }

    #[test]
    fn disabled_controller_returns_max_always() {
        let mut cfg = cfg_max(10_000_000);
        cfg.enabled = false;
        let mut c = BitrateController::new(cfg);
        c.observe(500, 1000); // 50% loss
        c.aimd_step(Instant::now());
        assert_eq!(c.target_bps(), 10_000_000);
        assert!(!c.should_send(), "disabled never sends");
    }
}
