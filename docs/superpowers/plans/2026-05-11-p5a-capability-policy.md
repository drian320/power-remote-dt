# P5A Capability/Policy Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `prdt-media-policy` crate that probes available encoder backends per OS, ranks them via deterministic scoring, and fails over within the same codec on `DeviceLost` / latency-degradation — without changing existing host call sites that already use `Box<dyn VideoProducer>`.

**Architecture:** New `prdt-media-policy` crate holds 4 components (`CapabilityProbe`, `SelectionPolicy`, `HealthMonitor`, `ProducerFactory`) plus a `PolicyDriven` wrapper that itself impls `VideoProducer` and contains `Box<dyn VideoProducer>` swapped on failure. Backend crates only impl `CapabilityProbe + ProducerFactory`; policy logic stays in the new crate. `ProducerError` gains a typed `DeviceLost { backend, reason }` variant so failover triggers don't depend on string matching.

**Tech Stack:** Rust 1.85, edition 2021, `tokio` async, `tracing`, `proptest`, `serde + toml` for policy weight config, `dirs` for default config path.

**Spec:** `docs/superpowers/specs/2026-05-11-p5a-capability-policy-design.md` (commit `0fea661`)

**Branch:** `phase-p5a-capability-policy`

**Tag (on completion):** `phase-p5a-capability-policy-complete`

**Cross-platform regression bar:** Linux + Windows both green for `cargo build/clippy/test --workspace -- -D warnings` (matches L0-L4 bar).

---

## File Structure

### Created

| Path | Responsibility |
|---|---|
| `crates/media-policy/Cargo.toml` | New workspace member: deps prdt-protocol, prdt-media-core, tracing, async-trait, serde, toml, dirs |
| `crates/media-policy/src/lib.rs` | Public re-exports for the 4 components + PolicyDriven |
| `crates/media-policy/src/capability.rs` | BackendKind, Codec, EncoderCapability, CapabilityProbe trait |
| `crates/media-policy/src/factory.rs` | ProducerFactory trait, FactoryError, ProducerConfig |
| `crates/media-policy/src/selection.rs` | SelectionPolicy trait, ScoringPolicy default impl, ScoringWeights, PolicyContext, HistoryTable, BackendStats |
| `crates/media-policy/src/health.rs` | HealthMonitor, HealthState, HealthAction, FailoverReason |
| `crates/media-policy/src/driver.rs` | PolicyDriven (impl VideoProducer) + bootstrap + swap_to_next |
| `crates/media-policy/tests/policy_driven_swap.rs` | In-process integration test: MockProducerA/B scripted Lost → swap → recovery |
| `crates/media-win/src/policy.rs` | WindowsProbe + WindowsFactory (NVENC/MF/OpenH264 enumeration) |
| `crates/media-linux/src/policy.rs` | LinuxSwProbe + LinuxSwFactory (OpenH264 only) |

### Modified

| Path | Change |
|---|---|
| `Cargo.toml` (workspace) | Add `crates/media-policy` to `[workspace] members` |
| `crates/protocol/src/video_pipeline.rs` | Add `ProducerError::DeviceLost { backend: String, reason: String }` variant |
| `crates/media-win/src/core_adapter.rs` | Map `MediaError::DeviceRemoved` → `EncodeError::DeviceLost` is already done; here we ensure the *producer-level* `next_frame` returns `ProducerError::DeviceLost { backend: "...", reason }` |
| `crates/media-win/src/lib.rs` | `pub mod policy;` |
| `crates/media-linux/src/lib.rs` | `pub mod policy;` |
| `crates/host/src/lib.rs` | Replace direct producer construction with `PolicyDriven::bootstrap(probe, factory, policy, cfg, ctx)` |
| `crates/host/src/main.rs` (or wherever clap is) | Add `--encoder-hint <kind>` and `--force-sw` flags; `--encoder` semantics: `auto` (default policy), `nvenc|mf|openh264` (Strict, no failover) |
| `crates/host/src/platform/win.rs` | Provide `pub fn probe()` and `pub fn factory()` returning `prdt_media_win::policy::WindowsProbe/Factory` |
| `crates/host/src/platform/linux.rs` | Same for Linux: `prdt_media_linux::policy::LinuxSwProbe/Factory` |
| `crates/viewer/src/lib.rs` (or overlay path) | Use `producer.backend_name()` (already piped via stats CSV) to render badge `🚀 NVENC` / `💻 OpenH264` in viewer-overlay |
| `docs/superpowers/STATUS.md` | Add P5A entry with smoke walkthrough notes |

---

## Task list overview

| # | Task | Files |
|---|---|---|
| T1 | Foundation: new crate skeleton + `ProducerError::DeviceLost` variant + workspace wire | crates/media-policy/{Cargo.toml,src/lib.rs}, Cargo.toml, crates/protocol/src/video_pipeline.rs |
| T2 | capability.rs (types + CapabilityProbe trait + MockProbe + JSON fixture test) | crates/media-policy/src/capability.rs |
| T3 | factory.rs (ProducerFactory + FactoryError + ProducerConfig + MockFactory test) | crates/media-policy/src/factory.rs |
| T4 | selection.rs (filter + ScoringPolicy + HistoryTable + 4+ tests incl. proptest) | crates/media-policy/src/selection.rs |
| T5 | health.rs (state machine + HealthMonitor + 3+ tests with tokio::time::pause) | crates/media-policy/src/health.rs |
| T6 | driver.rs (PolicyDriven impl VideoProducer + bootstrap + swap_to_next + integration test) | crates/media-policy/src/driver.rs, crates/media-policy/tests/policy_driven_swap.rs |
| T7 | Backend integration (media-win/linux probe+factory) + host CLI + viewer overlay badge | crates/media-{win,linux}/src/policy.rs, crates/host/src/{lib.rs,main.rs,platform/{win,linux}.rs}, crates/viewer/src/lib.rs |
| T8 | Manual smoke (Windows + Linux) + STATUS update + tag | docs/superpowers/STATUS.md |

---

## Task 1: Foundation — new crate skeleton + ProducerError::DeviceLost + workspace wire

**Files:**
- Create: `crates/media-policy/Cargo.toml`
- Create: `crates/media-policy/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/protocol/src/video_pipeline.rs:11-19` (add `DeviceLost` variant)

- [ ] **Step 1: Create the new crate's Cargo.toml**

```toml
# crates/media-policy/Cargo.toml
[package]
name = "prdt-media-policy"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
prdt-protocol     = { path = "../protocol" }
prdt-media-core   = { path = "../media-core" }
async-trait       = "0.1"
tokio             = { workspace = true, features = ["sync", "time", "rt"] }
tracing           = "0.1"
serde             = { version = "1", features = ["derive"] }
toml              = "0.8"
dirs              = "5"
thiserror         = "1"

[dev-dependencies]
tokio             = { workspace = true, features = ["rt-multi-thread", "macros", "test-util"] }
proptest          = "1"
serde_json        = "1"
```

- [ ] **Step 2: Create the new crate's src/lib.rs (placeholder for now)**

```rust
// crates/media-policy/src/lib.rs
//! Capability/Policy layer for the prdt media pipeline.
//!
//! This crate enumerates encoder backends (`CapabilityProbe`), ranks them
//! against runtime context (`SelectionPolicy`), watches encode performance
//! for degradation or device loss (`HealthMonitor`), constructs them
//! (`ProducerFactory`), and presents the result to host code as a single
//! `Box<dyn VideoProducer>` (`PolicyDriven`).
//!
//! See `docs/superpowers/specs/2026-05-11-p5a-capability-policy-design.md`
//! for the full design.

// Module shells; populated in T2-T6.
pub mod capability;
pub mod factory;
pub mod selection;
pub mod health;
pub mod driver;

// Re-exports for ergonomic consumer use:
pub use capability::{BackendKind, Codec, EncoderCapability, CapabilityProbe};
pub use factory::{FactoryError, ProducerConfig, ProducerFactory};
pub use selection::{
    BackendStats, HistoryTable, PolicyContext, ScoringPolicy, ScoringWeights, SelectionPolicy,
};
pub use health::{FailoverReason, HealthAction, HealthMonitor, HealthState};
pub use driver::PolicyDriven;
```

- [ ] **Step 3: Stub each module so lib.rs compiles**

Create five empty module files. Each just needs the right declarations to satisfy the `pub use` lines in `lib.rs`. They will be filled in T2-T6.

`crates/media-policy/src/capability.rs`:
```rust
// Stub — populated in T2.
pub enum BackendKind { Placeholder }
pub enum Codec { Placeholder }
pub struct EncoderCapability {}
pub trait CapabilityProbe: Send + Sync {}
```

`crates/media-policy/src/factory.rs`:
```rust
// Stub — populated in T3.
use crate::capability::BackendKind;
#[derive(Debug, thiserror::Error)]
pub enum FactoryError {
    #[error("unimplemented")]
    Unimplemented,
}
pub struct ProducerConfig {}
pub trait ProducerFactory: Send + Sync {
    fn create(&self, _kind: BackendKind, _cfg: &ProducerConfig)
        -> Result<Box<dyn prdt_protocol::VideoProducer>, FactoryError> {
        Err(FactoryError::Unimplemented)
    }
}
```

`crates/media-policy/src/selection.rs`:
```rust
// Stub — populated in T4.
use crate::capability::{BackendKind, EncoderCapability};
pub struct PolicyContext {}
pub struct HistoryTable {}
pub struct BackendStats {}
pub struct ScoringWeights {}
pub struct ScoringPolicy {}
pub trait SelectionPolicy: Send + Sync {
    fn rank(&self, _candidates: &[EncoderCapability], _ctx: &PolicyContext, _history: &HistoryTable)
        -> Vec<BackendKind> { Vec::new() }
}
```

`crates/media-policy/src/health.rs`:
```rust
// Stub — populated in T5.
pub enum HealthState { Placeholder }
pub enum HealthAction { Placeholder }
pub enum FailoverReason { Placeholder }
pub struct HealthMonitor {}
```

`crates/media-policy/src/driver.rs`:
```rust
// Stub — populated in T6.
pub struct PolicyDriven {}
```

- [ ] **Step 4: Add the new crate to the workspace members list**

Edit `Cargo.toml` (workspace root). Find the `[workspace] members = [` block (currently includes lines like `"crates/protocol",`) and append:

```toml
    "crates/media-policy",
```

(in alphabetical position; the workspace currently lists members in roughly insertion order — match the surrounding style).

- [ ] **Step 5: Add `ProducerError::DeviceLost` variant**

Edit `crates/protocol/src/video_pipeline.rs:11-19`. The current enum is:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProducerError {
    #[error("capture: {0}")]
    Capture(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("other: {0}")]
    Other(String),
}
```

Replace with:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProducerError {
    #[error("capture: {0}")]
    Capture(String),
    #[error("encode: {0}")]
    Encode(String),
    /// Backend permanently lost its device (driver crash, GPU hot-unplug,
    /// adapter removed). Carries a stable `backend` identifier and a free-form
    /// `reason`. PolicyDriven matches on this to trigger failover.
    #[error("device lost on {backend}: {reason}")]
    DeviceLost { backend: String, reason: String },
    #[error("other: {0}")]
    Other(String),
}
```

- [ ] **Step 6: Write the failing test for the new variant**

Append to `crates/protocol/src/video_pipeline.rs` (inside `mod tests`, after the existing `error_display` test):

```rust
    #[test]
    fn device_lost_display() {
        let e = ProducerError::DeviceLost {
            backend: "nvenc-h265".into(),
            reason: "DXGI_ERROR_DEVICE_REMOVED".into(),
        };
        assert_eq!(
            e.to_string(),
            "device lost on nvenc-h265: DXGI_ERROR_DEVICE_REMOVED",
        );
    }
```

- [ ] **Step 7: Run the build + tests on Linux**

Run: `cargo build -p prdt-media-policy -p prdt-protocol`
Expected: builds successfully (placeholder code compiles).

Run: `cargo test -p prdt-protocol video_pipeline::tests::device_lost_display`
Expected: `test video_pipeline::tests::device_lost_display ... ok`

Run: `cargo clippy --workspace -- -D warnings`
Expected: green (no new warnings).

Run: `cargo test --workspace --lib`
Expected: green (or only pre-existing flaky `transport::probe_test::two_transports_find_each_other`, which is unrelated and documented in STATUS).

- [ ] **Step 8: Commit**

```bash
git checkout -b phase-p5a-capability-policy
git add crates/media-policy/ Cargo.toml crates/protocol/src/video_pipeline.rs
git commit -m "P5A T1: prdt-media-policy crate skeleton + ProducerError::DeviceLost variant

- New crate prdt-media-policy with module stubs (capability/factory/
  selection/health/driver), populated in T2-T6.
- Add ProducerError::DeviceLost { backend, reason } typed variant for
  failover triggers (replaces fragile string matching).
- Workspace member added.
- 1 new test (device_lost_display)."
```

---

## Task 2: capability.rs — types + CapabilityProbe trait + MockProbe test

**Files:**
- Modify: `crates/media-policy/src/capability.rs` (replace stub with real impl)
- Test: `crates/media-policy/src/capability.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test (deterministic enumeration via MockProbe)**

Replace the stub `crates/media-policy/src/capability.rs` with:

```rust
//! Backend capability descriptors and probe trait.
//!
//! `CapabilityProbe` impls live in OS-specific backend crates
//! (`media-win::policy`, `media-linux::policy`, etc.). This file holds only
//! the platform-agnostic types they emit.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendKind {
    Nvenc,
    MfHevc,
    Openh264,
    // future: Vaapi, V4L2M2M, VideoToolbox, MediaCodec
}

impl BackendKind {
    /// Stable lowercase identifier for logs / config files.
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Nvenc => "nvenc",
            BackendKind::MfHevc => "mf-hevc",
            BackendKind::Openh264 => "openh264",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    H264,
    H265,
    // future: AV1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderCapability {
    pub backend: BackendKind,
    pub codec: Codec,
    pub max_resolution: (u32, u32), // (width, height)
    pub max_fps: u32,
    pub zero_copy: bool,
    /// OS-fixed default priority. NVENC=100, VAAPI=90, MfHevc=80, Openh264=10.
    pub priority: i32,
}

pub trait CapabilityProbe: Send + Sync {
    fn list_encoders(&self) -> Vec<EncoderCapability>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic in-memory probe used in unit + integration tests.
    pub struct MockProbe(pub Vec<EncoderCapability>);

    impl CapabilityProbe for MockProbe {
        fn list_encoders(&self) -> Vec<EncoderCapability> {
            self.0.clone()
        }
    }

    #[test]
    fn backend_kind_as_str_is_stable() {
        assert_eq!(BackendKind::Nvenc.as_str(), "nvenc");
        assert_eq!(BackendKind::MfHevc.as_str(), "mf-hevc");
        assert_eq!(BackendKind::Openh264.as_str(), "openh264");
    }

    #[test]
    fn mock_probe_returns_fixture() {
        let probe = MockProbe(vec![
            EncoderCapability {
                backend: BackendKind::Nvenc,
                codec: Codec::H265,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: true,
                priority: 100,
            },
            EncoderCapability {
                backend: BackendKind::Openh264,
                codec: Codec::H264,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: false,
                priority: 10,
            },
        ]);

        let caps = probe.list_encoders();
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].backend, BackendKind::Nvenc);
        assert_eq!(caps[1].backend, BackendKind::Openh264);
    }

    #[test]
    fn capability_round_trips_via_serde_json() {
        let cap = EncoderCapability {
            backend: BackendKind::MfHevc,
            codec: Codec::H265,
            max_resolution: (1920, 1080),
            max_fps: 60,
            zero_copy: true,
            priority: 80,
        };
        let json = serde_json::to_string(&cap).unwrap();
        let back: EncoderCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(back.backend, BackendKind::MfHevc);
        assert_eq!(back.max_resolution, (1920, 1080));
    }
}
```

- [ ] **Step 2: Run the tests to confirm they pass**

Run: `cargo test -p prdt-media-policy capability::tests`
Expected:
```
test capability::tests::backend_kind_as_str_is_stable ... ok
test capability::tests::mock_probe_returns_fixture ... ok
test capability::tests::capability_round_trips_via_serde_json ... ok
```

- [ ] **Step 3: Run clippy + workspace tests for regression**

Run: `cargo clippy -p prdt-media-policy -- -D warnings`
Expected: green.

Run: `cargo test --workspace --lib`
Expected: green (excluding pre-existing flaky probe_test).

- [ ] **Step 4: Commit**

```bash
git add crates/media-policy/src/capability.rs
git commit -m "P5A T2: capability.rs (BackendKind/Codec/EncoderCapability/CapabilityProbe)

- BackendKind enum (Nvenc/MfHevc/Openh264) with stable as_str().
- Codec enum (H264/H265).
- EncoderCapability descriptor (backend, codec, max_resolution, max_fps,
  zero_copy, priority).
- CapabilityProbe trait + MockProbe test impl.
- Serde derive on all public types so OS-specific probes can be tested
  with JSON fixtures in T7.
- 3 new tests (as_str stability, MockProbe enumeration, serde round-trip)."
```

---

## Task 3: factory.rs — ProducerFactory trait + FactoryError + ProducerConfig + MockFactory test

**Files:**
- Modify: `crates/media-policy/src/factory.rs` (replace stub with real impl)

- [ ] **Step 1: Replace the stub with the real implementation**

```rust
//! Producer factory trait. OS-specific factory impls live in backend crates.
//!
//! All errors during `create()` collapse into `FactoryError`. Once a producer
//! is constructed, runtime errors flow through `ProducerError` (defined in
//! `prdt-protocol`).

use crate::capability::{BackendKind, Codec};
use prdt_protocol::VideoProducer;

#[derive(Debug, thiserror::Error)]
pub enum FactoryError {
    #[error("backend {0:?} unavailable: {1}")]
    Unavailable(BackendKind, String),
    #[error("config invalid for backend {0:?}: {1}")]
    InvalidConfig(BackendKind, String),
}

#[derive(Debug, Clone)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use prdt_protocol::{EncodedFrame, ProducerError, VideoProducer};
    use std::sync::Mutex;

    /// In-memory producer that records every method call and returns a
    /// configurable result. Re-used in T6 integration test.
    pub struct ScriptedProducer {
        pub name: &'static str,
    }

    #[async_trait]
    impl VideoProducer for ScriptedProducer {
        async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
            Err(ProducerError::Other("scripted: not used in factory test".into()))
        }
        fn request_idr(&mut self) {}
        fn set_target_bitrate(&mut self, _bps: u32) {}
        fn backend_name(&self) -> &'static str { self.name }
    }

    /// Factory that returns one ScriptedProducer per call, recording every
    /// (kind, width, height, fps, bps) invocation.
    pub struct MockFactory {
        pub calls: Mutex<Vec<(BackendKind, u32, u32, u32, u32)>>,
    }

    impl ProducerFactory for MockFactory {
        fn create(
            &self,
            kind: BackendKind,
            cfg: &ProducerConfig,
        ) -> Result<Box<dyn VideoProducer>, FactoryError> {
            self.calls.lock().unwrap().push((
                kind, cfg.width, cfg.height, cfg.fps, cfg.initial_bitrate_bps,
            ));
            let name = match kind {
                BackendKind::Nvenc => "nvenc-mock",
                BackendKind::MfHevc => "mf-mock",
                BackendKind::Openh264 => "openh264-mock",
            };
            Ok(Box::new(ScriptedProducer { name }))
        }
    }

    #[test]
    fn mock_factory_records_call() {
        let f = MockFactory { calls: Mutex::new(vec![]) };
        let cfg = ProducerConfig {
            width: 1920, height: 1080, fps: 60,
            initial_bitrate_bps: 8_000_000, codec: Codec::H265,
        };
        let prod = f.create(BackendKind::Nvenc, &cfg).unwrap();
        assert_eq!(prod.backend_name(), "nvenc-mock");

        let calls = f.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], (BackendKind::Nvenc, 1920, 1080, 60, 8_000_000));
    }

    #[test]
    fn factory_error_display() {
        let e = FactoryError::Unavailable(BackendKind::Nvenc, "no NVIDIA driver".into());
        assert_eq!(e.to_string(), "backend Nvenc unavailable: no NVIDIA driver");
        let e2 = FactoryError::InvalidConfig(BackendKind::Openh264, "fps=0".into());
        assert_eq!(e2.to_string(), "config invalid for backend Openh264: fps=0");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p prdt-media-policy factory::tests`
Expected:
```
test factory::tests::mock_factory_records_call ... ok
test factory::tests::factory_error_display ... ok
```

- [ ] **Step 3: Run clippy + workspace tests**

Run: `cargo clippy -p prdt-media-policy -- -D warnings`
Run: `cargo test --workspace --lib`
Expected: both green.

- [ ] **Step 4: Commit**

```bash
git add crates/media-policy/src/factory.rs
git commit -m "P5A T3: factory.rs (ProducerFactory trait + ProducerConfig + FactoryError)

- ProducerFactory trait returning Box<dyn VideoProducer>; OS impls in T7.
- FactoryError {Unavailable, InvalidConfig} for boot-time failures (runtime
  errors flow through ProducerError instead).
- ProducerConfig captures width/height/fps/initial_bitrate_bps/codec.
- MockFactory + ScriptedProducer test helpers (re-used in T6).
- 2 new tests."
```

---

## Task 4: selection.rs — filter + ScoringPolicy + HistoryTable + 4+ tests including proptest

**Files:**
- Modify: `crates/media-policy/src/selection.rs` (replace stub with real impl)

- [ ] **Step 1: Replace the stub with the real implementation**

```rust
//! Selection policy: hard filter + soft score → ranked candidate list.
//!
//! The policy is deterministic given (candidates, context, history). All
//! mutable state lives in `HistoryTable`; the policy itself is `&self`.

use crate::capability::{BackendKind, EncoderCapability, Codec};
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
    pub fn new() -> Self { Self::default() }

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
        self.counts.get(&backend).and_then(|s| s.recent_encode_p95_us)
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
        s.failures += 1;
        s.last_failure_at = Some(now);
        // Exponential backoff capped at 300s.
        let prev = s.cooldown_until
            .and_then(|t| t.checked_duration_since(s.last_failure_at.unwrap_or(now)))
            .unwrap_or(Duration::from_secs(5));
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
        Self { priority: 0.45, zero_copy: 0.20, latency_fit: 0.25, reliability: 0.10 }
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
    pub fn new(weights: ScoringWeights) -> Self { Self { weights } }

    /// Reads `dirs::config_dir()/prdt/policy.toml` if present; falls back to
    /// defaults on any read/parse error. No CLI flag override in P5A.
    pub fn load_default_or_fallback() -> Self {
        let path = dirs::config_dir()
            .map(|d| d.join("prdt").join("policy.toml"));
        let weights = path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str::<ScoringWeights>(&s).ok())
            .unwrap_or_default();
        Self { weights }
    }
}

fn beta_posterior(s: u32, f: u32) -> f64 {
    // Beta(1,1) prior smoothing; cold start ⇒ 0.5
    (s as f64 + 1.0) / (s as f64 + f as f64 + 2.0)
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
                let priority_norm   = (cap.priority as f64 / 100.0).clamp(0.0, 1.0);
                let zero_copy_bonus = if cap.zero_copy { 1.0 } else { 0.0 };
                let runtime_p95_us  = history
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

        // Stable sort: descending by score, tie-break by BackendKind ordering
        // (which we make total via a manual key).
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| (a.0 as u8).cmp(&(b.0 as u8)))
        });
        scored.into_iter().map(|(k, _)| k).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(backend: BackendKind, codec: Codec, prio: i32, zc: bool) -> EncoderCapability {
        EncoderCapability {
            backend, codec,
            max_resolution: (3840, 2160), max_fps: 60,
            zero_copy: zc, priority: prio,
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
        // Openh264 (priority 10) gets a +0.5 hint bump; should beat NVENC.
        let candidates = vec![
            cap(BackendKind::Nvenc, Codec::H265, 100, true),
            cap(BackendKind::Openh264, Codec::H265, 10, false),
        ];
        let p = ScoringPolicy::new(ScoringWeights::default());
        let mut ctx = ctx_h265_1080p60();
        ctx.user_hint = Some(BackendKind::Openh264);
        let ranked = p.rank(&candidates, &ctx, &HistoryTable::new());
        assert_eq!(ranked[0], BackendKind::Openh264);
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
    fn beta_posterior_cold_start_is_half() {
        assert!((beta_posterior(0, 0) - 0.5).abs() < 1e-9);
    }

    proptest::proptest! {
        /// Property: for any input ordering of the same set of candidates,
        /// `rank` returns the same result. Determinism across shuffles.
        #[test]
        fn rank_is_invariant_under_input_shuffle(
            seed in 0u64..1000,
        ) {
            use proptest::prelude::*;
            let _ = seed; // explicit seed argument keeps proptest stable
            let mut candidates = vec![
                cap(BackendKind::Nvenc, Codec::H265, 100, true),
                cap(BackendKind::MfHevc, Codec::H265, 80, true),
                cap(BackendKind::Openh264, Codec::H265, 10, false),
            ];
            let p = ScoringPolicy::new(ScoringWeights::default());
            let baseline = p.rank(&candidates, &ctx_h265_1080p60(), &HistoryTable::new());
            // Reverse the input — same result.
            candidates.reverse();
            let reversed = p.rank(&candidates, &ctx_h265_1080p60(), &HistoryTable::new());
            prop_assert_eq!(baseline, reversed);
        }
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p prdt-media-policy selection::tests`
Expected (all 8 tests):
```
test selection::tests::rank_prefers_high_priority_zero_copy_backend ... ok
test selection::tests::rank_filters_codec_mismatch ... ok
test selection::tests::rank_force_sw_keeps_only_openh264 ... ok
test selection::tests::rank_user_override_strict_returns_only_that_backend ... ok
test selection::tests::rank_user_hint_promotes_chosen_backend_above_higher_priority ... ok
test selection::tests::cooldown_excludes_recently_failed_backend ... ok
test selection::tests::beta_posterior_cold_start_is_half ... ok
test selection::tests::rank_is_invariant_under_input_shuffle ... ok
```

- [ ] **Step 3: Run clippy + workspace tests**

Run: `cargo clippy -p prdt-media-policy -- -D warnings`
Run: `cargo test --workspace --lib`
Expected: both green.

- [ ] **Step 4: Commit**

```bash
git add crates/media-policy/src/selection.rs
git commit -m "P5A T4: selection.rs (filter + ScoringPolicy + HistoryTable + 8 tests)

- PolicyContext (target res/fps/bitrate/codec + user_override/hint/force_sw).
- HistoryTable (per-backend success/failure counts, last_failure_at,
  cooldown_until exp-backoff, recent_encode_p95_us snapshot from
  HealthMonitor).
- ScoringWeights deserialise + Default impl (0.45/0.20/0.25/0.10).
- ScoringPolicy::load_default_or_fallback reads
  \$config/prdt/policy.toml (no CLI override in P5A; deferred to §9).
- SelectionPolicy::rank: hard filter (codec/res/fps/cooldown/force_sw),
  user_override Strict, soft score (priority+zero_copy+latency_fit+
  reliability), user_hint +0.5 bump, deterministic tie-break.
- Beta(1,1) prior for cold-start reliability.
- 8 new tests including proptest shuffle invariance."
```

---

## Task 5: health.rs — state machine + HealthMonitor + 3+ tests with tokio::time::pause

**Files:**
- Modify: `crates/media-policy/src/health.rs` (replace stub with real impl)

- [ ] **Step 1: Replace the stub with the real implementation**

```rust
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
    LatencyDegradation { encode_p95_us: u64, frame_budget_us: u64 },
    ConsecutiveFailures { count: u32 },
    NoSuccessTimeout { idle_ms: u64 },
    DeviceLost { backend: String, reason: String },
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
    encode_p95_ema: f64,                  // microseconds
    consecutive_failures: u32,
    last_success_at: Instant,
    frame_budget_us: u64,
    deg_threshold_factor: f64,            // default 1.5
    rec_threshold_factor: f64,            // default 1.2
    deg_window_count_required: u32,       // default 3
    rec_window_count_required: u32,       // default 5
    consecutive_deg_windows: u32,
    consecutive_rec_windows: u32,
    failure_threshold: u32,               // default 3
    no_success_timeout: Duration,         // default 500ms
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

    pub fn current_state(&self) -> HealthState { self.state }
    pub fn encode_p95_ema(&self) -> u64 { self.encode_p95_ema as u64 }
    pub fn frame_budget_us(&self) -> u64 { self.frame_budget_us }

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
            self.encode_p95_ema =
                self.ema_alpha * x + (1.0 - self.ema_alpha) * self.encode_p95_ema;
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
                    backend: backend.clone(), reason: reason.clone(),
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

    fn budget_60fps() -> u64 { 1_000_000 / 60 } // 16,666 us

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
        for _ in 0..(30 * 3) { m.record_encode(30_000); }
        assert_eq!(m.current_state(), HealthState::Degraded);
        // Then 5 under-rec_threshold (1.2× budget = 20_000us; 10_000 is well under) windows.
        for _ in 0..(30 * 5) { m.record_encode(10_000); }
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
            Some(HealthAction::Failover { reason: FailoverReason::ConsecutiveFailures { count } }) => {
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
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p prdt-media-policy health::tests`
Expected (all 7 tests):
```
test health::tests::fresh_monitor_is_healthy ... ok
test health::tests::three_consecutive_overbudget_windows_trigger_degraded ... ok
test health::tests::five_consecutive_underbudget_windows_return_to_healthy ... ok
test health::tests::three_consecutive_failures_trigger_failing ... ok
test health::tests::device_lost_immediately_triggers_failover_lost ... ok
test health::tests::reset_for_new_backend_clears_state ... ok
test health::tests::successful_encode_resets_consecutive_failures ... ok
```

- [ ] **Step 3: Run clippy + workspace tests**

Run: `cargo clippy -p prdt-media-policy -- -D warnings`
Run: `cargo test --workspace --lib`
Expected: both green.

- [ ] **Step 4: Commit**

```bash
git add crates/media-policy/src/health.rs
git commit -m "P5A T5: health.rs (HealthMonitor state machine + 7 tests)

- HealthState: Healthy/Degraded/Failing/Lost.
- HealthAction: ReconfigureBitrate{factor:0.85} | Failover{reason}.
- FailoverReason: LatencyDegradation/ConsecutiveFailures/NoSuccessTimeout/
  DeviceLost.
- HealthMonitor with 30-frame windows, 1.5× deg / 1.2× rec hysteresis,
  3 failure threshold, 500ms no-success timeout, EMA alpha 1/31.
- record_encode evaluates window boundaries, handles
  Healthy→Degraded→Healthy.
- record_failure handles ProducerError::DeviceLost (immediate Lost) and
  consecutive-failure threshold (Failing).
- reset_for_new_backend used by PolicyDriven after swap.
- 7 new tests covering all transitions + reset semantics."
```

---

## Task 6: driver.rs — PolicyDriven impl VideoProducer + bootstrap + swap_to_next + integration test

**Files:**
- Modify: `crates/media-policy/src/driver.rs` (replace stub with real impl)
- Create: `crates/media-policy/tests/policy_driven_swap.rs` (integration test)

- [ ] **Step 1: Replace the stub with the real implementation**

```rust
//! `PolicyDriven` wraps any `Box<dyn VideoProducer>` and adds policy-driven
//! swap-on-failure. From the host's perspective it is just another
//! `VideoProducer` — same trait, same call sites.

use crate::capability::{BackendKind, Codec};
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
```

- [ ] **Step 2: Write the integration test (in tests/ directory)**

Create `crates/media-policy/tests/policy_driven_swap.rs`:

```rust
//! Integration test: scripted MockProducer A/B verifies that
//! DeviceLost on backend A causes PolicyDriven to swap to backend B
//! and the next next_frame() call succeeds via B.

use async_trait::async_trait;
use prdt_protocol::{EncodedFrame, ProducerError, VideoProducer};
use prdt_media_policy::{
    BackendKind, CapabilityProbe, Codec, EncoderCapability, FactoryError, PolicyContext,
    PolicyDriven, ProducerConfig, ProducerFactory, ScoringPolicy, ScoringWeights,
};
use bytes::Bytes;
use std::sync::{Arc, Mutex};

/// A scripted producer: each next_frame() call pops one entry from the
/// scripted result list. Empty result list returns Other("script exhausted").
struct ScriptedProducer {
    name: &'static str,
    script: Mutex<Vec<Result<(), ProducerError>>>, // () = success placeholder
    backend_name: &'static str,
}

impl ScriptedProducer {
    fn new(name: &'static str, backend_name: &'static str, script: Vec<Result<(), ProducerError>>) -> Self {
        Self { name, script: Mutex::new(script), backend_name }
    }
}

#[async_trait]
impl VideoProducer for ScriptedProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        let next = self.script.lock().unwrap().drain(..1).next();
        match next {
            None => Err(ProducerError::Other(format!("script exhausted for {}", self.name))),
            Some(Err(e)) => Err(e),
            Some(Ok(())) => Ok(EncodedFrame {
                stream_id: 0,
                pts_us: 0,
                is_keyframe: true,
                nal_units: Bytes::from_static(&[0u8, 0, 0, 1, 0x65]), // dummy IDR start code
            }),
        }
    }
    fn request_idr(&mut self) {}
    fn set_target_bitrate(&mut self, _bps: u32) {}
    fn backend_name(&self) -> &'static str { self.backend_name }
}

struct TwoBackendProbe;
impl CapabilityProbe for TwoBackendProbe {
    fn list_encoders(&self) -> Vec<EncoderCapability> {
        vec![
            EncoderCapability {
                backend: BackendKind::Nvenc,
                codec: Codec::H265,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: true,
                priority: 100,
            },
            EncoderCapability {
                backend: BackendKind::MfHevc,
                codec: Codec::H265,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: true,
                priority: 80,
            },
        ]
    }
}

/// Each call to create() pops one ScriptedProducer from the per-kind queue.
struct QueuedFactory {
    nvenc_queue: Mutex<Vec<ScriptedProducer>>,
    mf_queue: Mutex<Vec<ScriptedProducer>>,
}

impl ProducerFactory for QueuedFactory {
    fn create(
        &self,
        kind: BackendKind,
        _cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError> {
        let queue = match kind {
            BackendKind::Nvenc => &self.nvenc_queue,
            BackendKind::MfHevc => &self.mf_queue,
            _ => return Err(FactoryError::Unavailable(kind, "not in queue".into())),
        };
        let p = queue.lock().unwrap().drain(..1).next();
        p.map(|sp| Box::new(sp) as Box<dyn VideoProducer>)
            .ok_or_else(|| FactoryError::Unavailable(kind, "queue empty".into()))
    }
}

#[tokio::test]
async fn device_lost_on_nvenc_swaps_to_mf_and_recovers() {
    // NVENC scripted to fail with DeviceLost on the first call.
    // MF scripted to succeed on its first call.
    let nvenc = ScriptedProducer::new(
        "nvenc",
        "nvenc-h265",
        vec![
            Err(ProducerError::DeviceLost {
                backend: "nvenc-h265".into(),
                reason: "DXGI_ERROR_DEVICE_REMOVED".into(),
            }),
        ],
    );
    let mf = ScriptedProducer::new(
        "mf",
        "mf-h265",
        vec![Ok(())],
    );

    let factory = Arc::new(QueuedFactory {
        nvenc_queue: Mutex::new(vec![nvenc]),
        mf_queue: Mutex::new(vec![mf]),
    });
    let probe = Arc::new(TwoBackendProbe);
    let policy = Arc::new(ScoringPolicy::new(ScoringWeights::default()));

    let cfg = ProducerConfig {
        width: 1920, height: 1080, fps: 60,
        initial_bitrate_bps: 8_000_000, codec: Codec::H265,
    };
    let ctx = PolicyContext {
        target_resolution: (1920, 1080),
        target_fps: 60,
        target_bitrate_bps: 8_000_000,
        codec: Codec::H265,
        user_override: None,
        user_hint: None,
        force_sw: false,
    };

    let mut driven = PolicyDriven::bootstrap(probe, factory, policy, cfg, ctx)
        .expect("bootstrap should succeed (NVENC factory call OK)");

    // Sanity: bootstrap chose NVENC (priority 100 wins).
    assert_eq!(driven.backend_name(), "nvenc-h265");

    // First next_frame: NVENC returns DeviceLost; PolicyDriven should swap
    // to MF and retry. Outer call therefore returns Ok via MF.
    let frame = driven.next_frame().await.expect("after swap, MF should yield a frame");
    assert!(frame.is_keyframe);
    assert_eq!(driven.backend_name(), "mf-h265", "PolicyDriven should now be on MF");
}
```

- [ ] **Step 3: Add `bytes` to dev-deps so the test compiles**

Edit `crates/media-policy/Cargo.toml`, append to `[dev-dependencies]`:

```toml
bytes = "1"
```

(the version doesn't need to match transitive deps — `bytes::Bytes` is API-stable across 1.x).

- [ ] **Step 4: Run tests (unit + integration)**

Run: `cargo test -p prdt-media-policy`
Expected: all unit tests from T2-T5 plus the new integration test pass.

```
test driver::... (no inline tests; covered by integration)
test policy_driven_swap::device_lost_on_nvenc_swaps_to_mf_and_recovers ... ok
```

- [ ] **Step 5: Run clippy + workspace tests**

Run: `cargo clippy -p prdt-media-policy --all-targets -- -D warnings`
Run: `cargo test --workspace --lib`
Expected: both green.

- [ ] **Step 6: Commit**

```bash
git add crates/media-policy/src/driver.rs crates/media-policy/tests/policy_driven_swap.rs crates/media-policy/Cargo.toml
git commit -m "P5A T6: PolicyDriven (impl VideoProducer + bootstrap + swap_to_next) + integration test

- PolicyDriven holds Box<dyn VideoProducer>, intercepts next_frame errors,
  asks HealthMonitor for action, executes ReconfigureBitrate or
  swap_to_next.
- bootstrap: probe → rank → try create top candidates in order, return
  last FactoryError if all fail.
- swap_to_next: record failure in history (cooldown), re-rank, pick
  next != current backend, instantiate, hand off bitrate, request IDR,
  reset HealthMonitor.
- next_frame on inner Err with action: handle action and retry on the
  new backend. On Err without action: bubble up.
- Tracing structured fields: backend_chosen, state_transition, failover.
- Integration test: NVENC DeviceLost → swap to MF → next_frame succeeds
  via MF (in-process scripted producers + scripted factory)."
```

---

## Task 7: Backend integration (media-win/linux probe+factory) + host CLI + viewer overlay badge

**Files:**
- Create: `crates/media-win/src/policy.rs`
- Create: `crates/media-linux/src/policy.rs`
- Modify: `crates/media-win/src/lib.rs` (add `pub mod policy;`)
- Modify: `crates/media-linux/src/lib.rs` (add `pub mod policy;`)
- Modify: `crates/host/src/lib.rs` (replace direct producer construction with PolicyDriven::bootstrap; update DeviceLost match arm at lib.rs:518)
- Modify: `crates/host/src/main.rs` (add `--encoder-hint` and `--force-sw` clap flags; redefine `--encoder` semantics)
- Modify: `crates/host/src/platform/win.rs` (export `pub fn probe()` + `pub fn factory()`)
- Modify: `crates/host/src/platform/linux.rs` (same)
- Modify: `crates/viewer/src/lib.rs` (or wherever overlay rendering happens) — add backend badge

This task is the largest in the plan. It bridges `prdt-media-policy` to the existing host/viewer code without changing user-visible default behaviour.

- [ ] **Step 1: Add Cargo deps**

Edit `crates/media-win/Cargo.toml`, append to `[dependencies]`:

```toml
prdt-media-policy = { path = "../media-policy" }
```

Same for `crates/media-linux/Cargo.toml`.

Edit `crates/host/Cargo.toml`, append to `[dependencies]`:

```toml
prdt-media-policy = { path = "../media-policy" }
```

- [ ] **Step 2: Implement Linux probe + factory (smaller of the two; lets us validate cross-platform first)**

Create `crates/media-linux/src/policy.rs`:

```rust
//! P5A integration for Linux: only OpenH264 is available today.
//! VAAPI/V4L2/NVENC-Linux are P5C scope.

use prdt_media_policy::{
    BackendKind, CapabilityProbe, Codec, EncoderCapability, FactoryError, ProducerConfig,
    ProducerFactory,
};
use prdt_protocol::VideoProducer;

pub struct LinuxSwProbe;

impl CapabilityProbe for LinuxSwProbe {
    fn list_encoders(&self) -> Vec<EncoderCapability> {
        vec![EncoderCapability {
            backend: BackendKind::Openh264,
            codec: Codec::H264,
            max_resolution: (3840, 2160),
            max_fps: 60,
            zero_copy: false,
            priority: 10,
        }]
    }
}

pub struct LinuxSwFactory;

impl ProducerFactory for LinuxSwFactory {
    fn create(
        &self,
        kind: BackendKind,
        cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError> {
        if !matches!(kind, BackendKind::Openh264) {
            return Err(FactoryError::Unavailable(
                kind,
                "Linux only supports Openh264 in P5A".into(),
            ));
        }
        // Construct existing LinuxSwProducer. The exact constructor name lives
        // in `crates/media-linux/src/linux_sw_producer.rs`; if it differs from
        // the snippet below, mirror the same args.
        let producer = crate::linux_sw_producer::LinuxSwProducer::new(
            cfg.width, cfg.height, cfg.fps, cfg.initial_bitrate_bps,
        )
        .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?;
        Ok(Box::new(producer))
    }
}
```

Add to `crates/media-linux/src/lib.rs`:

```rust
pub mod policy;
```

(append in alphabetical position alongside existing `pub mod` statements).

- [ ] **Step 3: Implement Windows probe + factory**

Create `crates/media-win/src/policy.rs`:

```rust
//! P5A integration for Windows: NVENC + Media Foundation HEVC + OpenH264.

use prdt_media_policy::{
    BackendKind, CapabilityProbe, Codec, EncoderCapability, FactoryError, ProducerConfig,
    ProducerFactory,
};
use prdt_protocol::VideoProducer;

pub struct WindowsProbe;

impl CapabilityProbe for WindowsProbe {
    fn list_encoders(&self) -> Vec<EncoderCapability> {
        vec![
            EncoderCapability {
                backend: BackendKind::Nvenc,
                codec: Codec::H265,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: true,
                priority: 100,
            },
            EncoderCapability {
                backend: BackendKind::MfHevc,
                codec: Codec::H265,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: true,
                priority: 80,
            },
            EncoderCapability {
                backend: BackendKind::Openh264,
                codec: Codec::H264,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: false,
                priority: 10,
            },
        ]
    }
}

pub struct WindowsFactory;

impl ProducerFactory for WindowsFactory {
    fn create(
        &self,
        kind: BackendKind,
        cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError> {
        match kind {
            BackendKind::Nvenc => {
                let producer = crate::pipeline::DxgiNvencProducer::new(
                    cfg.width, cfg.height, cfg.fps, cfg.initial_bitrate_bps,
                )
                .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?;
                Ok(Box::new(producer))
            }
            BackendKind::MfHevc => {
                // The exact constructor for the MF-encoder producer lives in
                // crates/media-win/src/mf/. Mirror the same args; if the
                // current code only exposes it via with_encoder(), use that.
                let producer = crate::pipeline::DxgiMfProducer::new(
                    cfg.width, cfg.height, cfg.fps, cfg.initial_bitrate_bps,
                )
                .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?;
                Ok(Box::new(producer))
            }
            BackendKind::Openh264 => {
                // Use the existing CPU readback + Openh264 producer that
                // `--encoder openh264` already wires up on Windows.
                let producer = crate::pipeline::DxgiSwProducer::new(
                    cfg.width, cfg.height, cfg.fps, cfg.initial_bitrate_bps,
                )
                .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?;
                Ok(Box::new(producer))
            }
        }
    }
}
```

Add to `crates/media-win/src/lib.rs`:

```rust
pub mod policy;
```

> **Note:** if the actual constructor names in `crates/media-win/src/pipeline/mod.rs` differ from `DxgiNvencProducer::new` / `DxgiMfProducer::new` / `DxgiSwProducer::new` (e.g. they require a builder pattern with `with_encoder()`), keep the call shape the same as the existing `crates/host/src/platform/win.rs` does today. This task does not change pipeline internals; it merely re-routes construction.

- [ ] **Step 4: Add platform shim functions**

Edit `crates/host/src/platform/win.rs`, append:

```rust
use std::sync::Arc;
use prdt_media_policy::{CapabilityProbe, ProducerFactory};

pub fn probe() -> Arc<dyn CapabilityProbe> {
    Arc::new(prdt_media_win::policy::WindowsProbe)
}

pub fn factory() -> Arc<dyn ProducerFactory> {
    Arc::new(prdt_media_win::policy::WindowsFactory)
}
```

Edit `crates/host/src/platform/linux.rs`, append:

```rust
use std::sync::Arc;
use prdt_media_policy::{CapabilityProbe, ProducerFactory};

pub fn probe() -> Arc<dyn CapabilityProbe> {
    Arc::new(prdt_media_linux::policy::LinuxSwProbe)
}

pub fn factory() -> Arc<dyn ProducerFactory> {
    Arc::new(prdt_media_linux::policy::LinuxSwFactory)
}
```

- [ ] **Step 5: Add CLI flags to host main**

Locate the clap definition in `crates/host/src/main.rs` (search for `--encoder` to find it). The current flag is something like:

```rust
#[arg(long, default_value = "auto")]
encoder: String,
```

Replace with:

```rust
/// Encoder backend selection. "auto" lets PolicyDriven choose; specific
/// names ("nvenc", "mf", "openh264") force Strict mode (no failover).
#[arg(long, default_value = "auto")]
encoder: String,

/// Soft hint: prefer this backend if available, but failover is still
/// allowed. Mutually informative with --encoder; ignored if --encoder
/// is not "auto".
#[arg(long)]
encoder_hint: Option<String>,

/// Shorthand for --encoder openh264. Convenient for support cases.
#[arg(long, default_value_t = false)]
force_sw: bool,
```

In the function that maps these to `HostConfig`, parse `--encoder` into:

```rust
let (user_override, encoder_strict) = match cli.encoder.as_str() {
    "auto" => (None, false),
    "nvenc" => (Some(prdt_media_policy::BackendKind::Nvenc), true),
    "mf"    => (Some(prdt_media_policy::BackendKind::MfHevc), true),
    "openh264" => (Some(prdt_media_policy::BackendKind::Openh264), true),
    other => panic!("unknown --encoder value: {other}"),
};
let user_hint = cli.encoder_hint.as_deref().and_then(|s| match s {
    "nvenc" => Some(prdt_media_policy::BackendKind::Nvenc),
    "mf"    => Some(prdt_media_policy::BackendKind::MfHevc),
    "openh264" => Some(prdt_media_policy::BackendKind::Openh264),
    _ => None,
});
let force_sw = cli.force_sw;
// Extend HostConfig (defined in crates/host/src/lib.rs near the existing
// `pub struct HostConfig { ... }` declaration) with these new fields:
//
//     pub user_override: Option<prdt_media_policy::BackendKind>,
//     pub user_hint: Option<prdt_media_policy::BackendKind>,
//     pub force_sw: bool,
//
// Then populate them from the parsed values:
host_cfg.user_override = user_override;
host_cfg.user_hint = user_hint;
host_cfg.force_sw = force_sw;
let _ = encoder_strict; // keeps Strict mode opt-out; PolicyDriven::bootstrap
                        // already returns FactoryError if Strict's pick fails
```

- [ ] **Step 6: Wire PolicyDriven into host video task**

Edit `crates/host/src/lib.rs`, around the section that constructs the producer (look for `DxgiNvencProducer::new` or similar within the video task setup). Replace the construction with:

```rust
use std::sync::Arc;
use prdt_media_policy::{
    PolicyContext, PolicyDriven, ProducerConfig, ScoringPolicy,
};

let probe = crate::platform::probe();
let factory = crate::platform::factory();
let policy: Arc<dyn prdt_media_policy::SelectionPolicy> =
    Arc::new(ScoringPolicy::load_default_or_fallback());

let cfg = ProducerConfig {
    width: host_cfg.width,
    height: host_cfg.height,
    fps: host_cfg.fps,
    initial_bitrate_bps: host_cfg.initial_bitrate_bps,
    codec: host_cfg.codec, // existing field (already prdt-protocol Codec)
};
let ctx = PolicyContext {
    target_resolution: (host_cfg.width, host_cfg.height),
    target_fps: host_cfg.fps,
    target_bitrate_bps: host_cfg.initial_bitrate_bps,
    codec: host_cfg.codec,
    user_override: host_cfg.user_override,
    user_hint: host_cfg.user_hint,
    force_sw: host_cfg.force_sw,
};

let policy_driven = PolicyDriven::bootstrap(probe, factory, policy, cfg, ctx)
    .map_err(|e| anyhow::anyhow!("PolicyDriven bootstrap failed: {e}"))?;

let producer: Box<dyn prdt_protocol::VideoProducer> = Box::new(policy_driven);
// ↓ existing video task loop continues unchanged
```

> Map between `prdt-protocol::Codec` and `prdt-media-policy::Codec` may need a small `From` impl if they are separate types. If `host_cfg` already uses one of them and the other crate uses the other, add a thin conversion:
>
> ```rust
> fn to_policy_codec(c: prdt_protocol::Codec) -> prdt_media_policy::Codec {
>     match c {
>         prdt_protocol::Codec::H264 => prdt_media_policy::Codec::H264,
>         prdt_protocol::Codec::H265 => prdt_media_policy::Codec::H265,
>     }
> }
> ```

- [ ] **Step 7: Update the existing DeviceLost log site**

In `crates/host/src/lib.rs`, the existing match around line 518 (the producer error logging in the video loop) should be updated so `ProducerError::DeviceLost { .. }` is recognised:

```rust
match err {
    ProducerError::DeviceLost { ref backend, ref reason } => {
        warn!(backend, reason, "backend reported device lost; PolicyDriven handles failover internally");
    }
    other => warn!(?other, "producer error"),
}
```

- [ ] **Step 8: Add the viewer overlay badge**

In `crates/viewer/src/lib.rs` (search for where `present.backend_name` or stats are rendered), add a small helper:

```rust
fn backend_badge(backend_name: &str) -> &'static str {
    if backend_name.starts_with("nvenc") || backend_name.starts_with("mf") || backend_name.starts_with("vaapi") {
        "🚀 HW"
    } else {
        "💻 SW"
    }
}
```

And in the overlay rendering (likely `crates/viewer-overlay/src/`), augment the existing label string with `<badge> <backend_name>`. Color + glyph + text are all present (Gemini accessibility recommendation).

Concretely, where the overlay currently shows e.g. `Encoder: nvenc-h265`, change it to `Encoder: 🚀 HW nvenc-h265`. If the field was previously not exposed via the IPC stats file, add it to the stats writer in `crates/viewer/src/lib.rs` first; the overlay reads from that file.

- [ ] **Step 9: Run cross-platform builds**

Run: `cargo build --workspace`
Expected: green on Linux. (Windows step is in T8 manual smoke.)

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: green on Linux.

Run: `cargo test --workspace --lib`
Expected: green (excluding pre-existing flaky `transport::probe_test::two_transports_find_each_other`).

- [ ] **Step 10: Commit**

```bash
git add crates/media-win/src/policy.rs crates/media-win/src/lib.rs crates/media-win/Cargo.toml \
        crates/media-linux/src/policy.rs crates/media-linux/src/lib.rs crates/media-linux/Cargo.toml \
        crates/host/src/lib.rs crates/host/src/main.rs \
        crates/host/src/platform/win.rs crates/host/src/platform/linux.rs \
        crates/host/Cargo.toml \
        crates/viewer/src/lib.rs crates/viewer-overlay/src/
git commit -m "P5A T7: backend integration (media-win + media-linux) + host CLI + viewer overlay

- crates/media-win/src/policy.rs: WindowsProbe + WindowsFactory
  (NVENC priority 100, MF 80, Openh264 10).
- crates/media-linux/src/policy.rs: LinuxSwProbe + LinuxSwFactory
  (Openh264 only; VAAPI/V4L2/NVENC-Linux deferred to P5C).
- crates/host/src/platform/{win,linux}.rs: pub fn probe()/factory()
  shims returning Arc<dyn CapabilityProbe>/<dyn ProducerFactory>.
- crates/host CLI: --encoder {auto|nvenc|mf|openh264} (auto = policy,
  others = Strict no-failover), --encoder-hint <kind> (soft +0.5
  bump, failover OK), --force-sw shorthand.
- crates/host/src/lib.rs: PolicyDriven::bootstrap replaces direct
  producer construction; existing DeviceLost match arm typed.
- viewer overlay: 🚀 HW / 💻 SW badge in front of backend_name."
```

---

## Task 8: Manual smoke (Windows + Linux) + STATUS update + tag

**Files:**
- Modify: `docs/superpowers/STATUS.md`

This task has no automated test step. The two smoke scenarios verify the spec's DoD items 3, 4, 5, 6, 7.

- [ ] **Step 1: Linux smoke — verify probe + rank logged**

Build the Linux host:

```bash
cargo build --release -p prdt-host
```

Run with verbose tracing:

```bash
RUST_LOG=info ./target/release/prdt-host --bitrate-mbps 5 --silent-allow 2>&1 | grep -E "backend_chosen|state_transition|failover" | head -5
```

Expected: at least one `event=backend_chosen backend=Openh264 ranked=[Openh264] ...` line within the first second of startup.

Note the line for the STATUS entry below.

- [ ] **Step 2: Windows smoke (a) — NVENC fail → MF chosen**

(Run on a Windows machine with NVIDIA GPU available.)

Build:

```cmd
cargo build --release -p prdt-host
```

Inject NVENC factory failure for one boot. Easiest path: temporarily edit `crates/media-win/src/policy.rs` to return `FactoryError::Unavailable(BackendKind::Nvenc, "smoke test forced".into())` for `BackendKind::Nvenc`, rebuild, and run:

```cmd
set RUST_LOG=info
target\release\prdt-host.exe --bitrate-mbps 30 --silent-allow 2>&1 | findstr /C:"event=" 
```

Expected: tracing log shows `event=backend_chosen` choosing `MfHevc` (because NVENC factory failed), and a second host run with both NVENC+MF injection-failed shows OpenH264 chosen.

Revert the temporary policy.rs edit.

- [ ] **Step 3: Windows smoke (b) — runtime failover via DeviceLost**

(Optional, more complex.) Use a custom build that injects `ProducerError::DeviceLost` from the running NVENC producer after N frames (e.g. add a `#[cfg(feature = "p5a-smoke")]` counter). Verify the tracing log shows `event=failover from=Nvenc to=MfHevc reason=DeviceLost`.

This is documented as a smoke walkthrough; not required for DoD #3 (which only requires the boot-time variant from Step 2).

- [ ] **Step 4: CLI flag verification**

Both OS:

```bash
./prdt-host --encoder openh264 --bitrate-mbps 5  # Strict: openh264 only, no failover
./prdt-host --encoder-hint openh264 --bitrate-mbps 5  # auto + soft hint
./prdt-host --force-sw --bitrate-mbps 5  # shorthand for --encoder openh264
```

Expected for each: `backend_chosen` log shows the right pick.

- [ ] **Step 5: Viewer overlay badge check**

(Linux-side verifies cross-platform; Windows verifies HW path.)

Connect viewer to host, ESC to open overlay. Confirm the latency stats show a `🚀 HW <name>` or `💻 SW <name>` row (the exact phrasing depends on the existing overlay layout).

- [ ] **Step 6: Update STATUS.md**

Edit `docs/superpowers/STATUS.md`, change the **Last updated** and **Latest tag** lines:

```markdown
**Last updated:** 2026-05-11
**Latest tag:** `phase-p5a-capability-policy-complete`
```

Append under section 1 (Phase tag table) — find the L4 entry and add directly after:

```markdown
- **P5A (`phase-p5a-capability-policy-complete`, 2026-05-11)**: Capability/Policy
  layer for backend auto-selection + same-codec failover. New `prdt-media-policy`
  crate with 4 components (`CapabilityProbe`, `SelectionPolicy`, `HealthMonitor`,
  `ProducerFactory`) plus `PolicyDriven` wrapping `Box<dyn VideoProducer>`. New
  `ProducerError::DeviceLost { backend, reason }` typed variant replaces fragile
  string matching. Host CLI gains `--encoder-hint <kind>` and `--force-sw`;
  `--encoder auto` (default) is now policy-driven. Viewer overlay shows
  `🚀 HW`/`💻 SW` badge.
  - **Tests**: 8 SelectionPolicy + 7 HealthMonitor + 1 PolicyDriven integration +
    3 capability + 2 factory + 1 ProducerError = **22 new tests** cross-platform.
    Linux `cargo test --workspace` green.
  - **Linux smoke**: `event=backend_chosen backend=Openh264 ranked=[Openh264]` log
    confirmed at startup.
  - **Windows smoke (a)**: NVENC factory injection-failed → MF chosen via
    `event=backend_chosen backend=MfHevc`; then MF also failed → OpenH264.
  - **Out of scope (deferred)**: codec hot-swap (Phase 5 codec renegotiation),
    CSV telemetry writer (P9), viewer-side decoder PolicyDriven (P9), GUI
    `Force Software Encoder` toggle (Phase 4 GUI extension), AccessKit screen
    reader (Phase 4 GUI extension).
```

- [ ] **Step 7: Commit STATUS update**

```bash
git add docs/superpowers/STATUS.md
git commit -m "docs(STATUS): record P5A capability/policy layer + smoke notes"
```

- [ ] **Step 8: Open PR + merge to master + tag**

```bash
git push -u origin phase-p5a-capability-policy
gh pr create --title "P5A: capability/policy layer + same-codec failover" --body "$(cat <<'EOF'
## Summary
- New `prdt-media-policy` crate: CapabilityProbe / SelectionPolicy / HealthMonitor / ProducerFactory + PolicyDriven wrapper
- ProducerError::DeviceLost typed variant
- Host CLI: --encoder auto/strict, --encoder-hint, --force-sw
- Viewer overlay: 🚀 HW / 💻 SW badge

## Test plan
- [x] Linux: cargo test --workspace --lib green
- [x] Linux smoke: event=backend_chosen Openh264 logged
- [ ] Windows: workflow_dispatch CI green
- [ ] Windows smoke: NVENC injection-fail → MF chosen logged

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Wait for Windows CI green (existing GitHub Actions release workflow). Then:

```bash
gh pr merge --squash --delete-branch
git checkout master && git pull
git tag -a phase-p5a-capability-policy-complete -m "P5A: capability/policy layer + same-codec failover"
git push origin phase-p5a-capability-policy-complete
```

---

## Cross-task notes

- **Pre-existing flaky test:** `transport::probe_test::two_transports_find_each_other` is non-deterministic and unrelated to P5A. Do not treat it as a P5A regression. (Documented in STATUS L2 entry.)
- **MF-on-Windows known limitation:** NVIDIA's MF HEVC MFT ignores ICodecAPI bitrate hints (see STATUS `mf-encoder-fallback-complete`). PolicyDriven failover from NVENC → MF on a NVIDIA-only Windows host therefore ships with that pre-existing limitation; users with NVIDIA-only HW should use `--encoder nvenc` (Strict) to opt out of MF failover. Document in T8 STATUS entry.
- **`prdt-protocol::Codec` vs `prdt-media-policy::Codec`:** if both crates end up defining `Codec`, add a `From`/`Into` impl in `prdt-media-policy` (gated behind a `protocol` feature if needed) so the host wiring in T7 step 6 doesn't need a manual conversion table.
- **Viewer-side Codec from HelloAck:** the viewer currently fixes its decoder at session start. P5A on the host **only swaps within the same codec**, so the viewer's decoder commitment remains valid across a swap. This invariant is enforced by `swap_to_next` filtering candidates by `ctx.codec` (which is set once at bootstrap and never mutated).
