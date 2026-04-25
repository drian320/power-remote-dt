# Phase 4 G5 — Crash Reporter + Signing Scaffolding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a crash reporter (panic_hook → JSON dump under `dirs::cache_dir()/prdt/crashes/`) to the three GUI binaries, surface pending crashes in the host's Settings UI, and ship a `signtool` PowerShell wrapper + cert-procurement docs so the team can sign + release the MSI once a certificate is acquired.

**Architecture:** New `prdt_gui_common::crashlog` module owns `install_panic_hook`, `list_pending_crashes`, `mark_acknowledged`, plus an optional `register_tail(TailHandle)` so dumps include the most recent log lines from G1's TailLayer. Each GUI binary calls `install_panic_hook` from `main()`. The gui-host's `run_host_gui` calls `list_pending_crashes()` once at startup; results live on `HostApp::pending_crashes` and the Settings modal renders a banner above the existing G4 update banner. Code signing is scaffolding only: a `scripts/sign-msi.ps1` PowerShell script + `docs/sign-and-release.md` cert procurement guide.

**Tech Stack:** Rust 2021, `chrono` 0.4 (new workspace dep, ISO8601 timestamps), `serde_json` (existing), the existing `prdt_gui_common::TailHandle` for log line snapshot. Signing: PowerShell + `signtool.exe` from Windows SDK (no Rust deps).

**Spec:** `docs/superpowers/specs/2026-04-25-phase4-g5-crash-signing-design.md`

---

## File Structure

**Created files:**

```
crates/gui-common/src/
  crashlog.rs                       install_panic_hook, list_pending_crashes,
                                    mark_acknowledged, register_tail, CrashReport,
                                    env override for tests, 5 unit tests

scripts/
  sign-msi.ps1                      signtool sign + verify wrapper

docs/
  sign-and-release.md               Cert procurement (EV vs OV) + sign-msi.ps1
                                    usage + release checklist
```

**Modified files:**

```
Cargo.toml                          + chrono workspace dep
crates/gui-common/Cargo.toml        + chrono runtime dep
crates/gui-common/src/lib.rs        + pub mod crashlog + re-exports

crates/host/src/main.rs             + crashlog::install_panic_hook (init_tracing 直後)
crates/viewer/src/main.rs           replace inline panic_hook with crashlog
crates/viewer-overlay/src/main.rs   + crashlog::install_panic_hook

crates/gui-host/src/lib.rs          + register_tail + list_pending_crashes,
                                    pass into HostApp
crates/gui-host/src/app.rs          + pending_crashes field + initializer arg
crates/gui-host/src/settings.rs     + Pending crashes section + Open folder +
                                    Acknowledge all buttons; signature gains
                                    pending_crashes parameter

crates/gui-common/locales/en/main.ftl   + 4 crashlog-* IDs
crates/gui-common/locales/ja/main.ftl   same

docs/build-msi.md                   + "Sign the MSI" section pointing at
                                    scripts/sign-msi.ps1 and sign-and-release.md
```

**Public API surface added:**

- `prdt_gui_common::crashlog::CrashReport` (serde-serializable)
- `prdt_gui_common::crashlog::install_panic_hook(binary_name, version)`
- `prdt_gui_common::crashlog::register_tail(TailHandle)`
- `prdt_gui_common::crashlog::list_pending_crashes() -> std::io::Result<Vec<CrashReport>>`
- `prdt_gui_common::crashlog::mark_acknowledged(timestamp_iso, binary) -> std::io::Result<()>`
- `prdt_gui_common::crashlog::crashes_dir() -> Option<PathBuf>`
- Re-exports at crate root: `install_panic_hook`, `register_tail`, `list_pending_crashes`, `mark_acknowledged`, `CrashReport`

---

## Task 1: gui-common::crashlog module

**Files:**
- Modify: `Cargo.toml` (workspace deps)
- Modify: `crates/gui-common/Cargo.toml`
- Create: `crates/gui-common/src/crashlog.rs`
- Modify: `crates/gui-common/src/lib.rs`

- [ ] **Step 1: Add chrono workspace dep**

In root `Cargo.toml`, append to `[workspace.dependencies]`:

```toml
# Phase 4 G5 (crash reporter timestamps)
chrono = { version = "0.4", default-features = false, features = ["clock", "serde", "std"] }
```

- [ ] **Step 2: Add chrono to gui-common Cargo.toml**

In `crates/gui-common/Cargo.toml` `[dependencies]` append:

```toml
chrono = { workspace = true }
serde_json = { workspace = true }
```

(`serde_json` may already be in the gui-common deps from earlier work; check first with `grep "serde_json" crates/gui-common/Cargo.toml`. If absent, add the workspace ref.)

- [ ] **Step 3: Create crashlog.rs**

Create `crates/gui-common/src/crashlog.rs`:

```rust
//! Phase 4 G5 crash reporter. Installs a `std::panic::set_hook` that writes
//! a JSON dump under `dirs::cache_dir()/prdt/crashes/<timestamp>-<binary>-<pid>.json`,
//! including the most recent log lines (when a `TailHandle` is registered
//! via `register_tail`). The host GUI's Settings reads pending dumps with
//! `list_pending_crashes` and moves them to `crashes/acknowledged/` via
//! `mark_acknowledged` once the user has seen them.
//!
//! Native exceptions (e.g. DXGI_DEVICE_REMOVED bypassing Rust panics) are
//! NOT covered — see `known_limitations.md` §7. Phase 5 may add minidump
//! support.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::TailHandle;

/// Optional log-line provider. If set before a panic, recent lines are
/// included in the JSON dump.
static TAIL: OnceLock<TailHandle> = OnceLock::new();

/// Provide a TailHandle so future panic dumps include recent log lines.
/// Idempotent — only the first call wins.
pub fn register_tail(tail: TailHandle) {
    let _ = TAIL.set(tail);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrashReport {
    pub binary: String,
    pub version: String,
    pub timestamp_iso: String,
    pub panic_message: String,
    pub panic_location: String,
    #[serde(default)]
    pub recent_log_lines: Vec<String>,
}

/// Install a panic hook that writes a JSON dump on every Rust panic.
/// `binary_name` and `version` are typically `env!("CARGO_PKG_NAME")` and
/// `env!("CARGO_PKG_VERSION")` from the binary calling this.
pub fn install_panic_hook(binary_name: &'static str, version: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        let report = build_report(binary_name, version, info);
        if let Err(e) = write_report(&report) {
            // We're already in a panic; tracing might also be torn down.
            eprintln!("crashlog: failed to write report: {e}");
        }
        // Mirror the existing tracing-subscriber behaviour from the
        // pre-G5 viewer hook so logs still capture panics inline.
        tracing::error!(
            binary = report.binary,
            location = report.panic_location,
            message = report.panic_message,
            "PANIC"
        );
    }));
}

fn build_report(binary: &str, version: &str, info: &std::panic::PanicHookInfo) -> CrashReport {
    let panic_message = match info.payload().downcast_ref::<&'static str>() {
        Some(s) => (*s).to_string(),
        None => info
            .payload()
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "panic with non-string payload".to_string()),
    };
    let panic_location = info
        .location()
        .map(|l| format!("{}:{}", l.file(), l.line()))
        .unwrap_or_else(|| "unknown".to_string());
    let recent_log_lines = TAIL.get().map(|h| h.snapshot()).unwrap_or_default();
    CrashReport {
        binary: binary.to_string(),
        version: version.to_string(),
        timestamp_iso: chrono::Utc::now().to_rfc3339(),
        panic_message,
        panic_location,
        recent_log_lines,
    }
}

/// Resolve the crashes directory, honoring the `PRDT_CRASHLOG_DIR` env
/// override (used by tests). Falls back to `dirs::cache_dir()/prdt/crashes/`.
pub fn crashes_dir() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("PRDT_CRASHLOG_DIR") {
        return Some(PathBuf::from(s));
    }
    dirs::cache_dir().map(|d| d.join("prdt").join("crashes"))
}

fn write_report(report: &CrashReport) -> std::io::Result<PathBuf> {
    let dir = crashes_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no cache dir")
    })?;
    write_report_to(report, &dir)
}

fn write_report_to(report: &CrashReport, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!(
        "{}-{}-{}.json",
        stamp,
        report.binary,
        std::process::id()
    ));
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// List unacknowledged reports, newest first.
pub fn list_pending_crashes() -> std::io::Result<Vec<CrashReport>> {
    let Some(dir) = crashes_dir() else {
        return Ok(Vec::new());
    };
    list_pending_in(&dir)
}

fn list_pending_in(dir: &Path) -> std::io::Result<Vec<CrashReport>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths.reverse();
    let mut out = Vec::new();
    for p in paths {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(r) = serde_json::from_str::<CrashReport>(&s) {
                out.push(r);
            }
        }
    }
    Ok(out)
}

/// Move a matching report into `crashes/acknowledged/`. Returns NotFound
/// if no report has the given (timestamp, binary) pair.
pub fn mark_acknowledged(timestamp_iso: &str, binary: &str) -> std::io::Result<()> {
    let Some(dir) = crashes_dir() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no cache dir",
        ));
    };
    mark_acknowledged_in(&dir, timestamp_iso, binary)
}

fn mark_acknowledged_in(dir: &Path, timestamp_iso: &str, binary: &str) -> std::io::Result<()> {
    let acked = dir.join("acknowledged");
    std::fs::create_dir_all(&acked)?;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if !p.is_file() || p.extension().map(|e| e != "json").unwrap_or(true) {
            continue;
        }
        let s = match std::fs::read_to_string(&p) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Ok(r) = serde_json::from_str::<CrashReport>(&s) {
            if r.timestamp_iso == timestamp_iso && r.binary == binary {
                let dest = acked.join(p.file_name().expect("file name"));
                std::fs::rename(&p, dest)?;
                return Ok(());
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no matching crash report",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report(binary: &str, ts: &str) -> CrashReport {
        CrashReport {
            binary: binary.to_string(),
            version: "0.0.1-test".to_string(),
            timestamp_iso: ts.to_string(),
            panic_message: "boom".to_string(),
            panic_location: "src/main.rs:42".to_string(),
            recent_log_lines: vec!["INFO test: started".to_string()],
        }
    }

    #[test]
    fn json_round_trip_preserves_fields() {
        let r = sample_report("prdt-host", "2026-04-25T12:00:00Z");
        let json = serde_json::to_string(&r).unwrap();
        let back: CrashReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn write_then_list_returns_one_report() {
        let dir = tempfile::tempdir().unwrap();
        let r = sample_report("prdt-host", "2026-04-25T12:00:00Z");
        write_report_to(&r, dir.path()).unwrap();
        let pending = list_pending_in(dir.path()).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].binary, "prdt-host");
        assert_eq!(pending[0].panic_message, "boom");
    }

    #[test]
    fn list_pending_in_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        let pending = list_pending_in(&missing).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn list_pending_in_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        // Names sort lex; we want newest first → the file with the larger
        // numeric prefix should appear at index 0.
        let older = sample_report("prdt-host", "2026-04-25T10:00:00Z");
        std::fs::write(
            dir.path().join("20260425-100000-prdt-host-1.json"),
            serde_json::to_string(&older).unwrap(),
        )
        .unwrap();
        let newer = sample_report("prdt-viewer", "2026-04-25T15:00:00Z");
        std::fs::write(
            dir.path().join("20260425-150000-prdt-viewer-2.json"),
            serde_json::to_string(&newer).unwrap(),
        )
        .unwrap();
        let pending = list_pending_in(dir.path()).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].binary, "prdt-viewer", "newest first");
        assert_eq!(pending[1].binary, "prdt-host");
    }

    #[test]
    fn mark_acknowledged_moves_file_into_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let r = sample_report("prdt-host", "2026-04-25T12:00:00Z");
        let path = write_report_to(&r, dir.path()).unwrap();
        assert!(path.exists());

        mark_acknowledged_in(dir.path(), &r.timestamp_iso, &r.binary).unwrap();

        assert!(!path.exists(), "original file moved");
        let acked: Vec<_> = std::fs::read_dir(dir.path().join("acknowledged"))
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(acked.len(), 1, "exactly one entry under acknowledged/");
    }

    #[test]
    fn mark_acknowledged_missing_match_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let r = sample_report("prdt-host", "2026-04-25T12:00:00Z");
        write_report_to(&r, dir.path()).unwrap();
        let err = mark_acknowledged_in(dir.path(), "wrong-ts", "prdt-host").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
```

- [ ] **Step 4: Wire from lib.rs**

Edit `crates/gui-common/src/lib.rs`. Find the existing `pub mod` block and re-exports. Add:

```rust
pub mod crashlog;
```

In the `pub use` re-exports, add:

```rust
pub use crashlog::{
    install_panic_hook, list_pending_crashes, mark_acknowledged, register_tail, CrashReport,
};
```

Place adjacent to the existing `pub use config::...` / `pub use i18n::...` lines so the crate-root API stays grouped.

- [ ] **Step 5: Build + test**

```bash
cd E:/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-gui-common
cargo test -p prdt-gui-common crashlog::
```

Expected: 6 crashlog tests pass.

If `serde_json` isn't yet a workspace dep, the `crates/gui-common/Cargo.toml` line will fail. In that case add `serde_json = "1"` to root `Cargo.toml` `[workspace.dependencies]` and retry.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/gui-common/Cargo.toml crates/gui-common/src/crashlog.rs crates/gui-common/src/lib.rs
git commit -m "gui-common: add crashlog module (panic_hook + JSON dump + Ack)"
```

---

## Task 2: Wire 3 GUI binaries to install_panic_hook

**Files:**
- Modify: `crates/host/src/main.rs`
- Modify: `crates/viewer/src/main.rs`
- Modify: `crates/viewer-overlay/src/main.rs`

The host's bin (`prdt-host`) doesn't have `prdt-gui-common` as a direct dep yet (it depends on `prdt-gui-host` which re-exports gui-common's surface); same for viewer/overlay. Add explicit deps so we can call `prdt_gui_common::install_panic_hook` from `main()`.

- [ ] **Step 1: Add gui-common dep to the three bin Cargo.toml files**

In `crates/host/Cargo.toml` `[target.'cfg(windows)'.dependencies]` (the existing block where `prdt-gui-host` lives), append:

```toml
prdt-gui-common = { path = "../gui-common" }
```

Same for `crates/viewer/Cargo.toml`.

For `crates/viewer-overlay/Cargo.toml`, the dep is already present (gui-viewer-overlay imports gui-common). Verify with `grep "prdt-gui-common" crates/viewer-overlay/Cargo.toml`. If absent, add to `[dependencies]`.

- [ ] **Step 2: Replace existing panic_hook in viewer/main.rs**

Read the current viewer panic hook:

```bash
grep -n "panic::set_hook\|panic_hook" crates/viewer/src/main.rs
```

Replace the existing block:

```rust
    std::panic::set_hook(Box::new(|info| {
        tracing::error!(panic = %info, "PANIC");
    }));
```

with:

```rust
    prdt_gui_common::install_panic_hook(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
```

This call must happen BEFORE any other panic is triggered (i.e. early in `main`, after `tracing_subscriber::fmt()...init()`). The new hook still emits the `tracing::error!` (it wraps the same message internally), so no log regression.

- [ ] **Step 3: Add to host/main.rs**

Read `crates/host/src/main.rs` to find the `init_tracing()` helper that the host bin uses (added in earlier phases). Right after `init_tracing()` is called inside `run_cli` (or `main` if `--headless` flow uses `init_tracing` directly), add:

```rust
    prdt_gui_common::install_panic_hook(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
```

For the GUI mode (when `--headless` is false), the call lives inside `prdt_gui_host::run_host_gui` — see Task 3. The host bin itself only needs to call it from the CLI path.

Concretely in `crates/host/src/main.rs`'s `run_cli`:

```rust
#[tokio::main(flavor = "multi_thread")]
async fn run_cli(args: Args) -> Result<()> {
    init_tracing();
    prdt_gui_common::install_panic_hook(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    run_host(args, None, tokio_util::sync::CancellationToken::new()).await
}
```

- [ ] **Step 4: Add to viewer-overlay/main.rs**

Read `crates/viewer-overlay/src/main.rs`. Find the `tracing_subscriber::fmt()...init()` line (already there). Right after it, add:

```rust
    prdt_gui_common::install_panic_hook(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
```

- [ ] **Step 5: Build + verify**

```bash
cargo build -p prdt-host -p prdt-viewer -p prdt-viewer-overlay
```

Expected: clean build. The `prdt_gui_common::install_panic_hook` call should resolve via the dep chain.

- [ ] **Step 6: Commit**

```bash
git add crates/host/Cargo.toml crates/host/src/main.rs \
        crates/viewer/Cargo.toml crates/viewer/src/main.rs \
        crates/viewer-overlay/Cargo.toml crates/viewer-overlay/src/main.rs
git commit -m "host/viewer/overlay: install crashlog panic hook from main()"
```

---

## Task 3: Pending crashes UI in HostApp + Settings

**Files:**
- Modify: `crates/gui-host/src/lib.rs` (call register_tail + list_pending_crashes; pass to HostApp)
- Modify: `crates/gui-host/src/app.rs` (new field + initializer arg)
- Modify: `crates/gui-host/src/settings.rs` (Pending crashes section + Open folder + Acknowledge all)

- [ ] **Step 1: Wire register_tail + list_pending_crashes in lib.rs**

Edit `crates/gui-host/src/lib.rs`. Right after the existing `let (tail_layer, tail_handle) = TailLayer::new(200);` block + the `tracing_subscriber::registry()...try_init()` block, add:

```rust
    // Phase 4 G5: feed the panic hook so crash dumps include recent log lines.
    prdt_gui_common::register_tail(tail_handle.clone());
    prdt_gui_common::install_panic_hook(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    // Read any unacknowledged crash reports from previous runs.
    let pending_crashes = match prdt_gui_common::list_pending_crashes() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(?e, "failed to list pending crashes");
            Vec::new()
        }
    };
```

(The `install_panic_hook` here applies to the gui-host bin's GUI path; the CLI-only `--headless` path installs it from `host/main.rs::run_cli` per Task 2.)

Pass `pending_crashes` into the `HostApp::new` call. Find the existing closure:

```rust
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(app::HostApp::new(
                cfg, path, tail, rt_handle, run_host, tray,
            )))
        }),
```

Replace with:

```rust
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(app::HostApp::new(
                cfg,
                path,
                tail,
                rt_handle,
                run_host,
                tray,
                pending_crashes,
            )))
        }),
```

- [ ] **Step 2: Extend HostApp**

In `crates/gui-host/src/app.rs`, find the `pub struct HostApp { ... }` definition and append a field before its closing brace:

```rust
    /// Phase 4 G5 — Crash reports from previous runs that the user has not
    /// yet acknowledged. Populated once at startup; mutated when the user
    /// clicks "Acknowledge all".
    pending_crashes: Vec<prdt_gui_common::CrashReport>,
```

Update `HostApp::new` to accept it. Find the existing signature:

```rust
pub fn new(
    config: Arc<Mutex<Config>>,
    config_path: PathBuf,
    tail: TailHandle,
    rt_handle: Handle,
    run_host: crate::RunHostFn,
    tray: Option<crate::tray::TrayController>,
) -> Self {
```

Replace with:

```rust
pub fn new(
    config: Arc<Mutex<Config>>,
    config_path: PathBuf,
    tail: TailHandle,
    rt_handle: Handle,
    run_host: crate::RunHostFn,
    tray: Option<crate::tray::TrayController>,
    pending_crashes: Vec<prdt_gui_common::CrashReport>,
) -> Self {
```

In the struct literal inside `new()`, add the new field initializer:

```rust
            pending_crashes,
```

- [ ] **Step 3: Add Pending crashes section to Settings**

Edit `crates/gui-host/src/settings.rs`. Update the `pub fn render` signature to accept the pending list:

Find:
```rust
pub fn render(
    ctx: &egui::Context,
    config: &Arc<Mutex<Config>>,
    config_path: &std::path::Path,
    open: &mut bool,
    error: &mut Option<String>,
    update_ui: &Arc<Mutex<UpdateUi>>,
    rt_handle: &tokio::runtime::Handle,
)
```

Replace with:
```rust
pub fn render(
    ctx: &egui::Context,
    config: &Arc<Mutex<Config>>,
    config_path: &std::path::Path,
    open: &mut bool,
    error: &mut Option<String>,
    update_ui: &Arc<Mutex<UpdateUi>>,
    rt_handle: &tokio::runtime::Handle,
    pending_crashes: &mut Vec<prdt_gui_common::CrashReport>,
)
```

Inside the `egui::Window::new(t!("settings-window-title"))....show(ctx, |ui| { ... })` body, immediately AFTER the existing G4 update banner block (search for `// Phase 4 G4: Update banner.`) and BEFORE the existing config rows — insert:

```rust
            // Phase 4 G5: Pending crash reports from previous sessions.
            if !pending_crashes.is_empty() {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 220, 100),
                    t!(
                        "crashlog-pending-heading",
                        n => pending_crashes.len() as i64,
                    ),
                );
                for r in pending_crashes.iter().take(5) {
                    let summary = if r.panic_message.len() > 80 {
                        format!("{}…", &r.panic_message[..80])
                    } else {
                        r.panic_message.clone()
                    };
                    ui.label(format!(
                        "{}  {}  \"{}\"",
                        r.timestamp_iso, r.binary, summary
                    ));
                }
                ui.horizontal(|ui| {
                    if ui.button(t!("crashlog-button-open-folder")).clicked() {
                        if let Some(dir) = prdt_gui_common::crashlog::crashes_dir() {
                            let _ = open_in_explorer(&dir);
                        }
                    }
                    if ui.button(t!("crashlog-button-acknowledge")).clicked() {
                        let snapshot = pending_crashes.clone();
                        for r in &snapshot {
                            if let Err(e) = prdt_gui_common::mark_acknowledged(
                                &r.timestamp_iso,
                                &r.binary,
                            ) {
                                tracing::warn!(?e, "mark_acknowledged failed");
                            }
                        }
                        pending_crashes.clear();
                    }
                });
                ui.separator();
            }
```

`open_in_explorer` is the helper added in G3 Task 5 (lives in `app.rs`). To call it from `settings.rs`, either:

- Make it `pub(crate)` and import: add `pub(crate)` to its definition in `app.rs`, then `use crate::app::open_in_explorer;` at the top of `settings.rs`.
- OR duplicate the small helper inside `settings.rs` (3-platform `cfg` block, 12 lines).

Pick option A (no duplication). Edit `crates/gui-host/src/app.rs`:

Find:
```rust
fn open_in_explorer(path: &std::path::Path) -> std::io::Result<()> {
```

Replace with:
```rust
pub(crate) fn open_in_explorer(path: &std::path::Path) -> std::io::Result<()> {
```

In `crates/gui-host/src/settings.rs` add to the imports at the top:

```rust
use crate::app::open_in_explorer;
```

- [ ] **Step 4: Update the settings::render call site**

In `crates/gui-host/src/app.rs`, find the existing call:

```rust
            crate::settings::render(
                ctx,
                &self.config,
                &self.config_path,
                &mut self.settings_open,
                &mut self.error,
                &self.update_ui,
                &self.rt_handle,
            );
```

Replace with:

```rust
            crate::settings::render(
                ctx,
                &self.config,
                &self.config_path,
                &mut self.settings_open,
                &mut self.error,
                &self.update_ui,
                &self.rt_handle,
                &mut self.pending_crashes,
            );
```

- [ ] **Step 5: Build + clippy**

```bash
cd E:/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-gui-host
cargo clippy -p prdt-gui-host --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: clean build, clippy clean. Common errors:
- `t!("crashlog-...")` → "missing-string: ..." at runtime: that's expected until Task 5 adds the IDs. Build passes.
- `pending_crashes` borrow conflicts: read the local `snapshot` THEN call `mark_acknowledged`. The Vec is held by `&mut`, so cloning before the loop avoids `&mut` overlap.

- [ ] **Step 6: Commit**

```bash
git add crates/gui-host/src/lib.rs crates/gui-host/src/app.rs crates/gui-host/src/settings.rs
git commit -m "gui-host: pending crashes section + Acknowledge button + register_tail"
```

---

## Task 4: signing scaffolding (PowerShell + docs)

**Files:**
- Create: `scripts/sign-msi.ps1`
- Create: `docs/sign-and-release.md`
- Modify: `docs/build-msi.md` (add Sign step pointing at the new script + doc)

- [ ] **Step 1: Create scripts/sign-msi.ps1**

```bash
mkdir -p scripts
```

Create `scripts/sign-msi.ps1`:

```powershell
# Phase 4 G5 — Sign a Power Remote Desktop MSI with Authenticode.
# Requires Windows SDK signtool.exe in PATH.
param(
    [Parameter(Mandatory=$true)] [string]$CertPath,
    [Parameter(Mandatory=$true)] [string]$CertPassword,
    [Parameter(Mandatory=$true)] [string]$MsiPath,
    [string]$TimestampUrl = "http://timestamp.digicert.com",
    [string]$Description = "Power Remote Desktop"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $CertPath)) {
    throw "Certificate file not found: $CertPath"
}
if (-not (Test-Path $MsiPath)) {
    throw "MSI not found: $MsiPath"
}

$signtool = (Get-Command signtool.exe -ErrorAction SilentlyContinue).Source
if (-not $signtool) {
    throw "signtool.exe not in PATH. Install Windows SDK or add the SDK bin dir to PATH."
}

Write-Host "Signing $MsiPath..."
& $signtool sign `
    /f $CertPath `
    /p $CertPassword `
    /t $TimestampUrl `
    /td sha256 `
    /fd sha256 `
    /d $Description `
    /v `
    $MsiPath
if ($LASTEXITCODE -ne 0) {
    throw "signtool sign failed (exit $LASTEXITCODE)"
}

Write-Host "Verifying signature..."
& $signtool verify /pa /v $MsiPath
if ($LASTEXITCODE -ne 0) {
    throw "signtool verify failed (exit $LASTEXITCODE)"
}

Write-Host "Successfully signed and verified $MsiPath"
```

- [ ] **Step 2: Smoke the script (cert-less dry-run)**

```bash
pwsh -File scripts/sign-msi.ps1 -CertPath nonexistent.pfx -CertPassword "" -MsiPath nonexistent.msi 2>&1 | tail -5
```

Expected: "Certificate file not found: nonexistent.pfx". If `pwsh` (PowerShell 7) is not on PATH, try `powershell -File ...` (Windows PowerShell 5.x); the script is compatible with both. The error confirms the script parses + the early validation works.

If neither `pwsh` nor `powershell` is in PATH (Linux dev environment without WSL), skip this step and document.

- [ ] **Step 3: Create docs/sign-and-release.md**

Create `docs/sign-and-release.md`:

```markdown
# Signing and releasing a Power Remote Desktop MSI

Phase 4 G5 ships the signing scaffolding (script + docs) but does NOT include a code-signing certificate. This guide is the runbook for the day a cert is procured.

## Choosing a certificate

Public OSS distribution on Windows hits **SmartScreen** — Windows checks the publisher's signature on every installer run. Three options:

| Option | Cost | SmartScreen | Notes |
|---|---|---|---|
| **EV (Extended Validation)** | $300+/year | Trusted immediately | Requires hardware token (USB key); validation 1-2 weeks |
| **OV (Organization Validation)** | $100+/year | Warns until reputation builds (~weeks of installs) | File-based cert, easy to use in CI |
| **Self-signed** | $0 | Always warns | Test only, not for public release |

Common vendors: Sectigo, DigiCert, SSL.com. EV cert procurement involves identity / business verification — start the process several weeks before you intend to ship.

## Storing the cert

- **Local dev**: keep the `.pfx` outside the repo (e.g. `~/secrets/prdt-codesign.pfx`). Never commit.
- **CI**: store as an encrypted secret (GitHub Actions: `secrets.CODESIGN_PFX_BASE64`, decode at job start to a temp file). The MSI workflow runs `scripts/sign-msi.ps1` after `cargo wix`.

## Using `scripts/sign-msi.ps1`

```powershell
scripts/sign-msi.ps1 `
    -CertPath "C:\path\to\prdt-codesign.pfx" `
    -CertPassword "<password>" `
    -MsiPath "target/wix/prdt-setup-v0.0.1.msi"
```

What it does:

1. Validates the cert file and MSI exist.
2. Runs `signtool sign /f <cert> /p <pass> /t <timestamp_url> /td sha256 /fd sha256 /d "Power Remote Desktop" /v <msi>`.
3. Runs `signtool verify /pa /v <msi>` to confirm the signature is valid and the timestamp is trusted.

Pass a different `-TimestampUrl` if `timestamp.digicert.com` is unreachable. Backup options:

- `http://timestamp.sectigo.com`
- `http://tsa.starfieldtech.com`
- `http://timestamp.globalsign.com`

## Release checklist

After a green build:

1. `version` bumped in workspace `Cargo.toml`.
2. `cargo run -p prdt-gui-host --bin mkicon`.
3. `cargo build --release -p prdt-host -p prdt-viewer -p prdt-viewer-overlay`.
4. `cargo wix --no-build`.
5. **Sign**: `scripts/sign-msi.ps1 -CertPath ... -MsiPath target/wix/prdt-setup-vX.Y.Z.msi`.
6. `git tag -a vX.Y.Z` matching workspace version.
7. `git push && git push --tags`.
8. `gh release create vX.Y.Z target/wix/prdt-setup-vX.Y.Z.msi --notes-file CHANGELOG-vX.Y.Z.md`.
9. Verify the auto-update path (G4) by running an installed older `prdt-host.exe` and checking that Settings → Check for updates surfaces the new version.

## Troubleshooting

- **`signtool: unknown error 0x80092009`** — Cert format mismatch. Ensure the `.pfx` was exported with the private key included.
- **`The specified timestamp server either could not be reached`** — Try a different `-TimestampUrl`.
- **SmartScreen still warns after signing** — That's expected with OV certs until enough installs accumulate "reputation". Microsoft's algorithm; nothing to do but wait or upgrade to EV.
- **`signtool verify` fails after sign succeeds** — A timestamp service mismatch or trust root issue. Run `certutil -store My` to inspect the local cert store.
```

- [ ] **Step 4: Update docs/build-msi.md with Sign step**

Edit `docs/build-msi.md`. Find the existing "Building a release MSI" section. AFTER the `cargo wix --no-build` step (and before "Smoke test on a clean VM"), insert:

```markdown
## Sign the MSI (optional, recommended for public release)

Once a code-signing certificate is available, sign the generated MSI with:

```powershell
scripts/sign-msi.ps1 `
    -CertPath "C:\path\to\prdt-codesign.pfx" `
    -CertPassword "<password>" `
    -MsiPath "target/wix/prdt-setup-vX.Y.Z.msi"
```

See [`docs/sign-and-release.md`](sign-and-release.md) for cert procurement, timestamp servers, and the full release checklist.

Unsigned MSIs install fine but trigger Windows SmartScreen warnings on first run. Signing is recommended before public release.
```

- [ ] **Step 5: Commit**

```bash
git add scripts/sign-msi.ps1 docs/sign-and-release.md docs/build-msi.md
git commit -m "g5: sign-msi.ps1 + sign-and-release.md + build-msi.md Sign step"
```

---

## Task 5: i18n IDs + final validation + tag

**Files:**
- Modify: `crates/gui-common/locales/en/main.ftl`
- Modify: `crates/gui-common/locales/ja/main.ftl`

- [ ] **Step 1: Append en IDs**

Append to `crates/gui-common/locales/en/main.ftl`:

```ftl

# Crash reporter (Phase 4 G5)
crashlog-pending-heading = Last session crashed ({ $n } reports):
crashlog-button-open-folder = Open crashes folder
crashlog-button-acknowledge = Acknowledge all
crashlog-no-pending = No pending crash reports.
```

- [ ] **Step 2: Append ja IDs**

Append to `crates/gui-common/locales/ja/main.ftl`:

```ftl

# クラッシュレポータ (Phase 4 G5)
crashlog-pending-heading = 前回のセッションでクラッシュしました ({ $n } 件)
crashlog-button-open-folder = クラッシュフォルダを開く
crashlog-button-acknowledge = すべて確認済みにする
crashlog-no-pending = 未送信のクラッシュレポートはありません。
```

- [ ] **Step 3: Run id-completeness + placeholder-match tests**

```bash
cd E:/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo test -p prdt-gui-common locale_files_have_same_ids placeholders_match_across_locales
```

Expected: both pass.

- [ ] **Step 4: Workspace tests + clippy + fmt**

```bash
cargo test --workspace 2>&1 | awk '/^test result:/ {p+=$4; f+=$6} END {print "total:", p, "failed:", f}'
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected: total ≥ 272 (was 266 + 6 new crashlog tests), failed: 0; clippy clean.

```bash
rustfmt \
  crates/gui-common/src/crashlog.rs \
  crates/gui-common/src/lib.rs \
  crates/host/src/main.rs \
  crates/viewer/src/main.rs \
  crates/viewer-overlay/src/main.rs \
  crates/gui-host/src/lib.rs \
  crates/gui-host/src/app.rs \
  crates/gui-host/src/settings.rs
git diff --stat
```

If non-empty:

```bash
git add -u
git commit -m "phase4-g5: cargo fmt on touched files"
```

- [ ] **Step 5: Tag**

Commit i18n first:

```bash
git add crates/gui-common/locales/en/main.ftl crates/gui-common/locales/ja/main.ftl
git commit -m "gui-common: add crashlog-* i18n IDs (en + ja)"
```

Then tag:

```bash
git tag -a phase4-g5-complete -m "$(cat <<'EOF'
Phase 4 G5 complete — crash reporter + Authenticode signing scaffolding

Phase 4 is now fully complete (G1 + G2 + G3 + G4 + G5 + G6 all merged).

- gui-common::crashlog: install_panic_hook (panic hook → JSON dump under
  dirs::cache_dir()/prdt/crashes/<timestamp>-<binary>-<pid>.json),
  list_pending_crashes (newest first), mark_acknowledged (move to
  acknowledged/ subdir), register_tail (TailHandle → recent log lines)
- 3 GUI binaries (host / viewer / overlay) install the panic hook from
  main(); existing inline panic hooks replaced
- gui-host startup loads pending crashes; Settings shows a banner with
  timestamp + binary + summary, "Open crashes folder" + "Acknowledge all"
- 4 new i18n IDs (crashlog-*)
- 6 new crashlog tests (round-trip, list, sort, mark, missing-match,
  not-found-error)
- scripts/sign-msi.ps1: signtool wrapper (sign + verify, SHA256 +
  timestamp)
- docs/sign-and-release.md: EV/OV cert procurement guide + release
  checklist; docs/build-msi.md gains a "Sign the MSI" section
- Cert purchase deferred to Phase 5 public release decision (no
  $$ spent in G5)

Out of scope: native exception minidumps, auto-upload (GitHub/Sentry),
PII masking, viewer/overlay-side pending crashes UI, cert procurement.
EOF
)"
git tag | grep phase4
```

Expected: `phase4-g5-complete` listed, alongside g1, g2, g3, g4, g6, title-status.

- [ ] **Step 6: Final summary**

Report back to the user:

- Files changed across all 5 tasks
- Workspace test count (272 expected) + delta from 266
- Clippy result
- Fmt commit needed (yes/no)
- Tag listing
- `git log --oneline master..HEAD` showing all G5 commits
- Manual smoke instructions: trigger a panic in any GUI binary, verify the JSON file in `dirs::cache_dir()/prdt/crashes/`, restart `prdt-host`, see Settings banner, click Acknowledge all
- Note: Phase 4 is now complete; G5 deliverables enable signed releases as soon as a cert is procured

---

## Risks & Notes for Implementer

- **`std::panic::PanicHookInfo`** is the renamed type in Rust 1.81+; older toolchains used `std::panic::PanicInfo`. Workspace MSRV is 1.78 → check whether `PanicHookInfo` resolves. If it doesn't, swap to `&std::panic::PanicInfo`. The closure body is unchanged.
- **`OnceLock<TailHandle>::set` returns `Err` on second call**. We swallow the `Err` (`let _ = ...`) so multiple calls in tests are no-ops — the first registered tail wins for the process lifetime.
- **`install_panic_hook` REPLACES any previous hook**. The viewer's pre-G5 hook is replaced cleanly because we call `set_hook` after `tracing_subscriber::fmt()...init()`. The host bin had no inline hook; the new one is the only one. The overlay binary previously had no hook either.
- **Tail registration ordering**: in `gui-host::run_host_gui`, register the tail BEFORE installing the panic hook so the hook closure sees a populated `OnceLock` if it fires immediately. Done in spec order.
- **`CARGO_PKG_NAME`** for the host bin resolves to `prdt-host` (the binary's package name, not the parent workspace). Same for viewer / overlay. This matches the JSON `binary` field naming used by `mark_acknowledged`.
- **`std::env::var("PRDT_CRASHLOG_DIR")`** is consulted on every call to `crashes_dir()`. Tests set it via `tempdir`. Do NOT set it in production runtime — leave to `dirs::cache_dir()`.
- **Settings UI borrow juggling**: `pending_crashes` is `&mut Vec<...>`. The Acknowledge handler clones it into a local `snapshot`, iterates that, and clears the original. This avoids holding `&mut` across `mark_acknowledged` calls.
- **PowerShell on the dev machine**: Step 2 of Task 4 tries `pwsh` first then falls back to `powershell.exe`. If neither runs (Linux dev shell), document the fact and skip the smoke; it's not a blocker.
- **`signtool.exe` is part of the Windows SDK**, NOT a Rust dep. The PowerShell script's `Get-Command signtool.exe` resolves it at run time.
- **`prdt-gui-common` dep on host/viewer**: existing crates already pull `prdt-gui-host` / `prdt-gui-viewer` which in turn depend on `prdt-gui-common`. We add an explicit dep so the binary's `main()` can reference the `prdt_gui_common::install_panic_hook` symbol directly without re-exporting from the GUI crates.

---

## Self-Review

- **Spec coverage**: crashlog module + tests (Task 1) ✓, 3-binary panic hook wiring (Task 2) ✓, register_tail + list_pending_crashes at gui-host startup (Task 3) ✓, Pending crashes Settings UI (Task 3) ✓, signtool wrapper script (Task 4) ✓, sign-and-release.md (Task 4) ✓, build-msi.md Sign step (Task 4) ✓, 4 i18n IDs (Task 5) ✓, tag phase4-g5-complete (Task 5) ✓.
- **Placeholder scan**: No "TBD", "implement later", or vague stubs. The "cert procurement deferred to Phase 5 decision" notes are documenting an intentional out-of-scope item, not skipping a step.
- **Type consistency**: `CrashReport { binary, version, timestamp_iso, panic_message, panic_location, recent_log_lines }` consistent across spec and plan. `install_panic_hook(binary_name, version)`, `register_tail(TailHandle)`, `list_pending_crashes() -> io::Result<Vec<CrashReport>>`, `mark_acknowledged(timestamp_iso, binary) -> io::Result<()>` consistent. `HostApp::pending_crashes: Vec<CrashReport>` matches the parameter type passed to settings::render.
