//! `PolicyDriven` wraps any `Box<dyn VideoProducer>` and adds policy-driven
//! swap-on-failure. From the host's perspective it is just another
//! `VideoProducer` — same trait, same call sites.

use crate::capability::BackendKind;
use crate::factory::{FactoryError, ProducerConfig, ProducerFactory};
use crate::health::{FailoverReason, HealthAction, HealthMonitor};
use crate::selection::{HistoryTable, PolicyContext, SelectionPolicy};
use async_trait::async_trait;
use prdt_protocol::{EncodedFrame, ProducerError, VideoProducer};
use std::sync::Arc;
use std::time::Instant;

pub struct PolicyDriven {
    factory: Arc<dyn ProducerFactory>,
    probe: Arc<dyn crate::capability::CapabilityProbe>,
    policy: Arc<dyn SelectionPolicy>,
    monitor: HealthMonitor,
    history: HistoryTable,
    inner: Box<dyn VideoProducer>,
    inner_kind: BackendKind,
    cfg: ProducerConfig,
    ctx: PolicyContext,
    current_bitrate_bps: u32,
}

impl PolicyDriven {
    /// Probe → rank → instantiate top-1. If top-1 fails to create, try the
    /// next candidate. If all fail, return the last `FactoryError`.
    pub fn bootstrap(
        probe: Arc<dyn crate::capability::CapabilityProbe>,
        factory: Arc<dyn ProducerFactory>,
        policy: Arc<dyn SelectionPolicy>,
        cfg: ProducerConfig,
        ctx: PolicyContext,
    ) -> Result<Self, FactoryError> {
        let candidates = probe.list_encoders();
        let ranked = policy.rank(&candidates, &ctx, &HistoryTable::new());
        if ranked.is_empty() {
            return Err(FactoryError::Unavailable(
                BackendKind::Openh264,
                "no candidate survived policy filter".into(),
            ));
        }
        let mut last_err: Option<FactoryError> = None;
        for kind in &ranked {
            match factory.create(*kind, &cfg) {
                Ok(producer) => {
                    let monitor = HealthMonitor::new(cfg.fps);
                    let initial_bitrate = cfg.initial_bitrate_bps;
                    tracing::info!(
                        event = "backend_chosen",
                        backend = ?kind,
                        ranked = ?ranked,
                        "PolicyDriven bootstrap chose backend",
                    );
                    return Ok(Self {
                        factory,
                        probe,
                        policy,
                        monitor,
                        history: HistoryTable::new(),
                        inner: producer,
                        inner_kind: *kind,
                        cfg,
                        ctx,
                        current_bitrate_bps: initial_bitrate,
                    });
                }
                Err(e) => {
                    tracing::warn!(backend = ?kind, error = %e, "factory failed; trying next candidate");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.expect("ranked non-empty implies at least one factory call"))
    }

    fn handle_action(&mut self, action: Option<HealthAction>) -> Result<(), ProducerError> {
        match action {
            None => Ok(()),
            Some(HealthAction::ReconfigureBitrate { factor }) => {
                let new_bps = ((self.current_bitrate_bps as f32) * factor) as u32;
                tracing::info!(
                    event = "state_transition",
                    from = "Healthy",
                    to = "Degraded",
                    encode_p95_us = self.monitor.encode_p95_ema(),
                    frame_budget_us = self.monitor.frame_budget_us(),
                    new_bitrate_bps = new_bps,
                );
                self.current_bitrate_bps = new_bps;
                self.inner.set_target_bitrate(new_bps);
                self.inner.request_idr();
                Ok(())
            }
            Some(HealthAction::Failover { reason }) => self.swap_to_next(reason),
        }
    }

    fn swap_to_next(&mut self, reason: FailoverReason) -> Result<(), ProducerError> {
        let now = Instant::now();
        self.history.record_failure(self.inner_kind, now);

        let candidates = self.probe.list_encoders();
        let ranked = self.policy.rank(&candidates, &self.ctx, &self.history);
        let next = ranked
            .into_iter()
            .find(|k| *k != self.inner_kind)
            .ok_or_else(|| ProducerError::Other(format!(
                "no failover candidate available (current = {:?})", self.inner_kind
            )))?;

        let mut new_producer = self.factory.create(next, &self.cfg).map_err(|e| {
            ProducerError::Other(format!("factory failed for {next:?}: {e}"))
        })?;
        new_producer.set_target_bitrate(self.current_bitrate_bps);
        new_producer.request_idr();

        tracing::warn!(
            event = "failover",
            from = ?self.inner_kind,
            to = ?next,
            reason = ?reason,
            retained_bitrate_bps = self.current_bitrate_bps,
        );

        self.inner = new_producer;
        self.inner_kind = next;
        self.monitor.reset_for_new_backend();
        Ok(())
    }
}

#[async_trait]
impl VideoProducer for PolicyDriven {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        let t0 = Instant::now();
        match self.inner.next_frame().await {
            Ok(frame) => {
                let encode_us = t0.elapsed().as_micros() as u64;
                self.history.update_encode_p95(self.inner_kind, encode_us);
                self.history.record_success(self.inner_kind);
                let action = self.monitor.record_encode(encode_us);
                self.handle_action(action)?;
                Ok(frame)
            }
            Err(e) => {
                let action = self.monitor.record_failure(&e);
                if action.is_some() {
                    self.handle_action(action)?;
                    // Retry on the new backend.
                    self.inner.next_frame().await
                } else {
                    Err(e)
                }
            }
        }
    }

    fn request_idr(&mut self) { self.inner.request_idr(); }

    fn set_target_bitrate(&mut self, bps: u32) {
        self.current_bitrate_bps = bps;
        self.inner.set_target_bitrate(bps);
    }

    fn backend_name(&self) -> &'static str { self.inner.backend_name() }
}
