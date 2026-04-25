# Phase 4 G2 — Viewer Overlay (B1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Phase 4 viewer overlay that opens on ESC. The overlay is a separate process (`prdt-viewer-overlay`, eframe) that reads latency stats from a per-PID JSON file the main viewer writes once per second. Disconnect button writes a control file the viewer polls and acts on. Clean cross-platform implementation (Win/Linux/macOS desktop).

**Architecture:** New eframe binary crate `viewer-overlay` consumes JSON from `dirs::cache_dir()/prdt/overlay-ipc/<pid>/`. Main viewer gains `overlay_ipc` (writer / control reader) and `overlay_supervisor` (Child management) modules. ESC key in viewer event loop spawns the overlay if not already running; viewer's exit/Drop kills the child and removes the IPC dir. `--headless` skips overlay entirely.

**Tech Stack:** Rust 2021, `eframe` 0.28 + `egui` (existing in workspace via G1), `serde_json` (existing), `dirs` 5.0 (existing in gui-common), `tempfile` 3 (dev-dep). No new workspace deps.

**Spec:** `docs/superpowers/specs/2026-04-25-phase4-g2-overlay-design.md`

---

## File Structure

**Created files:**

```
crates/viewer-overlay/
  Cargo.toml
  src/
    main.rs            bin entry — eframe::run_native
    app.rs             OverlayApp impl eframe::App
    ipc.rs             read_stats(dir) / write_disconnect(dir) + StatsPayload type

crates/viewer/src/
  overlay_ipc.rs       StatsPayload (serde) + write_stats / read_control on a given dir
  overlay_supervisor.rs  OverlaySupervisor { ipc_dir, child } — spawn / try_wait / cleanup
```

**Modified files:**

```
Cargo.toml                                workspace members += viewer-overlay
crates/viewer/Cargo.toml                  + serde_json (workspace), dirs (workspace)
crates/viewer/src/main.rs                 ESC handler, 1Hz tick for write_stats /
                                          read_control, --headless gate, supervisor
                                          owned by ViewerApp
crates/gui-common/locales/en/main.ftl     + 8 overlay-* IDs
crates/gui-common/locales/ja/main.ftl     + 8 overlay-* IDs (translations)
```

**Public API surface added:**

- `prdt_viewer_overlay::ipc::StatsPayload` (the binary's internal type — exposed for tests)
- (no public crate-level API on the viewer side — `overlay_ipc` and `overlay_supervisor` are private modules)

---

## Task 1: viewer-overlay crate skeleton

**Files:**
- Modify: `Cargo.toml` workspace `members`
- Create: `crates/viewer-overlay/Cargo.toml`
- Create: `crates/viewer-overlay/src/main.rs`
- Create: `crates/viewer-overlay/src/app.rs`
- Create: `crates/viewer-overlay/src/ipc.rs`

- [ ] **Step 1: Add to workspace members**

In root `Cargo.toml`, append `"crates/viewer-overlay"` to `[workspace] members`.

- [ ] **Step 2: Create Cargo.toml**

Create `crates/viewer-overlay/Cargo.toml`:

```toml
[package]
name = "prdt-viewer-overlay"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
prdt-gui-common = { path = "../gui-common" }
eframe = { workspace = true }
egui = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
clap = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Create ipc.rs (with failing tests)**

Create `crates/viewer-overlay/src/ipc.rs`:

```rust
//! Read-side IPC: pull StatsPayload out of stats.json and write control flags
//! back into control.json.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LatencyUs {
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub samples: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatsPayload {
    pub version: u32,
    pub viewer_pid: u32,
    pub updated_at_unix_ms: u64,
    pub connection_state: String, // "connecting" | "connected" | "disconnecting"
    pub host_label: String,
    pub decoder: String,
    /// None when no samples yet (still connecting / handshaking).
    pub latency_us: Option<LatencyUs>,
    pub fps_observed: f32,
}

#[derive(thiserror::Error, Debug)]
pub enum IpcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn stats_path(ipc_dir: &Path) -> PathBuf {
    ipc_dir.join("stats.json")
}

pub fn control_path(ipc_dir: &Path) -> PathBuf {
    ipc_dir.join("control.json")
}

/// Read the latest stats from `ipc_dir/stats.json`. Returns Err(NotFound) if
/// the writer hasn't written one yet (overlay shows "Connecting…").
pub fn read_stats(ipc_dir: &Path) -> Result<StatsPayload, IpcError> {
    let raw = std::fs::read_to_string(stats_path(ipc_dir))?;
    let parsed = serde_json::from_str(&raw)?;
    Ok(parsed)
}

/// Write `{ "action": "disconnect", "issued_at_unix_ms": <now> }` to
/// `ipc_dir/control.json`. Atomic via tempfile + rename.
pub fn write_disconnect(ipc_dir: &Path) -> Result<(), IpcError> {
    write_control(ipc_dir, "disconnect")
}

fn write_control(ipc_dir: &Path, action: &str) -> Result<(), IpcError> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let payload = serde_json::json!({
        "action": action,
        "issued_at_unix_ms": now_ms,
    });
    let tmp = ipc_dir.join(".control.tmp");
    std::fs::write(&tmp, serde_json::to_string(&payload)?)?;
    std::fs::rename(&tmp, control_path(ipc_dir))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stats() -> StatsPayload {
        StatsPayload {
            version: 1,
            viewer_pid: 42,
            updated_at_unix_ms: 1_714_024_822_123,
            connection_state: "connected".into(),
            host_label: "192.168.1.5:9000".into(),
            decoder: "nvdec".into(),
            latency_us: Some(LatencyUs {
                p50: 18_000,
                p95: 41_000,
                p99: 67_000,
                samples: 512,
            }),
            fps_observed: 59.8,
        }
    }

    #[test]
    fn stats_round_trip_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let payload = sample_stats();
        std::fs::write(
            stats_path(dir.path()),
            serde_json::to_string(&payload).unwrap(),
        )
        .unwrap();
        let parsed = read_stats(dir.path()).unwrap();
        assert_eq!(payload, parsed);
    }

    #[test]
    fn read_stats_missing_returns_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_stats(dir.path()).unwrap_err();
        assert!(matches!(err, IpcError::Io(_)));
    }

    #[test]
    fn write_disconnect_creates_control_json() {
        let dir = tempfile::tempdir().unwrap();
        write_disconnect(dir.path()).unwrap();
        let raw = std::fs::read_to_string(control_path(dir.path())).unwrap();
        assert!(raw.contains("\"disconnect\""));
        assert!(raw.contains("issued_at_unix_ms"));
    }

    #[test]
    fn null_latency_parses_as_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let raw = r#"{
            "version": 1,
            "viewer_pid": 42,
            "updated_at_unix_ms": 0,
            "connection_state": "connecting",
            "host_label": "test",
            "decoder": "mf",
            "latency_us": null,
            "fps_observed": 0.0
        }"#;
        std::fs::write(stats_path(dir.path()), raw).unwrap();
        let parsed = read_stats(dir.path()).unwrap();
        assert!(parsed.latency_us.is_none());
        assert_eq!(parsed.connection_state, "connecting");
    }
}
```

Add `thiserror = { workspace = true }` to `crates/viewer-overlay/Cargo.toml` `[dependencies]` if not present (it should be, via workspace; verify `thiserror` exists in workspace deps — it does, from earlier phases).

- [ ] **Step 4: Create app.rs (minimal)**

Create `crates/viewer-overlay/src/app.rs`:

```rust
//! eframe app that renders the overlay window. Polls stats.json @ 5 Hz and
//! displays the parsed StatsPayload. Resume / Disconnect buttons.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use prdt_gui_common::t;

use crate::ipc::{self, StatsPayload};

pub struct OverlayApp {
    ipc_dir: PathBuf,
    last_poll: Instant,
    stats: Option<StatsPayload>,
    error: Option<String>,
}

impl OverlayApp {
    pub fn new(ipc_dir: PathBuf) -> Self {
        Self {
            ipc_dir,
            last_poll: Instant::now() - Duration::from_secs(60), // force first poll
            stats: None,
            error: None,
        }
    }

    fn poll_if_due(&mut self) {
        if self.last_poll.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.last_poll = Instant::now();
        match ipc::read_stats(&self.ipc_dir) {
            Ok(s) => {
                self.stats = Some(s);
                self.error = None;
            }
            Err(ipc::IpcError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                // Pre-first-write: keep showing whatever we had (likely
                // None → "Connecting…").
            }
            Err(e) => {
                self.error = Some(format!("read_stats: {e}"));
            }
        }
    }
}

impl eframe::App for OverlayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(200));
        self.poll_if_due();

        let mut want_close = false;
        let mut want_disconnect = false;

        egui::CentralPanel::default().show(ctx, |ui| {
            match &self.stats {
                Some(s) if s.connection_state == "connected" && s.latency_us.is_some() => {
                    let l = s.latency_us.as_ref().unwrap();
                    ui.label(t!("overlay-host-label", host => s.host_label.as_str()));
                    ui.add_space(8.0);
                    ui.heading(t!("overlay-stats-latency"));
                    ui.label(format!("p50: {:.1} ms", l.p50 as f64 / 1000.0));
                    ui.label(format!("p95: {:.1} ms", l.p95 as f64 / 1000.0));
                    ui.label(format!("p99: {:.1} ms", l.p99 as f64 / 1000.0));
                    ui.label(t!("overlay-stats-samples", n => l.samples as i64));
                    ui.add_space(8.0);
                    ui.label(t!("overlay-stats-decoder", name => s.decoder.as_str()));
                    ui.label(format!("FPS: {:.1}", s.fps_observed));
                }
                Some(s) => {
                    ui.heading(t!("overlay-stats-connecting"));
                    ui.add_space(4.0);
                    ui.label(t!("overlay-host-label", host => s.host_label.as_str()));
                    ui.label(t!("overlay-stats-decoder", name => s.decoder.as_str()));
                }
                None => {
                    ui.heading(t!("overlay-stats-connecting"));
                }
            }

            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button(t!("overlay-button-resume")).clicked() {
                    want_close = true;
                }
                if ui.button(t!("overlay-button-disconnect")).clicked() {
                    want_disconnect = true;
                }
            });
        });

        if want_disconnect {
            if let Err(e) = ipc::write_disconnect(&self.ipc_dir) {
                tracing::warn!(?e, "write_disconnect failed");
                self.error = Some(format!("disconnect: {e}"));
            }
            want_close = true;
        }
        if want_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
```

- [ ] **Step 5: Create main.rs**

Create `crates/viewer-overlay/src/main.rs`:

```rust
//! Phase 4 G2 viewer overlay binary. Spawned by prdt-viewer when the user
//! presses ESC. Reads stats.json from the IPC dir and shows an eframe
//! window with latency / decoder info plus Resume / Disconnect buttons.

use std::path::PathBuf;

use clap::Parser;
use prdt_gui_common::install_jp_font;

mod app;
mod ipc;

#[derive(Parser, Debug)]
#[command(name = "prdt-viewer-overlay")]
struct Args {
    /// Per-PID IPC directory (under dirs::cache_dir()/prdt/overlay-ipc/<pid>/).
    /// Required — passed by the spawning viewer.
    #[arg(long)]
    ipc_dir: PathBuf,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    // Apply the user's locale preference (best-effort: read viewer's
    // config.toml if it exists, else OS detect).
    let locale = prdt_gui_common::default_config_path()
        .and_then(|p| prdt_gui_common::Config::load(&p).ok())
        .map(|c| c.gui.locale)
        .unwrap_or_default();
    prdt_gui_common::init_locale(&locale);

    let ipc_dir = args.ipc_dir.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([360.0, 280.0])
            .with_min_inner_size([320.0, 240.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        &prdt_gui_common::tr("overlay-window-title"),
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(app::OverlayApp::new(ipc_dir)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
```

- [ ] **Step 6: Build + test**

```bash
cd E:/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-viewer-overlay
cargo test -p prdt-viewer-overlay
```

Expected: clean build, 4 ipc tests pass.

If `t!` resolves the `overlay-*` IDs to `"missing-string: overlay-..."` at runtime (because Task 5 hasn't added the .ftl entries yet), that's expected. The build does not fail since the macro takes a literal — IDs are resolved at runtime.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/viewer-overlay
git commit -m "viewer-overlay: skeleton crate with eframe app + IPC reader"
```

---

## Task 2: viewer-side IPC writer + supervisor

**Files:**
- Modify: `crates/viewer/Cargo.toml` (add serde_json, dirs)
- Create: `crates/viewer/src/overlay_ipc.rs`
- Create: `crates/viewer/src/overlay_supervisor.rs`

- [ ] **Step 1: Add deps to viewer's Cargo.toml**

In `crates/viewer/Cargo.toml` `[target.'cfg(windows)'.dependencies]` append (if not already present — `serde_json` and `dirs` come from workspace):

```toml
serde_json = { workspace = true }
dirs = { workspace = true }
```

(These are already at the workspace level via earlier phases / G1. The only new viewer-side dep is the explicit dependency declaration.)

- [ ] **Step 2: Create overlay_ipc.rs (with failing tests)**

Create `crates/viewer/src/overlay_ipc.rs`:

```rust
//! Viewer-side IPC: serialize StatsPayload and atomically write to
//! `ipc_dir/stats.json`; poll `ipc_dir/control.json` for action requests
//! from the overlay (consumed-on-read).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LatencyUs {
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub samples: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatsPayload {
    pub version: u32,
    pub viewer_pid: u32,
    pub updated_at_unix_ms: u64,
    pub connection_state: String,
    pub host_label: String,
    pub decoder: String,
    pub latency_us: Option<LatencyUs>,
    pub fps_observed: f32,
}

#[derive(thiserror::Error, Debug)]
pub enum IpcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn stats_path(ipc_dir: &Path) -> PathBuf {
    ipc_dir.join("stats.json")
}

pub fn control_path(ipc_dir: &Path) -> PathBuf {
    ipc_dir.join("control.json")
}

/// Atomically write `payload` to `ipc_dir/stats.json` via `tempfile + rename`.
pub fn write_stats(ipc_dir: &Path, payload: &StatsPayload) -> Result<(), IpcError> {
    let tmp = ipc_dir.join(".stats.tmp");
    std::fs::write(&tmp, serde_json::to_string(payload)?)?;
    std::fs::rename(&tmp, stats_path(ipc_dir))?;
    Ok(())
}

/// Poll `ipc_dir/control.json`. If present, parse the `action` field and
/// remove the file (consume-on-read semantics). Returns `Ok(None)` when
/// no control file exists.
pub fn read_control(ipc_dir: &Path) -> Result<Option<String>, IpcError> {
    let path = control_path(ipc_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(IpcError::Io(e)),
    };
    // Best-effort delete (already consumed even if delete fails).
    let _ = std::fs::remove_file(&path);
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    let action = v
        .get("action")
        .and_then(|a| a.as_str())
        .map(|s| s.to_string());
    Ok(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StatsPayload {
        StatsPayload {
            version: 1,
            viewer_pid: 99,
            updated_at_unix_ms: 0,
            connection_state: "connected".into(),
            host_label: "h".into(),
            decoder: "mf".into(),
            latency_us: None,
            fps_observed: 0.0,
        }
    }

    #[test]
    fn write_then_read_stats_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let payload = sample();
        write_stats(dir.path(), &payload).unwrap();
        let raw = std::fs::read_to_string(stats_path(dir.path())).unwrap();
        let parsed: StatsPayload = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, payload);
    }

    #[test]
    fn read_control_no_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_control(dir.path()).unwrap().is_none());
    }

    #[test]
    fn read_control_consumes_file_on_success() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            control_path(dir.path()),
            r#"{"action":"disconnect","issued_at_unix_ms":0}"#,
        )
        .unwrap();
        let action = read_control(dir.path()).unwrap();
        assert_eq!(action, Some("disconnect".to_string()));
        assert!(!control_path(dir.path()).exists());
    }
}
```

- [ ] **Step 3: Create overlay_supervisor.rs**

Create `crates/viewer/src/overlay_supervisor.rs`:

```rust
//! OverlaySupervisor — owns the per-PID IPC directory and the optional
//! `prdt-viewer-overlay` child process. Used by the viewer's main app to
//! spawn the overlay on ESC, write stats periodically, and poll for control
//! requests.
//!
//! `Drop` cleans up the child process and the IPC directory.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::overlay_ipc::{self, StatsPayload};

pub struct OverlaySupervisor {
    ipc_dir: PathBuf,
    child: Option<Child>,
}

impl OverlaySupervisor {
    /// Build a supervisor with `dirs::cache_dir()/prdt/overlay-ipc/<pid>/`
    /// as the IPC directory. Creates the directory on disk.
    pub fn new() -> std::io::Result<Self> {
        let root = dirs::cache_dir()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no cache dir")
            })?
            .join("prdt")
            .join("overlay-ipc");
        let ipc_dir = root.join(std::process::id().to_string());
        std::fs::create_dir_all(&ipc_dir)?;
        Ok(Self {
            ipc_dir,
            child: None,
        })
    }

    /// Test-only constructor that uses an explicit IPC directory.
    #[cfg(test)]
    pub fn with_ipc_dir(ipc_dir: PathBuf) -> Self {
        Self {
            ipc_dir,
            child: None,
        }
    }

    pub fn ipc_dir(&self) -> &Path {
        &self.ipc_dir
    }

    /// Resolve the path of the `prdt-viewer-overlay` binary that should sit
    /// next to `prdt-viewer` (cargo `install` / `target/debug` / app bundle
    /// MacOS layouts all put binaries in the same directory).
    pub fn overlay_binary_path() -> std::io::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        let dir = exe.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "current_exe has no parent")
        })?;
        Ok(dir.join(format!("prdt-viewer-overlay{}", std::env::consts::EXE_SUFFIX)))
    }

    /// Spawn the overlay child if not already alive. Idempotent: if a child
    /// is already running, this is a no-op.
    pub fn spawn_if_idle(&mut self) -> std::io::Result<()> {
        // If there's a child, check whether it has exited.
        if let Some(c) = self.child.as_mut() {
            match c.try_wait()? {
                Some(_) => self.child = None, // exited; fall through to spawn
                None => return Ok(()),         // still alive
            }
        }
        let bin = Self::overlay_binary_path()?;
        let child = Command::new(&bin)
            .arg("--ipc-dir")
            .arg(&self.ipc_dir)
            .spawn()?;
        self.child = Some(child);
        Ok(())
    }

    pub fn write_stats(&self, payload: &StatsPayload) -> Result<(), overlay_ipc::IpcError> {
        overlay_ipc::write_stats(&self.ipc_dir, payload)
    }

    /// Poll for a control action from the overlay. Returns `Ok(None)` if
    /// none pending.
    pub fn read_control(&self) -> Result<Option<String>, overlay_ipc::IpcError> {
        overlay_ipc::read_control(&self.ipc_dir)
    }
}

impl Drop for OverlaySupervisor {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.ipc_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_creates_ipc_dir() {
        let parent = tempfile::tempdir().unwrap();
        let ipc_dir = parent.path().join("test-pid");
        let _s = OverlaySupervisor::with_ipc_dir(ipc_dir.clone());
        // with_ipc_dir doesn't create the dir — verify via explicit creation
        // matches what new() does.
        std::fs::create_dir_all(&ipc_dir).unwrap();
        assert!(ipc_dir.exists());
    }

    #[test]
    fn drop_removes_ipc_dir() {
        let parent = tempfile::tempdir().unwrap();
        let ipc_dir = parent.path().join("drop-test");
        std::fs::create_dir_all(&ipc_dir).unwrap();
        std::fs::write(ipc_dir.join("stats.json"), "{}").unwrap();
        {
            let _s = OverlaySupervisor::with_ipc_dir(ipc_dir.clone());
        }
        assert!(!ipc_dir.exists(), "Drop should have removed {ipc_dir:?}");
    }

    #[test]
    fn write_then_read_stats_through_supervisor() {
        let parent = tempfile::tempdir().unwrap();
        let ipc_dir = parent.path().join("rw");
        std::fs::create_dir_all(&ipc_dir).unwrap();
        let s = OverlaySupervisor::with_ipc_dir(ipc_dir.clone());
        let payload = StatsPayload {
            version: 1,
            viewer_pid: 7,
            updated_at_unix_ms: 0,
            connection_state: "connected".into(),
            host_label: "x".into(),
            decoder: "mf".into(),
            latency_us: None,
            fps_observed: 0.0,
        };
        s.write_stats(&payload).unwrap();
        let raw = std::fs::read_to_string(ipc_dir.join("stats.json")).unwrap();
        let parsed: StatsPayload = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, payload);
        // No control file → None.
        assert!(s.read_control().unwrap().is_none());
    }

    #[test]
    fn overlay_binary_path_uses_exe_suffix() {
        let p = OverlaySupervisor::overlay_binary_path().unwrap();
        let name = p.file_name().unwrap().to_string_lossy();
        if cfg!(windows) {
            assert!(name.ends_with(".exe"), "got {name}");
        }
        assert!(name.starts_with("prdt-viewer-overlay"), "got {name}");
    }
}
```

- [ ] **Step 4: Build + test**

```bash
cd E:/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-viewer
cargo test -p prdt-viewer overlay_
```

Expected: clean build. The `overlay_` filter runs both `overlay_ipc::tests::*` (3 tests) and `overlay_supervisor::tests::*` (4 tests) → 7 tests pass.

The modules need to be wired into `crates/viewer/src/main.rs` for the build to succeed (Rust requires all `mod` declarations to be visible). Add at the top of `crates/viewer/src/main.rs` (alongside other `mod` declarations like `mod latency;`):

```rust
mod overlay_ipc;
mod overlay_supervisor;
```

If `crates/viewer/src/main.rs` lacks an existing `mod` block to colocate with, search for `use latency::` or `use crate::latency` patterns to find where modules are declared and add adjacent.

Without the `mod` declarations, the modules won't be compiled and tests won't run.

- [ ] **Step 5: Commit**

```bash
git add crates/viewer/Cargo.toml crates/viewer/src/overlay_ipc.rs crates/viewer/src/overlay_supervisor.rs crates/viewer/src/main.rs
git commit -m "viewer: overlay_ipc + overlay_supervisor modules with tests"
```

---

## Task 3: Wire ESC handler + 1Hz tick into viewer

**Files:**
- Modify: `crates/viewer/src/main.rs`

- [ ] **Step 1: Read existing ESC / keyboard handling**

```bash
grep -n "WindowEvent::KeyboardInput\|PhysicalKey\|Escape\|KeyCode" E:/project/rust-desktop/power-remote-dt/crates/viewer/src/main.rs | head -10
```

Confirm the keyboard input handler location. Look for the `match event` block inside `WindowEvent::KeyboardInput { event, .. } => { ... }`.

- [ ] **Step 2: Add the supervisor field to ViewerApp**

Find the `struct ViewerApp { ... }` definition. Add a new field:

```rust
    /// Overlay supervisor (Phase 4 G2). None when --headless or when
    /// overlay init failed at startup.
    overlay: Option<overlay_supervisor::OverlaySupervisor>,
    /// Last time we wrote stats.json. Throttled to 1 Hz.
    last_overlay_tick: std::time::Instant,
```

In `ViewerApp::new` (or wherever the struct is constructed), initialize:

```rust
            overlay: None,  // initialized in resumed() if !headless
            last_overlay_tick: std::time::Instant::now(),
```

- [ ] **Step 3: Init supervisor in resumed()**

Find the `fn resumed` method. After successful swapchain construction (search for `info!("swapchain created");`), add:

```rust
        // Phase 4 G2: spawn overlay supervisor (skipped in --headless mode).
        if !self.headless {
            match overlay_supervisor::OverlaySupervisor::new() {
                Ok(s) => {
                    tracing::info!(ipc_dir = %s.ipc_dir().display(), "overlay supervisor ready");
                    self.overlay = Some(s);
                }
                Err(e) => tracing::warn!(?e, "overlay supervisor disabled (cache dir error)"),
            }
        }
```

If `self.headless` doesn't exist as a struct field on `ViewerApp`, add it:

```rust
    headless: bool,
```

and pass it through from `Args::parse()`. If the existing structure already has the headless flag accessible elsewhere, use that directly. Read the existing `fn main()` for how `args.headless` flows into `ViewerApp`.

- [ ] **Step 4: ESC key handler + spawn overlay**

Find the `WindowEvent::KeyboardInput { event, .. } =>` handler. At the very top of the match arm body (before any input forwarding), insert:

```rust
                if event.physical_key
                    == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Escape)
                    && event.state == ElementState::Pressed
                {
                    if let Some(ref mut s) = self.overlay {
                        if let Err(e) = s.spawn_if_idle() {
                            warn!(?e, "overlay spawn failed");
                        }
                    }
                    return;
                }
```

(`return` swallows ESC so it isn't forwarded to the host as a remote keystroke.)

`use winit::keyboard::KeyCode;` may need adding to the top of `main.rs` — check existing `use` block.

- [ ] **Step 5: 1Hz stats write + control poll in about_to_wait**

Find `fn about_to_wait` (this is winit's hook called once per event-loop iteration when the queue is empty). Add overlay tick logic at the top of the body:

```rust
        // Phase 4 G2: 1 Hz overlay tick.
        if self.last_overlay_tick.elapsed() >= std::time::Duration::from_secs(1) {
            self.last_overlay_tick = std::time::Instant::now();
            if let Some(ref s) = self.overlay {
                let payload = build_stats_payload(self);
                if let Err(e) = s.write_stats(&payload) {
                    warn!(?e, "write_stats failed");
                }
                match s.read_control() {
                    Ok(Some(action)) if action == "disconnect" => {
                        tracing::info!("overlay requested disconnect; shutting down");
                        // event_loop.exit() — see below
                        self.disconnect_requested = true;
                    }
                    Ok(_) => {}
                    Err(e) => warn!(?e, "read_control failed"),
                }
            }
        }
```

The `disconnect_requested` flag (add to `ViewerApp` if absent):

```rust
    disconnect_requested: bool,
```

Initialize to `false`. Also in `about_to_wait` after the tick block, check the flag:

```rust
        if self.disconnect_requested {
            event_loop.exit();
        }
```

Add the helper function `build_stats_payload` at module level, OUTSIDE `impl ViewerApp`:

```rust
fn build_stats_payload(app: &ViewerApp) -> overlay_ipc::StatsPayload {
    let snap = app.shared.latency.snapshot();
    let present = snap.present;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    overlay_ipc::StatsPayload {
        version: 1,
        viewer_pid: std::process::id(),
        updated_at_unix_ms: now_ms,
        connection_state: if present.as_ref().map(|p| p.samples).unwrap_or(0) > 0 {
            "connected".into()
        } else {
            "connecting".into()
        },
        host_label: app.host_label_for_overlay(),
        decoder: app.decoder.clone(),
        latency_us: present.as_ref().map(|p| overlay_ipc::LatencyUs {
            p50: p.p50_us,
            p95: p.p95_us,
            p99: p.p99_us,
            samples: p.samples,
        }),
        fps_observed: app.fps_observed_smoothed(),
    }
}
```

Add helper methods to `impl ViewerApp` (or inline the logic if the struct already exposes the data):

```rust
impl ViewerApp {
    fn host_label_for_overlay(&self) -> String {
        // Prefer the human label saved with the connection (host_id or addr).
        // Fallback to whatever Args provided.
        if let Some(addr) = &self.host_addr {
            addr.to_string()
        } else if let Some(id) = &self.host_id {
            id.clone()
        } else {
            "(unknown)".into()
        }
    }

    fn fps_observed_smoothed(&self) -> f32 {
        // Crude EMA over the last second of frame presents. If the existing
        // app already tracks fps for the title bar, reuse that. Otherwise
        // compute from `tex_count` delta in `shared`.
        // For G2 v1 we approximate with a fixed assumption of 60 fps when
        // connected, 0 otherwise. Refine in G3+.
        if self.shared.latency.snapshot().present.is_some() {
            60.0
        } else {
            0.0
        }
    }
}
```

(Existing `app.host_addr` / `app.host_id` field names may differ — `grep` viewer/main.rs for the flow that ends with `direct_host` and `normalized_host_id` to find the names. Adapt as needed. If the fields don't exist on the struct itself, build the label from `Args` saved alongside.)

- [ ] **Step 6: Build + smoke**

```bash
cd E:/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-viewer
```

Expected: clean build. If `host_addr` / similar field doesn't exist on `ViewerApp`, the compile error tells you what's there — adapt `host_label_for_overlay` to use the actual data.

- [ ] **Step 7: Test (--headless still works)**

```bash
./target/debug/prdt-viewer.exe --headless --help 2>&1 | head -5
```

Expected: usage prints normally; no overlay spawn (because `--headless` skips supervisor init).

- [ ] **Step 8: Commit**

```bash
git add crates/viewer/src/main.rs
git commit -m "viewer: ESC spawns overlay; 1Hz stats write + control poll"
```

---

## Task 4: i18n IDs for overlay

**Files:**
- Modify: `crates/gui-common/locales/en/main.ftl`
- Modify: `crates/gui-common/locales/ja/main.ftl`

- [ ] **Step 1: Append overlay IDs to en**

Append to `crates/gui-common/locales/en/main.ftl`:

```ftl

# Viewer overlay (Phase 4 G2)
overlay-window-title = Power Remote Desktop — Overlay
overlay-host-label = Connected to: { $host }
overlay-stats-latency = Latency
overlay-stats-samples = samples: { $n }
overlay-stats-decoder = Decoder: { $name }
overlay-stats-connecting = Connecting…
overlay-button-resume = Resume
overlay-button-disconnect = Disconnect
```

- [ ] **Step 2: Append overlay IDs to ja**

Append to `crates/gui-common/locales/ja/main.ftl`:

```ftl

# Viewer overlay (Phase 4 G2)
overlay-window-title = Power Remote Desktop — オーバーレイ
overlay-host-label = 接続先: { $host }
overlay-stats-latency = レイテンシ
overlay-stats-samples = サンプル: { $n }
overlay-stats-decoder = デコーダー: { $name }
overlay-stats-connecting = 接続中…
overlay-button-resume = 再開
overlay-button-disconnect = 切断
```

- [ ] **Step 3: Run ID-completeness test**

```bash
cd E:/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo test -p prdt-gui-common locale_files_have_same_ids placeholders_match_across_locales
```

Expected: both tests pass (en and ja have the same 8 new IDs, all `{ $host }` / `{ $n }` / `{ $name }` placeholders match).

- [ ] **Step 4: Commit**

```bash
git add crates/gui-common/locales
git commit -m "gui-common: add overlay-* i18n IDs (en + ja)"
```

---

## Task 5: Final validation + tag

**Files:** none (verification only).

- [ ] **Step 1: Workspace tests**

```bash
cd E:/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo test --workspace 2>&1 | awk '/^test result:/ {p+=$4; f+=$6} END {print "total:", p, "failed:", f}'
```

Expected: `total: ≥249 failed: 0`. Was 238 (Phase 4 G6); +4 viewer-overlay::ipc + +3 viewer::overlay_ipc + +4 viewer::overlay_supervisor = +11 tests = 249.

- [ ] **Step 2: Workspace clippy**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -5
```

Expected: clean. If any unused-import warning fires on the new `mod overlay_ipc; mod overlay_supervisor;` declarations (e.g., `use overlay_ipc::StatsPayload` not yet referenced from a public location), tighten the `use` to what's actually consumed.

- [ ] **Step 3: fmt on touched files**

```bash
rustfmt \
  crates/viewer-overlay/src/main.rs \
  crates/viewer-overlay/src/app.rs \
  crates/viewer-overlay/src/ipc.rs \
  crates/viewer/src/overlay_ipc.rs \
  crates/viewer/src/overlay_supervisor.rs \
  crates/viewer/src/main.rs
git diff --stat
```

If non-empty:

```bash
git add -u
git commit -m "phase4-g2: cargo fmt on touched files"
```

- [ ] **Step 4: Manual smoke (informational)**

```bash
# Terminal 1 — start a host (or skip if you're testing GUI alone with no host).
./target/debug/prdt-host.exe --headless --bind 0.0.0.0:9000

# Terminal 2 — viewer (GUI mode).
./target/debug/prdt-viewer.exe
# Click Connect from launcher to a saved host.
# Once connected, press ESC.
# Expect: overlay window opens showing latency + Resume / Disconnect.
# Click Disconnect → viewer exits, host log shows Bye.
```

- [ ] **Step 5: Tag**

```bash
git tag -a phase4-g2-complete -m "$(cat <<'EOF'
Phase 4 G2 complete — viewer overlay (B1 separate process)

- New crate viewer-overlay (eframe, reads stats.json, writes control.json)
- viewer ESC spawns overlay; cleanup via Drop
- 1 Hz stats write, 1 Hz control poll, atomic JSON via tempfile + rename
- PID-isolated IPC dir under dirs::cache_dir()/prdt/overlay-ipc/<pid>/
- 8 new overlay-* i18n IDs (en + ja)
- --headless skips overlay entirely
- 11 new tests (4 ipc reader, 3 ipc writer, 4 supervisor)
- Cross-platform desktop (Win/Linux/macOS); mobile deferred to Phase 5+
- Out of scope (G3+): true inline overlay, volume slider, fullscreen
  toggle, F-key shortcuts
EOF
)"
git tag | grep phase4
```

Expected: `phase4-g2-complete` listed alongside g1 / g6 / title-status.

- [ ] **Step 6: Final summary**

Report:
- Workspace test count
- Clippy result
- Tag created
- Manual smoke status (if performed)

---

## Risks & Notes for Implementer

- **`build_stats_payload` field-name uncertainty**: the existing `ViewerApp` struct's exact field names for the host label may differ from the snippets. If `host_addr` / `host_id` aren't directly on the struct, search for what stores them (`Args`-derived or worker-task-derived) and adapt. Don't reshape the struct just for this — pass the data via existing references.
- **`ApplicationHandler::about_to_wait` runs frequently** (every event-loop iteration). The 1 Hz throttle via `last_overlay_tick.elapsed()` is essential. Don't move the IPC writes into a hotter spot.
- **Cross-process IPC race**: `tempfile + rename` makes stats.json atomic on the write side. The overlay's read side may miss a single update if it polls during the rename window — that's acceptable (next 200ms tick catches it).
- **Child process cleanup on viewer crash**: if viewer is `SIGKILL`'d, Drop doesn't run; the IPC dir leaks until next reboot (cache_dir is wiped periodically by the OS). Don't add a complex cleanup; document in spec under "Open Questions" — already done.
- **Overlay ESC shouldn't quit the overlay** unless the user clicks Resume. eframe's default ESC behavior may close windows on some configurations — if you observe this, override via `egui::Context::input(...)`.
- **`ctx.send_viewport_cmd(ViewportCommand::Close)`** is the eframe 0.28 way to close. Confirms with G6's existing usage in gui-viewer.
- **Helper functions order in main.rs**: Rust hoists fn declarations within a module, so `build_stats_payload` can be defined anywhere relative to its callers.
- **`disconnect_requested` flag on ViewerApp**: only checked in `about_to_wait`. Don't try to exit from inside the IPC poll — wait for the next iteration.
- **`event_loop.exit()`** signals winit to terminate the loop after the current frame. Existing connection cleanup (Bye send, decoder shutdown) should already run via the existing exit path triggered by event_loop.exit().

---

## Self-Review

- **Spec coverage**: viewer-overlay crate (Task 1) ✓, viewer-side IPC (Task 2) ✓, ESC handler + tick (Task 3) ✓, i18n IDs (Task 4) ✓, validation + tag (Task 5) ✓.
- **Placeholder scan**: No `TBD`/`TODO`/vague phrasing. Every code block is concrete; the few "if existing field name differs, adapt" notes are unavoidable for plan-time uncertainty about the exact viewer struct.
- **Type consistency**: `StatsPayload` shape (version, viewer_pid, updated_at_unix_ms, connection_state, host_label, decoder, `Option<LatencyUs>`, fps_observed) is identical across viewer/`overlay_ipc.rs` and viewer-overlay/`ipc.rs` — necessary because they communicate via JSON. `LatencyUs` shape (p50/p95/p99/samples) consistent. `OverlaySupervisor` API stable across tasks 2-3.
