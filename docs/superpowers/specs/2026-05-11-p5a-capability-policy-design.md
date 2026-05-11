# P5A: Capability/Policy Layer — Design

**Date:** 2026-05-11
**Phase:** 5A (first phase of post-L4 roadmap, see `docs/superpowers/specs/2026-05-11-final-goal-roadmap.md`)
**Branch (planned):** `phase-p5a-capability-policy`
**Estimated effort:** 2-3 weeks
**Source advisors:** CCG synthesis (Codex trait/state-machine review + Gemini DX/observability review)
**Spec self-review applied:** 2026-05-11

---

## 1. Goal & DoD

### 1.1 Goal

Add a Capability/Policy Layer (`prdt-media-policy` crate + `PolicyDriven` wrapper) that, at startup, **enumerates available encoder backends per OS**, **picks the best one via a deterministic scoring policy**, and at runtime **detects encode degradation / device-loss and fails over to the next candidate** — all without changing host call sites that already use `Box<dyn VideoProducer>`.

### 1.2 Definition of Done

1. `cargo build/clippy/test --workspace` green on **Linux + Windows** (existing CI bar from L0-L4)
2. ≥ 8 new tests across SelectionPolicy / HealthMonitor / PolicyDriven (in-process integration)
3. Windows manual smoke (a) NVENC fail-injected → MF chosen via tracing log; (b) MF also fail → OpenH264
4. Linux manual smoke: OpenH264 probe + rank tracing log visible (1 backend so no failover, but enumeration path verified)
5. Host CLI accepts `--encoder {auto, nvenc, mf, openh264}`, `--encoder-hint <kind>`, `--force-sw`
6. Viewer overlay shows backend badge: `🚀 NVENC` (HW) / `💻 OpenH264` (SW)
7. Tracing structured log emits `event = backend_chosen | state_transition | failover` with documented fields
8. Section 9 (Out of scope) explicitly enumerates what is deferred (codec hot-swap, CSV telemetry writer, viewer-side PolicyDriven, GUI Force-SW toggle, etc.)

### 1.3 Non-goals (deferred — see §9)

- Codec hot-swap (H.265 ↔ H.264) — deferred to Phase 5 codec renegotiation
- CSV telemetry writer — deferred to P9 Hardening
- viewer-side decoder PolicyDriven — deferred to P9
- Settings GUI Force-SW toggle — deferred to Phase 4 GUI extension
- AccessKit screen-reader for backend badge — deferred to Phase 4 GUI extension

---

## 2. Background — current state

After L4 (`master` HEAD `294b109`):

- `prdt-media-core::Encoder` is a trait **with associated type `Frame`**, so `Box<dyn Encoder>` does not type-check (`crates/media-core/src/traits.rs:17-31`). Cross-OS factory therefore must work at the `VideoProducer` layer instead.
- `prdt-protocol::VideoProducer` is `Box<dyn>`-friendly (no associated type) and already returns `EncodedFrame` + exposes `request_idr()` / `set_target_bitrate()` / `backend_name()` (`crates/protocol/src/video_pipeline.rs:33-50`). Host already stores it as `producer: Box<dyn VideoProducer>` (`crates/host/src/lib.rs:522`).
- `ProducerError` today is string-based with three variants (`Capture(String) / Encode(String) / Other(String)`) — no typed `DeviceLost`, so failover triggers cannot be matched without fragile string comparisons.
- Existing backends: Windows `HwHevcEncoder { Nvenc(...), Mf(...) }` enum (in `media-win`) + cross-platform `Openh264Encoder` (in `media-sw`). Selection is hard-coded by `--encoder {auto, nvenc, mf, openh264}` flag at startup; no runtime fallback.
- L4 added live `set_target_bitrate` (NVENC + OpenH264 do real reconfigure, MF still no-op pending L5). The Capability/Policy layer reuses this for the `Healthy → Degraded` action.

---

## 3. Architecture & crate boundaries

### 3.1 Layout

```text
new crate: crates/media-policy/                            (workspace member name: prdt-media-policy)
├── Cargo.toml         deps: prdt-protocol, prdt-media-core, tracing, async-trait, serde, toml
└── src/
    ├── lib.rs         public re-exports
    ├── capability.rs  BackendKind, Codec, EncoderCapability, CapabilityProbe trait
    ├── factory.rs     ProducerFactory trait + FactoryError enum + ProducerConfig struct
    ├── selection.rs   SelectionPolicy + ScoringPolicy + filter_then_rank logic
    ├── health.rs      HealthMonitor + HealthState + HealthAction
    ├── driver.rs      PolicyDriven (impl VideoProducer; holds Box<dyn VideoProducer>)
    └── tests/         in-process integration with MockProducerA/B
```

### 3.2 Boundary rules

- `prdt-media-policy` has **no OS-specific code**. All OS knowledge lives behind `dyn CapabilityProbe` + `dyn ProducerFactory`.
- Backend crates (`prdt-media-win`, `prdt-media-sw`, `prdt-media-linux`, future `prdt-media-mac`) **only impl** `CapabilityProbe + ProducerFactory`. They do not contain policy logic.
- `host` is unchanged at the call-site level: it still receives a `Box<dyn VideoProducer>`. The constructor changes from `DxgiNvencProducer::new(...)` to `PolicyDriven::bootstrap(...)`.

### 3.3 Why a separate crate (`prdt-media-policy`) and not `prdt-media-core`

`media-core` is the stable trait/error boundary that all backend crates depend on. P5A introduces fast-changing logic (scoring weights, state-machine thresholds, exponential-backoff cooldowns) that should not force backend recompiles. Separating keeps the dependency graph DAG clean: `policy → core ← backends`.

---

## 4. Public API skeleton

### 4.1 `capability.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Nvenc,
    MfHevc,
    Openh264,
    // future: Vaapi, V4L2M2M, VideoToolbox, MediaCodec
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec { H264, H265 /* future: AV1 */ }

#[derive(Debug, Clone)]
pub struct EncoderCapability {
    pub backend: BackendKind,
    pub codec: Codec,
    pub max_resolution: (u32, u32),     // (width, height)
    pub max_fps: u32,
    pub zero_copy: bool,
    pub priority: i32,                   // OS-fixed: NVENC=100, MF=80, VAAPI=90, Openh264=10
}

pub trait CapabilityProbe: Send + Sync {
    fn list_encoders(&self) -> Vec<EncoderCapability>;
}
```

### 4.2 `factory.rs`

```rust
#[derive(Debug, thiserror::Error)]
pub enum FactoryError {
    #[error("backend {0:?} unavailable: {1}")]
    Unavailable(BackendKind, String),
    #[error("config invalid for backend {0:?}: {1}")]
    InvalidConfig(BackendKind, String),
}

pub struct ProducerConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub initial_bitrate_bps: u32,
    pub codec: Codec,
}

pub trait ProducerFactory: Send + Sync {
    fn create(
        &self,
        kind: BackendKind,
        cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError>;
}
```

### 4.3 `selection.rs`

```rust
pub struct PolicyContext {
    pub target_resolution: (u32, u32),
    pub target_fps: u32,
    pub target_bitrate_bps: u32,
    pub codec: Codec,
    pub user_override: Option<BackendKind>,   // --encoder nvenc (Strict, no failover)
    pub user_hint: Option<BackendKind>,       // --encoder-hint nvenc (preferred but failover OK)
    pub force_sw: bool,                        // --force-sw shorthand
}

pub struct HistoryTable {
    /// Per-backend cumulative success/failure counts (used in beta-posterior reliability).
    /// Updated by HealthMonitor.
    counts: HashMap<BackendKind, BackendStats>,
}

pub struct BackendStats {
    pub successes: u32,
    pub failures: u32,
    pub last_failure_at: Option<Instant>,
    pub cooldown_until: Option<Instant>,       // exponential backoff: 10s → 20s → 40s → 80s → cap 300s
    pub recent_encode_p95_us: Option<u64>,     // snapshot from HealthMonitor on each successful frame; used by SelectionPolicy::rank for `latency_fit`
}

impl HistoryTable {
    pub fn cooldown_remaining(&self, backend: BackendKind) -> Duration { ... }
    pub fn record_success(&mut self, backend: BackendKind);
    pub fn record_failure(&mut self, backend: BackendKind, now: Instant);
}

pub trait SelectionPolicy: Send + Sync {
    /// Two-stage:
    ///   (1) hard filter — drop candidates that fail codec/resolution/fps/cooldown gates
    ///   (2) soft score — rank remainder by weighted score, return descending list
    fn rank(
        &self,
        candidates: &[EncoderCapability],
        ctx: &PolicyContext,
        history: &HistoryTable,
    ) -> Vec<BackendKind>;
}

/// Default impl
pub struct ScoringPolicy {
    pub weights: ScoringWeights,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScoringWeights {
    pub priority: f64,    // default 0.45
    pub zero_copy: f64,   // default 0.20
    pub latency_fit: f64, // default 0.25
    pub reliability: f64, // default 0.10
}
```

### 4.4 `health.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Failing,
    Lost,
}

pub enum HealthAction {
    /// Stay on current backend; ask it to reduce bitrate by `factor` (e.g. 0.85)
    /// and request an IDR. Triggered on Healthy → Degraded.
    ReconfigureBitrate { factor: f32 },
    /// Drop current backend; ask SelectionPolicy to pick a new one in the same codec.
    /// Triggered on Degraded → Failing.
    Failover { reason: FailoverReason },
}

#[derive(Debug, Clone)]
pub enum FailoverReason {
    LatencyDegradation { encode_p95_us: u64, frame_budget_us: u64 },
    ConsecutiveFailures { count: u32 },
    NoSuccessTimeout { idle_ms: u64 },
    DeviceLost { backend: String, reason: String },
}

pub struct HealthMonitor {
    state: HealthState,
    encode_p95_ema: f64,                      // exponential moving average of encode time
    consecutive_failures: u32,
    last_success_at: Instant,
    frame_budget_us: u64,                     // 1_000_000 / target_fps
    deg_threshold_factor: f64,                 // 1.5
    rec_threshold_factor: f64,                 // 1.2 (hysteresis)
    deg_window_count: u32,                     // 3 windows of 30 frames each
    rec_window_count: u32,                     // 5 windows
    failure_threshold: u32,                    // 3 consecutive
    no_success_timeout: Duration,              // 500ms
}

impl HealthMonitor {
    pub fn record_encode(&mut self, encode_us: u64) -> Option<HealthAction>;
    pub fn record_failure(&mut self, err: &ProducerError) -> Option<HealthAction>;
    pub fn current_state(&self) -> HealthState;
}
```

### 4.5 `driver.rs`

```rust
pub struct PolicyDriven {
    factory: Arc<dyn ProducerFactory>,
    probe: Arc<dyn CapabilityProbe>,
    policy: Arc<dyn SelectionPolicy>,
    monitor: HealthMonitor,
    history: HistoryTable,
    inner: Box<dyn VideoProducer>,
    inner_kind: BackendKind,
    candidates: Vec<BackendKind>,            // SelectionPolicy::rank result
    cfg: ProducerConfig,
    current_bitrate_bps: u32,                 // for bitrate handoff on swap
    ctx: PolicyContext,                       // for re-rank on failover
}

impl PolicyDriven {
    /// Probe → rank → instantiate top-1. If top-1 fails to instantiate, try next.
    /// If all fail, return last FactoryError.
    pub fn bootstrap(
        probe: Arc<dyn CapabilityProbe>,
        factory: Arc<dyn ProducerFactory>,
        policy: Arc<dyn SelectionPolicy>,
        cfg: ProducerConfig,
        ctx: PolicyContext,
    ) -> Result<Self, FactoryError>;

    fn handle_action(&mut self, action: Option<HealthAction>) -> Result<(), ProducerError> {
        match action {
            None => Ok(()),
            Some(HealthAction::ReconfigureBitrate { factor }) => {
                let new_bps = ((self.current_bitrate_bps as f32) * factor) as u32;
                self.inner.set_target_bitrate(new_bps);
                self.inner.request_idr();
                self.current_bitrate_bps = new_bps;
                Ok(())
            }
            Some(HealthAction::Failover { reason }) => {
                self.swap_to_next(reason)
            }
        }
    }

    fn swap_to_next(&mut self, reason: FailoverReason) -> Result<(), ProducerError> {
        // 1. Mark current backend failed in history (resets cooldown timer)
        self.history.record_failure(self.inner_kind, Instant::now());
        // 2. Re-rank remaining candidates within the same codec
        self.candidates = self.policy.rank(&self.probe.list_encoders(), &self.ctx, &self.history);
        // 3. Pick next that is not the current and not in cooldown
        let next = self.candidates.iter()
            .copied()
            .find(|k| *k != self.inner_kind && self.history.cooldown_remaining(*k).is_zero())
            .ok_or_else(|| ProducerError::Other("no failover candidate available".into()))?;
        // 4. Instantiate, hand off bitrate, force IDR (so viewer gets fresh SPS/PPS)
        let mut new_producer = self.factory.create(next, &self.cfg).map_err(|e| {
            ProducerError::Other(format!("factory failed for {next:?}: {e}"))
        })?;
        new_producer.set_target_bitrate(self.current_bitrate_bps);
        new_producer.request_idr();
        // 5. Drain previous (best-effort, capped at 50ms in next_frame loop) then drop
        let prev_kind = self.inner_kind;
        self.inner = new_producer;
        self.inner_kind = next;
        // 6. Reset HealthMonitor for new backend (Healthy state)
        self.monitor.reset_for_new_backend();
        tracing::warn!(
            event = "failover",
            from = ?prev_kind,
            to = ?next,
            reason = ?reason,
            retained_bitrate_bps = self.current_bitrate_bps,
        );
        Ok(())
    }
}

#[async_trait]
impl VideoProducer for PolicyDriven {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        let t0 = Instant::now();
        match self.inner.next_frame().await {
            Ok(frame) => {
                let action = self.monitor.record_encode(t0.elapsed().as_micros() as u64);
                self.handle_action(action)?;
                Ok(frame)
            }
            Err(e) => {
                let action = self.monitor.record_failure(&e);
                if action.is_some() {
                    self.handle_action(action)?;
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
```

---

## 5. State machine & thresholds

```text
              ┌─────────┐
              │ Healthy │
              └────┬────┘
                   │ encode_p95_ema > 1.5 × frame_budget_us, in 3 consecutive 30-frame windows
                   ▼
              ┌──────────┐  HealthAction::ReconfigureBitrate { factor: 0.85 }
              │ Degraded │  + request_idr (within current backend, L4 path)
              └────┬─┬───┘
                   │ │ encode_p95_ema < 1.2 × frame_budget_us, in 5 consecutive windows (hysteresis)
                   │ └────────► Healthy
                   │
                   │ consecutive failures ≥ 3  OR  no success in > 500ms
                   ▼
              ┌─────────┐  HealthAction::Failover { reason: ConsecutiveFailures }
              │ Failing │  → SelectionPolicy::rank → next candidate (same codec only)
              └────┬────┘  → drain inner ≤ 50ms, drop overflow, swap, set_target_bitrate(retained), request_idr
                   │
                   │ ProducerError::DeviceLost (immediate, regardless of state)
                   ▼
              ┌─────┐  HealthAction::Failover { reason: DeviceLost }
              │Lost │  → CapabilityProbe re-run + factory re-create
              └─────┘  cooldown for failed backend: exponential backoff
                       (10s → 20s → 40s → 80s, cap 300s)
```

### 5.1 Numerical defaults

| Parameter | Default | Rationale |
|---|---|---|
| `frame_budget_us` | `1_000_000 / target_fps` | 60 fps → 16,667 us |
| `deg_threshold_factor` | 1.5 | Codex recommendation; above 1.5× budget the encoder is missing frame deadlines |
| `rec_threshold_factor` | 1.2 | Hysteresis margin to avoid flapping |
| `window_size_frames` | 30 | 0.5 s @ 60 fps → smooths instantaneous spikes |
| `deg_window_count` | 3 | 1.5 s sustained degradation triggers reconfigure |
| `rec_window_count` | 5 | 2.5 s sustained recovery returns to Healthy |
| `failure_threshold` | 3 | Codex recommendation (down from initial proposal of 5) |
| `no_success_timeout` | 500 ms | Catches stalls that don't emit explicit errors |
| `cooldown_initial` | 10 s | Long enough that a transient driver glitch doesn't immediately retry |
| `cooldown_max` | 300 s | Cap so a permanently-failed backend is still re-probed eventually |
| `drain_timeout` | 50 ms | Brief drain of in-flight encodes before swap; balance between data loss and stall |

### 5.2 Why same-codec-only failover

P5A keeps failover within the same codec (e.g. NVENC H.265 → MF H.265 → fall back to **H.265** OpenH264). Codec hot-swap (H.265 → H.264) requires:

1. viewer-side decoder hot-swap (currently the viewer commits to a decoder at session start, see `crates/viewer/src/lib.rs:1273`)
2. Wire renegotiation of the codec field (HelloAck-style mid-stream)
3. SPS/PPS handling that crosses the codec boundary

These are intrusive enough that they belong in a dedicated phase (Phase 5 codec renegotiation). P5A explicitly declines them.

If no same-codec candidate is available (e.g. on Linux only OpenH264 is registered for the codec), `swap_to_next` returns `ProducerError::Other("no failover candidate available")` and the host's existing watchdog is responsible for session teardown.

---

## 6. SelectionPolicy: filter then score

### 6.1 Hard filter

```rust
candidates.retain(|cap| {
    cap.codec == ctx.codec
        && cap.max_resolution.0 >= ctx.target_resolution.0
        && cap.max_resolution.1 >= ctx.target_resolution.1
        && cap.max_fps >= ctx.target_fps
        && (!ctx.force_sw || matches!(cap.backend, BackendKind::Openh264))
        && history.cooldown_remaining(cap.backend).is_zero()
});
```

`-1000` style soft penalties for hard requirements are explicitly rejected (Codex feedback): a sufficiently bad weight tuning could otherwise cause an unsupportable backend to slip through.

### 6.2 Soft score (0.0–1.0 weighted)

```rust
let priority_norm   = (cap.priority as f64 / 100.0).clamp(0.0, 1.0);
let zero_copy_bonus = if cap.zero_copy { 1.0 } else { 0.0 };
let runtime_p95_us  = history.recent_encode_p95_us(cap.backend)        // None ⇒ never run on this backend
                              .unwrap_or(frame_budget_us / 2) as f64;  // optimistic prior: half-budget
let latency_fit     = (frame_budget_us as f64 / runtime_p95_us.max(1.0)).min(1.0);  // 1.0 = on-budget; 0.5 = double budget
let reliability     = beta_posterior(history.successes(cap.backend), history.failures(cap.backend));

let score = w.priority * priority_norm
          + w.zero_copy * zero_copy_bonus
          + w.latency_fit * latency_fit
          + w.reliability * reliability;
```

Default weights: `priority=0.45, zero_copy=0.20, latency_fit=0.25, reliability=0.10`.

`beta_posterior(s, f)` returns `(s + 1) / (s + f + 2)` (Beta(1,1) prior; smoothing for cold start).

### 6.3 user_override / user_hint / force_sw resolution order

1. `force_sw = true` ⇒ filter step retains only `Openh264`
2. `user_override = Some(kind)` ⇒ if kind survives the hard filter, the rank list is just `[kind]`. If it doesn't (e.g. unsupported on this OS), `bootstrap` returns `FactoryError::Unavailable`. **Strict mode does not failover** at runtime either.
3. `user_hint = Some(kind)` ⇒ kind gets a `+0.5` score bump in the soft score; failover is still allowed.
4. None of the above ⇒ normal score-driven rank.

### 6.4 Weight tuning before telemetry exists

Defaults above are educated guesses. The `ScoringWeights` struct deserialises from a fixed default path (`dirs::config_dir()/prdt/policy.toml`, e.g. `~/.config/prdt/policy.toml` on Linux, `%APPDATA%\prdt\policy.toml` on Windows). If the file is missing or malformed, `Default::default()` is used. **No CLI flag for an alternate path in P5A** — that override is deferred (see §9). Every selection logs `event = backend_chosen, score = .., breakdown = {..}` at `info!` level so operators can observe scoring in field deployments.

CSV telemetry / pandas-friendly export is **out of scope** for P5A (deferred to P9 Hardening, see §9).

---

## 7. ProducerError extension

### 7.1 Wire-compatible variant addition

```rust
// crates/protocol/src/video_pipeline.rs
#[derive(Debug, thiserror::Error)]
pub enum ProducerError {
    #[error("capture: {0}")]
    Capture(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("device lost on {backend}: {reason}")]
    DeviceLost { backend: String, reason: String },     // ← P5A new variant
    #[error("other: {0}")]
    Other(String),
}
```

### 7.2 Mapping rules per backend

| Backend | Source | Maps to |
|---|---|---|
| `media-win` NVENC | `MediaError::DeviceRemoved` (existing, see `crates/media-win/src/core_adapter.rs:54`) | `ProducerError::DeviceLost { backend: "nvenc-h265", reason: err.to_string() }` |
| `media-win` MF | `MediaError::DeviceRemoved` (same source) | `ProducerError::DeviceLost { backend: "mf-h265", reason: err.to_string() }` |
| `media-sw` OpenH264 | catastrophic init failure (e.g. WelsCreateSVCEncoder NULL) | `ProducerError::DeviceLost { backend: "openh264", reason: ... }` |
| `media-sw` OpenH264 | per-frame encode failure (recoverable) | `ProducerError::Encode(...)` (existing) |

### 7.3 Existing call-site update

`crates/host/src/lib.rs:518` currently logs producer errors as opaque strings. Update the `match` to:

```rust
match err {
    ProducerError::DeviceLost { ref backend, ref reason } => {
        warn!(backend, reason, "backend reported device lost; PolicyDriven will failover");
        // PolicyDriven::next_frame already handles internally; host just logs
    }
    other => warn!(?other, "producer error"),
}
```

No `protocol_version` bump required: the wire format is unchanged (errors are not serialised across the wire; they are local Rust types).

---

## 8. Host integration & UX hooks

### 8.1 Host wiring (`crates/host/src/lib.rs`)

Approximate diff: ~20 lines.

```rust
let probe = platform::probe();           // Win: WindowsProbe, Linux: LinuxSwProbe
let factory = platform::factory();       // Win: WindowsFactory (NVENC + MF + Openh264), Linux: LinuxSwFactory (Openh264)
let policy: Arc<dyn SelectionPolicy> = Arc::new(
    // Reads from `dirs::config_dir()/prdt/policy.toml`, falls back to defaults if missing.
    // No CLI override in P5A; see §9.
    ScoringPolicy::load_default_or_fallback()
);
let ctx = PolicyContext {
    target_resolution: (cfg.width, cfg.height),
    target_fps: cfg.fps,
    target_bitrate_bps: cfg.initial_bitrate_bps,
    codec: cfg.codec,
    user_override: cfg.encoder_override,
    user_hint: cfg.encoder_hint,
    force_sw: cfg.force_sw,
};
let policy_driven = PolicyDriven::bootstrap(probe, factory, policy, ProducerConfig {
    width: cfg.width,
    height: cfg.height,
    fps: cfg.fps,
    initial_bitrate_bps: cfg.initial_bitrate_bps,
    codec: cfg.codec,
}, ctx)?;
let producer: Box<dyn VideoProducer> = Box::new(policy_driven);
// ↓ from here, the existing video task loop is unchanged
```

### 8.2 New `platform::probe()` / `platform::factory()`

`crates/host/src/platform/win.rs`: aggregate probes for `NVENC`, `MF`, `Openh264`. Internally calls into existing constructors.

`crates/host/src/platform/linux.rs`: returns `Openh264` only.

### 8.3 CLI (`crates/host/src/main.rs`)

| Flag | Meaning |
|---|---|
| `--encoder auto` (default) | Policy-driven auto-selection |
| `--encoder nvenc \| mf \| openh264` | Strict mode — init failure exits, no failover |
| `--encoder-hint <kind>` | Add +0.5 score bump for `<kind>` but allow failover |
| `--force-sw` | Shorthand for `--encoder openh264` |

Viewer side (`crates/viewer/src/main.rs`) gets symmetrical `--decoder {auto, nvdec, mf, openh264}` and `--force-sw` flags. **viewer-side PolicyDriven is NOT implemented in P5A** — viewer continues to commit to one decoder at session start (see §9).

### 8.4 Tracing structured fields

Three documented events (operators can grep `event=`):

```rust
tracing::info!(
    event = "backend_chosen",
    backend = ?chosen_kind,
    score = chosen_score,
    candidates = ?ranked_with_scores,
);
tracing::info!(
    event = "state_transition",
    from = ?prev_state,
    to = ?next_state,
    encode_p95_us = monitor.encode_p95_ema as u64,
    frame_budget_us = monitor.frame_budget_us,
);
tracing::warn!(
    event = "failover",
    from = ?prev_kind,
    to = ?next_kind,
    reason = ?failover_reason,
    retained_bitrate_bps = current_bitrate_bps,
);
```

### 8.5 Viewer overlay UX (P5A scope)

- Show `🚀 NVENC` (HW backends) or `💻 OpenH264` (SW backend) badge in `crates/viewer-overlay`. Source: existing `VideoProducer::backend_name()` (already piped to viewer via stats CSV).
- Color + glyph + text label all present (Gemini accessibility recommendation: never color-only).

### 8.6 Out of overlay scope for P5A

- Hover-revealed detail (e.g. `NVENC HEVC P3 1080p60`) — Phase 4 GUI extension
- Recent-failure summary in Settings — P9
- AccessKit screen-reader integration — Phase 4 GUI extension

---

## 9. Out of scope (deferred)

Each item below has a recorded defer target. Do **not** scope-creep them into P5A.

| Item | Defer to | Rationale |
|---|---|---|
| Codec hot-swap (H.265 ↔ H.264 mid-stream) | Phase 5 codec renegotiation | Requires viewer decoder swap + HelloAck mid-stream renegotiation |
| viewer-side PolicyDriven for decoder | P9 Hardening | Same-codec failover on host means viewer does not need to swap decoder |
| CSV telemetry writer + Python analysis | P9 Hardening | Tracing logs are sufficient for MVP weight observation |
| GUI Settings `Force SW` toggle | Phase 4 GUI extension | P5A ships CLI flags only |
| GUI overlay hover-detail / 24h fallback summary | Phase 4 GUI extension + P9 | Beyond minimum badge for MVP |
| AccessKit screen-reader on overlay | Phase 4 GUI extension | egui crate-level work, separate from P5A |
| `prdt-bench-matrix` backend axis | P9 | Benchmark expansion is its own phase |
| Wire renegotiation of codec field | Phase 5 | Touches Hello/HelloAck protocol, separate spec |
| Adding `--policy-config <path>` CLI flag | Phase 4 GUI extension | P5A reads from default location only |

---

## 10. Test strategy

| Layer | Test | Tool |
|---|---|---|
| `CapabilityProbe` | OS-specific fixture loaded via `serde_json` into `MockProbe` | `serde_json` |
| `SelectionPolicy` | Deterministic rank for fixed candidate + history input; proptest for shuffle-invariance | `proptest` |
| `HealthMonitor` | Time-based transitions driven by `tokio::time::pause` + `advance` | `tokio::time` mock |
| `PolicyDriven` swap | `MockProducerA/B` scripted with `Ok / Encode / DeviceLost`; verify `Lost → swap → recovery` in one async test | in-process integration |
| Cross-platform CI | `cargo build/clippy/test --workspace` on Linux + Windows | existing GitHub Actions |
| Windows manual smoke | env-var injection forces NVENC fail → assert MF chosen via tracing log; force MF fail → OpenH264 | manual |
| Linux manual smoke | OpenH264 probe + rank tracing log visible | manual |

Target: ≥ 8 new tests (4 SelectionPolicy + 3 HealthMonitor + 1 PolicyDriven integration; more if proptest splits count separately).

---

## 11. Risks & mitigations

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| 1 | Default scoring weights cause wrong backend selection on real hardware | MEDIUM | Toml override + tracing log of all candidate scores; tune in P9 from real telemetry |
| 2 | Hysteresis thresholds cause flapping under realistic GPU contention | MEDIUM | Initial defaults from Codex review (1.5× / 1.2× / 3 windows / 5 windows); tune in P9 |
| 3 | `ProducerError::DeviceLost` variant addition breaks downstream `match` | LOW | Compile-time exhaustive match enforced; add `_` arm to consumer code where appropriate |
| 4 | OpenH264 `DeviceLost` is meaningless (always available) — wasted variant on Linux | LOW | Used for catastrophic init failures only; non-init failures stay as `Encode(...)` |
| 5 | NVENC same-codec failover to MF inherits known MF latency-instability problem (see L0 known limitations) | MEDIUM | `--encoder nvenc` Strict mode lets users opt out; tracing log surfaces the swap |
| 6 | `bootstrap` instantiates first-pick backend during PolicyDriven::new — slow on cold start (NVENC init can be 50-200ms) | LOW | Acceptable: this matches current behaviour where NVENC init runs at session start |
| 7 | Drain timeout (50ms) loses in-flight frames during swap | LOW | Documented; viewer's existing IDR-recovery loop handles the gap |

---

## 12. References

- Roadmap: `docs/superpowers/specs/2026-05-11-final-goal-roadmap.md`
- Existing trait definitions: `crates/media-core/src/traits.rs`, `crates/protocol/src/video_pipeline.rs`
- Existing host wiring: `crates/host/src/lib.rs:486-525`
- Existing Windows backend enum: `crates/media-win/src/encoder_trait.rs`
- L4 (live encoder reconfigure, prior phase): `docs/superpowers/specs/2026-05-11-l4-encoder-reconfigure-design.md`
- Status: `docs/superpowers/STATUS.md`
- CCG advisor outputs (verbatim): `.omc/artifacts/ask/codex-rust-video-pipeline-trait-...-2026-05-11T05-16-59-989Z.md`, `.omc/artifacts/ask/gemini-rust-video-pipeline-dx-...-2026-05-11T05-13-44-451Z.md`
