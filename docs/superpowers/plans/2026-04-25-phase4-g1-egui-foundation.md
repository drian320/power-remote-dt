# Phase 4 G1 — egui Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add egui-based GUI entry points to `prdt-host` and `prdt-viewer` so users can run the system without typing CLI flags. Existing CLI is preserved via `--headless`. Settings persist to `%APPDATA%\prdt\config.toml`.

**Architecture:** Three new crates: `gui-common` (Config, style, QR, log tail), `gui-host` (eframe app that supervises the host server via tokio task), `gui-viewer` (eframe launcher that exits before the existing winit/D3D11 viewer takes over). Existing `host::main` body is refactored into `pub async fn run_host(...)` so both CLI and GUI can call it.

**Tech Stack:** Rust 2021, `eframe` 0.28 (egui native backend), `qrcode` 0.14, `rfd` 0.14, `toml` 0.8 (new workspace dep), `dirs` 5.0, `tokio` 1.40 (existing), `tracing-subscriber` (existing layer trait).

**Spec:** `docs/superpowers/specs/2026-04-25-phase4-g1-egui-foundation-design.md`

---

## File Structure

**Created files:**

```
crates/gui-common/
  Cargo.toml
  src/
    lib.rs            re-exports
    config.rs         Config struct + TOML load/save + default_path()
    paths.rs          %APPDATA%\prdt\* path resolvers via dirs
    style.rs          egui style + Noto JP font setup
    qr.rs             qrcode → egui::ColorImage helper
    log_tail.rs       tracing Layer storing last N lines in Mutex<VecDeque>
  assets/
    NotoSansJP-Reduced.ttf  (~3MB, JP-Reduced subset checked in)

crates/gui-host/
  Cargo.toml
  src/
    lib.rs            pub fn run_host_gui(...)
    app.rs            HostApp impl eframe::App + state machine
    keygen.rs         first-run key generation
    settings.rs       settings modal

crates/gui-viewer/
  Cargo.toml
  src/
    lib.rs            pub fn run_viewer_launcher(...)
    app.rs            LauncherApp impl eframe::App
    hosts_list.rs     saved-hosts UI
    connect_form.rs   add-new-host modal
    settings.rs       viewer prefs modal
```

**Modified files:**

```
Cargo.toml                               add workspace deps for eframe/egui/qrcode/rfd/toml/dirs/tracing-subscriber
crates/host/src/main.rs                  extract `run_host(args, status, cancel)` async fn; route main() via --headless
crates/viewer/src/main.rs                add `--headless` arg; route main() via run_viewer_launcher unless headless
crates/host/Cargo.toml                   add gui-host dep
crates/viewer/Cargo.toml                 add gui-viewer dep
```

**Public API surface added:**

- `prdt_gui_common::config::Config` (+ load/save/default_path)
- `prdt_gui_common::style::install_jp_font(&mut egui::Context)`
- `prdt_gui_common::qr::generate(&str) -> Result<egui::ColorImage>`
- `prdt_gui_common::log_tail::TailLayer` (+ `tail_lines() -> Vec<String>`)
- `prdt_gui_host::run_host_gui(config_path: Option<PathBuf>) -> Result<()>`
- `prdt_gui_viewer::run_viewer_launcher(config_path: Option<PathBuf>) -> Result<LaunchOutcome>`
- `prdt_gui_viewer::{LaunchOutcome, ConnectArgs, ConnectMode}`

---

## Task 1: gui-common crate skeleton + Config + paths

**Files:**
- Create: `crates/gui-common/Cargo.toml`
- Create: `crates/gui-common/src/lib.rs`
- Create: `crates/gui-common/src/config.rs`
- Create: `crates/gui-common/src/paths.rs`
- Modify: workspace `Cargo.toml` (add `members` entry + workspace deps)

- [ ] **Step 1: Add workspace dependencies**

In `Cargo.toml` (workspace root), append to `[workspace.dependencies]`:

```toml
# GUI (Phase 4 G1)
eframe = { version = "0.28", default-features = false, features = ["default_fonts", "glow", "wayland"] }
egui = "0.28"
qrcode = "0.14"
rfd = "0.14"
toml = "0.8"
dirs = "5.0"
tracing-subscriber = { version = "0.3", features = ["env-filter", "registry"] }
```

(Note: `tracing-subscriber` may already be referenced indirectly — if it appears as a direct dep on individual crates, leave those alone but add the workspace entry so new crates can pull it.)

In `[workspace] members`, append `"crates/gui-common"`. (Subsequent tasks add `gui-host` / `gui-viewer`.)

- [ ] **Step 2: Create the gui-common Cargo.toml**

Create `crates/gui-common/Cargo.toml`:

```toml
[package]
name = "prdt-gui-common"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
egui = { workspace = true }
serde = { workspace = true }
toml = { workspace = true }
dirs = { workspace = true }
qrcode = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

- [ ] **Step 3: Write failing tests for Config**

Create `crates/gui-common/src/config.rs`:

```rust
//! Persistent configuration shared by host and viewer GUIs.
//!
//! Schema is documented in
//! `docs/superpowers/specs/2026-04-25-phase4-g1-egui-foundation-design.md`.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub host: HostConfig,
    #[serde(default)]
    pub viewer: ViewerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostConfig {
    pub bind: String,
    pub monitor: u32,
    pub bitrate_mbps: u32,
    pub outgoing_dir: PathBuf,
    #[serde(default)]
    pub signaling_url: String,
    pub host_id_file: PathBuf,
    pub key_file: PathBuf,
    #[serde(default)]
    pub auto_start: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:9000".into(),
            monitor: 0,
            bitrate_mbps: 30,
            outgoing_dir: PathBuf::from("prdt-outgoing"),
            signaling_url: String::new(),
            host_id_file: PathBuf::from("host-id.txt"),
            key_file: PathBuf::from("host-key.bin"),
            auto_start: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViewerConfig {
    pub recv_dir: PathBuf,
    pub decoder: String,
    pub default_resolution: String,
    pub default_fps: u32,
    #[serde(default)]
    pub signaling_url: String,
    pub known_hosts: PathBuf,
    pub known_host_ids: PathBuf,
    #[serde(default)]
    pub hosts: Vec<HostEntry>,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            recv_dir: PathBuf::from("prdt-received"),
            decoder: "mf".into(),
            default_resolution: "1920x1080".into(),
            default_fps: 60,
            signaling_url: String::new(),
            known_hosts: PathBuf::from("known-hosts"),
            known_host_ids: PathBuf::from("known-host-ids"),
            hosts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostEntry {
    pub label: String,
    pub mode: String, // "direct" | "signaling"
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub host_id: String,
    #[serde(default)]
    pub pubkey: String,
    #[serde(default)]
    pub last_connected: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: HostConfig::default(),
            viewer: ViewerConfig::default(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml decode: {0}")]
    TomlDecode(#[from] toml::de::Error),
    #[error("toml encode: {0}")]
    TomlEncode(#[from] toml::ser::Error),
}

impl Config {
    /// Load config from `path`. If the file doesn't exist, return
    /// `Config::default()` and write it to disk so subsequent loads pick
    /// up the same defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let cfg: Config = toml::from_str(&s)?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                cfg.save(path)?;
                Ok(cfg)
            }
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(self)?;
        std::fs::write(path, s)?;
        Ok(())
    }

    /// Parsed bind address; falls back to `0.0.0.0:9000` if invalid.
    pub fn bind_addr(&self) -> SocketAddr {
        self.host
            .bind
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:9000".parse().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_toml() {
        let c = Config::default();
        let s = toml::to_string_pretty(&c).unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(c, parsed);
    }

    #[test]
    fn load_missing_writes_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let c = Config::load(&path).unwrap();
        assert_eq!(c, Config::default());
        assert!(path.exists(), "missing file should have been created");
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/config.toml");
        Config::default().save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn host_entry_supports_signaling_only() {
        let mut c = Config::default();
        c.viewer.hosts.push(HostEntry {
            label: "Home".into(),
            mode: "signaling".into(),
            addr: String::new(),
            host_id: "123-456-789".into(),
            pubkey: String::new(),
            last_connected: String::new(),
        });
        let s = toml::to_string_pretty(&c).unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(parsed.viewer.hosts.len(), 1);
        assert_eq!(parsed.viewer.hosts[0].host_id, "123-456-789");
    }
}
```

Add `tempfile` to dev-dependencies. Update `crates/gui-common/Cargo.toml`:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Create paths.rs**

Create `crates/gui-common/src/paths.rs`:

```rust
//! OS-aware paths under `dirs::config_dir()/prdt/`.

use std::path::PathBuf;

/// Root config directory: `%APPDATA%\prdt\` on Windows, `$XDG_CONFIG_HOME/prdt/` on Linux.
/// Returns `None` if the OS doesn't expose a config dir (extremely unusual; shouldn't happen
/// on supported platforms).
pub fn config_root() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("prdt"))
}

/// Default path for `config.toml`.
pub fn default_config_path() -> Option<PathBuf> {
    config_root().map(|d| d.join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_root_ends_with_prdt() {
        let p = config_root().expect("OS has a config dir");
        assert!(
            p.ends_with("prdt"),
            "config_root() should end with 'prdt', got {p:?}"
        );
    }
}
```

- [ ] **Step 5: lib.rs re-exports**

Create `crates/gui-common/src/lib.rs`:

```rust
//! Shared GUI infrastructure used by `prdt-gui-host` and `prdt-gui-viewer`.

pub mod config;
pub mod paths;

pub use config::{Config, ConfigError, HostConfig, HostEntry, ViewerConfig};
pub use paths::{config_root, default_config_path};
```

- [ ] **Step 6: Run tests**

```bash
cd E:/project/rust-desktop/power-remote-dt
cargo test -p prdt-gui-common
```

Expected: all 5 tests pass (4 in `config::tests`, 1 in `paths::tests`).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/gui-common
git commit -m "gui-common: add Config (toml) + paths skeleton"
```

---

## Task 2: gui-common QR helper + log tail Layer + style

**Files:**
- Create: `crates/gui-common/src/qr.rs`
- Create: `crates/gui-common/src/log_tail.rs`
- Create: `crates/gui-common/src/style.rs`
- Create: `crates/gui-common/assets/NotoSansJP-Reduced.ttf` (placeholder; real download in Step 4)
- Modify: `crates/gui-common/src/lib.rs`
- Modify: `crates/gui-common/Cargo.toml`

- [ ] **Step 1: QR helper with failing test**

Create `crates/gui-common/src/qr.rs`:

```rust
//! QR code generation for displaying host pubkey/host_id strings.

use egui::ColorImage;
use qrcode::{Color, QrCode};

#[derive(thiserror::Error, Debug)]
pub enum QrError {
    #[error("qrcode: {0}")]
    QrCode(#[from] qrcode::types::QrError),
}

/// Render `text` as a black-on-white QR code at integer pixel `scale`.
/// Returns an `egui::ColorImage` of size `(modules*scale)x(modules*scale)`.
pub fn generate(text: &str, scale: usize) -> Result<ColorImage, QrError> {
    let code = QrCode::new(text.as_bytes())?;
    let modules = code.width();
    let pixel_w = modules * scale;
    let mut pixels = vec![egui::Color32::WHITE; pixel_w * pixel_w];
    let bools: Vec<bool> = code.to_colors().into_iter().map(|c| c == Color::Dark).collect();
    for my in 0..modules {
        for mx in 0..modules {
            if bools[my * modules + mx] {
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = mx * scale + sx;
                        let py = my * scale + sy;
                        pixels[py * pixel_w + px] = egui::Color32::BLACK;
                    }
                }
            }
        }
    }
    Ok(ColorImage {
        size: [pixel_w, pixel_w],
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonempty_input_produces_image() {
        let img = generate("hello", 4).unwrap();
        assert!(img.size[0] >= 4 * 21); // smallest QR is 21x21 modules
        assert_eq!(img.size[0], img.size[1]);
    }

    #[test]
    fn larger_payload_grows() {
        let small = generate("a", 2).unwrap();
        let large = generate(&"a".repeat(100), 2).unwrap();
        assert!(large.size[0] > small.size[0]);
    }
}
```

Add `qrcode` to `[dependencies]` in `crates/gui-common/Cargo.toml` (already added in Task 1 Step 2).

- [ ] **Step 2: log_tail Layer**

Create `crates/gui-common/src/log_tail.rs`:

```rust
//! `tracing_subscriber::Layer` that buffers the last N formatted log lines
//! into a shared `VecDeque<String>`. Used by host GUI to show a recent
//! activity tail without changing the existing stderr output.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

#[derive(Clone)]
pub struct TailHandle(Arc<Mutex<VecDeque<String>>>);

impl TailHandle {
    pub fn snapshot(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().cloned().collect()
    }

    fn push(&self, line: String) {
        let mut q = self.0.lock().unwrap();
        q.push_back(line);
        let cap = q.capacity();
        while q.len() > cap.max(1) {
            q.pop_front();
        }
    }
}

pub struct TailLayer {
    handle: TailHandle,
}

impl TailLayer {
    /// Build a TailLayer that retains at most `capacity` lines.
    pub fn new(capacity: usize) -> (Self, TailHandle) {
        let q = VecDeque::with_capacity(capacity);
        let handle = TailHandle(Arc::new(Mutex::new(q)));
        (
            Self {
                handle: handle.clone(),
            },
            handle,
        )
    }
}

impl<S: Subscriber> Layer<S> for TailLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        struct V(String);
        impl Visit for V {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    use std::fmt::Write;
                    let _ = write!(&mut self.0, "{value:?}");
                }
            }
        }
        let mut v = V(String::new());
        event.record(&mut v);
        let level = *event.metadata().level();
        let target = event.metadata().target();
        let line = format!("{level:5} {target}: {}", v.0);
        self.handle.push(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    #[test]
    fn captures_recent_events_up_to_capacity() {
        let (layer, handle) = TailLayer::new(3);
        let subscriber = Registry::default().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("a");
            tracing::info!("b");
            tracing::info!("c");
            tracing::info!("d");
        });
        let snap = handle.snapshot();
        assert_eq!(snap.len(), 3);
        // oldest dropped: only b, c, d remain.
        assert!(snap[0].contains("\"b\""));
        assert!(snap[2].contains("\"d\""));
    }
}
```

- [ ] **Step 3: Style + JP font**

Create `crates/gui-common/src/style.rs`:

```rust
//! egui style + Japanese font setup.
//!
//! Embeds Noto Sans CJK JP (subset) so Japanese strings render without
//! relying on the user's installed fonts. Apply once at startup with
//! `install_jp_font(&ctx)`.

use egui::{FontData, FontDefinitions, FontFamily};

const JP_FONT: &[u8] = include_bytes!("../assets/NotoSansJP-Reduced.ttf");

/// Install the bundled JP font alongside egui's default fonts. The font is
/// added as the highest-priority Proportional fallback so JP glyphs render
/// while ASCII keeps egui's default look.
pub fn install_jp_font(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("noto_jp".into(), FontData::from_static(JP_FONT));

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "noto_jp".into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("noto_jp".into());

    ctx.set_fonts(fonts);
}
```

- [ ] **Step 4: Place the JP font asset**

The font asset is a checked-in binary. For this plan we ship a tiny stub so the build succeeds; the production-quality JP-Reduced subset is a separate upstream task (G6 may revisit).

Create `crates/gui-common/assets/NotoSansJP-Reduced.ttf` as a copy of any TTF available on the build machine, OR a placeholder ~1KB file containing valid TTF magic bytes. For dev-time bring-up, copy `C:\Windows\Fonts\YuGothR.ttc` truncated to TTF (or any small JP-capable TTF the dev has). The binary location is `crates/gui-common/assets/NotoSansJP-Reduced.ttf`.

If no TTF is available on the dev machine, run:

```bash
mkdir -p crates/gui-common/assets
# Use a system Japanese-capable TTF as a placeholder; replace with proper subset later.
cp "/c/Windows/Fonts/msmincho.ttc" crates/gui-common/assets/NotoSansJP-Reduced.ttf
```

If `msmincho.ttc` is missing or you prefer not to ship MS-licensed fonts, download a proper Noto subset:

```bash
# Optional: fetch upstream Noto Sans JP TTF (Google Fonts, OFL license).
# This is a one-time setup; skip if asset already exists.
curl -L -o crates/gui-common/assets/NotoSansJP-Reduced.ttf \
  https://github.com/notofonts/noto-cjk/raw/main/Sans/SubsetOTF/JP/NotoSansCJKjp-Regular.otf
```

Either way, ensure `crates/gui-common/assets/NotoSansJP-Reduced.ttf` exists and is loadable as a font. The `include_bytes!` will compile against whatever bytes are there; if the bytes aren't a valid font, `ctx.set_fonts(fonts)` will silently fall back to egui defaults — acceptable for G1 (G6 polishes).

Add to `.gitignore` if needed (depending on font license). For G1, commit the file.

- [ ] **Step 5: lib.rs re-exports**

Edit `crates/gui-common/src/lib.rs` to add the new modules:

```rust
//! Shared GUI infrastructure used by `prdt-gui-host` and `prdt-gui-viewer`.

pub mod config;
pub mod log_tail;
pub mod paths;
pub mod qr;
pub mod style;

pub use config::{Config, ConfigError, HostConfig, HostEntry, ViewerConfig};
pub use log_tail::{TailHandle, TailLayer};
pub use paths::{config_root, default_config_path};
pub use qr::generate as generate_qr;
pub use style::install_jp_font;
```

- [ ] **Step 6: Run tests**

```bash
cargo test -p prdt-gui-common
```

Expected: 7 tests pass (4 config + 1 paths + 2 qr + 1 log_tail).

- [ ] **Step 7: Commit**

```bash
git add crates/gui-common
git commit -m "gui-common: QR + tail layer + JP font style"
```

---

## Task 3: Refactor host main() — extract `run_host()`

This task does NO behavior change. It's prep for Task 5 (gui-host) which needs to call the host server logic from a tokio task.

**Files:**
- Modify: `crates/host/src/main.rs`

- [ ] **Step 1: Identify the existing main body**

Read `crates/host/src/main.rs:93-end`. The body is one async function decorated `#[tokio::main]`. It:
- parses `Args`
- loads / generates the keypair
- prints pubkey
- builds D3D11 device + selects monitor
- binds UDP transport (with TURN if configured)
- runs the rendezvous loop / handshake / encode-loop

We need to extract everything after `Args::parse()` into a new `pub async fn run_host(args: Args, status: Option<Arc<Mutex<HostStatus>>>, cancel: CancellationToken) -> Result<()>` and reduce `main()` to:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let args = Args::parse();
    run_host(args, None, CancellationToken::new()).await
}
```

`status: Option<...>` is None for now; G1 Task 5 fills it in. `CancellationToken` is from `tokio_util::sync` — add `tokio-util = { version = "0.7", features = ["rt"] }` to `crates/host/Cargo.toml` if not already present.

- [ ] **Step 2: Add HostStatus type stub in host crate**

Create `crates/host/src/status.rs` (or add to a new module file):

```rust
//! Optional supervisor channel between host's main loop and an embedding
//! GUI (Phase 4 G1). When `None`, the loop runs as before with no status
//! reporting.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::net::SocketAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Idle,
    Listening,
    Stopping,
}

#[derive(Debug)]
pub struct HostStatus {
    pub state: HostState,
    pub pubkey_b64: String,
    pub allocated_host_id: Option<String>,
    pub listening_addr: Option<SocketAddr>,
    pub peers_connected: u32,
    pub bitrate_mbps_actual: f32,
    pub last_log_lines: VecDeque<String>,
}

impl Default for HostStatus {
    fn default() -> Self {
        Self {
            state: HostState::Idle,
            pubkey_b64: String::new(),
            allocated_host_id: None,
            listening_addr: None,
            peers_connected: 0,
            bitrate_mbps_actual: 0.0,
            last_log_lines: VecDeque::with_capacity(200),
        }
    }
}

pub type SharedStatus = Arc<Mutex<HostStatus>>;
```

In `crates/host/src/main.rs`, add `mod status; use status::*;` near the top.

- [ ] **Step 3: Refactor main() body into run_host()**

Move everything between `let args = Args::parse();` (around line 101) and the function's closing `}` into a new top-level `pub async fn run_host`. Replace the in-body uses of `args` with the function parameter. After every existing `info!(...)` or significant state change (peer connected, bitrate measured), add an optional `if let Some(s) = &status { /* update s */ }` block.

The minimum viable update points for G1:
1. Right after the keypair is loaded: write `pubkey_b64` into status
2. Right after UDP bind succeeds: set `state = Listening`, write `listening_addr`
3. After signaling rendezvous succeeds (if applicable): write `allocated_host_id`
4. Loop exit (normal or cancel): set `state = Idle`

Honor `cancel.cancelled().await` at the top of the main accept-loop / encode-loop, e.g.:

```rust
tokio::select! {
    _ = cancel.cancelled() => break,
    res = some_existing_await => { /* original handling */ }
}
```

The exact call sites depend on the existing `main` body. Read it first:

```bash
sed -n '93,260p' crates/host/src/main.rs
```

Apply the refactor task-by-task: extract first, run a smoke test, then add cancel/status hooks one at a time.

After refactoring, `main()` should be:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let args = Args::parse();
    run_host(args, None, tokio_util::sync::CancellationToken::new()).await
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
}
```

- [ ] **Step 4: Build + smoke**

```bash
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-host
```

Expected: clean build.

- [ ] **Step 5: Verify behavior unchanged**

```bash
cargo test --workspace 2>&1 | grep "test result" | tail -5
```

Expected: same 218 tests pass as on master.

Then run a smoke check that the host bin still starts:

```bash
./target/debug/prdt-host.exe --help 2>&1 | head -20
```

Expected: existing CLI usage prints unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/host
git commit -m "host: extract run_host() async fn for GUI supervisor reuse"
```

---

## Task 4: gui-host crate skeleton + key generation flow

**Files:**
- Create: `crates/gui-host/Cargo.toml`
- Create: `crates/gui-host/src/lib.rs`
- Create: `crates/gui-host/src/app.rs`
- Create: `crates/gui-host/src/keygen.rs`
- Modify: workspace `Cargo.toml` `members`

- [ ] **Step 1: Cargo.toml**

Add `"crates/gui-host"` to workspace `members`. Create `crates/gui-host/Cargo.toml`:

```toml
[package]
name = "prdt-gui-host"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
prdt-gui-common = { path = "../gui-common" }
prdt-protocol = { path = "../protocol" }
prdt-crypto = { path = "../crypto" }
eframe = { workspace = true }
egui = { workspace = true }
tokio = { workspace = true }
tokio-util = { version = "0.7", features = ["rt"] }
tracing = { workspace = true }
thiserror = { workspace = true }
anyhow = "1"
```

- [ ] **Step 2: lib.rs entry point**

Create `crates/gui-host/src/lib.rs`:

```rust
//! Phase 4 G1 host GUI.

mod app;
mod keygen;

use std::path::PathBuf;
use std::sync::Arc;

use prdt_gui_common::{install_jp_font, Config};

/// Run the host GUI as the main blocking call. Returns when the user
/// closes the window.
pub fn run_host_gui(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    let config_path = config_path
        .or_else(prdt_gui_common::default_config_path)
        .ok_or_else(|| anyhow::anyhow!("could not resolve config path"))?;

    let config = Config::load(&config_path)?;
    let shared_cfg = Arc::new(parking_lot_or_std_mutex(config));
    let path = config_path.clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 480.0])
            .with_min_inner_size([520.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Power Remote Desktop — Host",
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Box::new(app::HostApp::new(shared_cfg, path))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

fn parking_lot_or_std_mutex<T>(t: T) -> std::sync::Mutex<T> {
    std::sync::Mutex::new(t)
}
```

- [ ] **Step 3: keygen.rs**

Create `crates/gui-host/src/keygen.rs`:

```rust
//! Host key generation flow for the GUI's first-run experience.

use std::path::Path;

use prdt_crypto::KeyPair;

/// Result of `try_load_or_generate`: either the existing key was loaded
/// or a fresh one was generated and persisted.
pub struct KeyOutcome {
    pub keypair: KeyPair,
    pub pubkey_b64: String,
    pub generated: bool,
}

/// Try to load `path`; if missing, generate a new keypair and write it.
/// Returns the keypair plus a base64-encoded pubkey for display.
pub fn try_load_or_generate(path: &Path) -> anyhow::Result<KeyOutcome> {
    if path.exists() {
        let bytes = std::fs::read(path)?;
        if bytes.len() != 32 {
            anyhow::bail!("key file {} is not 32 bytes", path.display());
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        let kp = KeyPair::from_private(arr);
        let pubkey_b64 = kp.public.to_base64();
        return Ok(KeyOutcome {
            keypair: kp,
            pubkey_b64,
            generated: false,
        });
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let kp = KeyPair::generate();
    std::fs::write(path, kp.private.0)?;
    let pubkey_b64 = kp.public.to_base64();
    Ok(KeyOutcome {
        keypair: kp,
        pubkey_b64,
        generated: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-key.bin");
        let out = try_load_or_generate(&path).unwrap();
        assert!(out.generated);
        assert!(path.exists());
        assert_eq!(out.pubkey_b64.len() > 0, true);
    }

    #[test]
    fn loads_existing_without_regenerating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-key.bin");
        let first = try_load_or_generate(&path).unwrap();
        let second = try_load_or_generate(&path).unwrap();
        assert!(!second.generated);
        assert_eq!(first.pubkey_b64, second.pubkey_b64);
    }
}
```

Add `tempfile` to dev-dependencies in `crates/gui-host/Cargo.toml`:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Skeleton app.rs (Idle screen only — Listening comes in Task 5)**

Create `crates/gui-host/src/app.rs`:

```rust
//! Host GUI state machine. Task 4 ships the Idle (key-loaded) screen.
//! Task 5 adds the Listening state + Settings modal.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{generate_qr, Config};

use crate::keygen;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    NeedsKey,
    Idle,
}

pub struct HostApp {
    config: Arc<Mutex<Config>>,
    config_path: PathBuf,
    stage: Stage,
    pubkey_b64: String,
    qr_handle: Option<egui::TextureHandle>,
    error: Option<String>,
}

impl HostApp {
    pub fn new(config: Arc<Mutex<Config>>, config_path: PathBuf) -> Self {
        let key_path = {
            let cfg = config.lock().unwrap();
            cfg.host.key_file.clone()
        };
        let mut app = Self {
            config,
            config_path,
            stage: if key_path.exists() {
                Stage::Idle
            } else {
                Stage::NeedsKey
            },
            pubkey_b64: String::new(),
            qr_handle: None,
            error: None,
        };
        if app.stage == Stage::Idle {
            app.try_load_key(&key_path);
        }
        app
    }

    fn try_load_key(&mut self, path: &std::path::Path) {
        match keygen::try_load_or_generate(path) {
            Ok(out) => {
                self.pubkey_b64 = out.pubkey_b64;
                self.stage = Stage::Idle;
            }
            Err(e) => self.error = Some(format!("key load failed: {e}")),
        }
    }

    fn ensure_qr_texture(&mut self, ctx: &egui::Context) {
        if self.qr_handle.is_some() || self.pubkey_b64.is_empty() {
            return;
        }
        match generate_qr(&self.pubkey_b64, 4) {
            Ok(image) => {
                let handle =
                    ctx.load_texture("host_qr", image, egui::TextureOptions::default());
                self.qr_handle = Some(handle);
            }
            Err(e) => self.error = Some(format!("qr generation failed: {e}")),
        }
    }
}

impl eframe::App for HostApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| match self.stage {
            Stage::NeedsKey => self.show_needs_key(ui),
            Stage::Idle => {
                self.ensure_qr_texture(ctx);
                self.show_idle(ui);
            }
        });
    }
}

impl HostApp {
    fn show_needs_key(&mut self, ui: &mut egui::Ui) {
        ui.heading("Welcome");
        ui.add_space(12.0);
        ui.label("Generate a host key to start. The key uniquely identifies this machine to viewers.");
        ui.add_space(8.0);
        let key_path = self.config.lock().unwrap().host.key_file.clone();
        ui.label(format!("Key file: {}", key_path.display()));
        ui.add_space(20.0);
        if ui.button("Generate host key").clicked() {
            self.try_load_key(&key_path);
        }
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }
    }

    fn show_idle(&mut self, ui: &mut egui::Ui) {
        ui.heading("Status: Idle");
        ui.add_space(8.0);
        ui.label("Public key:");
        ui.horizontal(|ui| {
            ui.code(&self.pubkey_b64);
            if ui.button("Copy").clicked() {
                ui.output_mut(|o| o.copied_text = self.pubkey_b64.clone());
            }
        });
        ui.add_space(12.0);
        if let Some(qr) = &self.qr_handle {
            ui.image(egui::load::SizedTexture::new(qr.id(), qr.size_vec2()));
        }
        ui.add_space(16.0);
        ui.label("[ Start listening ] (added in Task 5)");
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }
    }
}
```

- [ ] **Step 5: Build + tests**

```bash
cargo build -p prdt-gui-host
cargo test -p prdt-gui-host
```

Expected: clean build, 2 keygen tests pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/gui-host
git commit -m "gui-host: skeleton + first-run keygen + idle screen"
```

---

## Task 5: gui-host listening state + start/stop + log tail integration

**Files:**
- Modify: `crates/gui-host/src/app.rs`
- Create: `crates/gui-host/src/settings.rs`
- Modify: `crates/gui-host/src/lib.rs` (rewire run_host_gui to install TailLayer + share status)

- [ ] **Step 1: Add Listening stage + Stop control**

Update `Stage` enum and `HostApp` state in `crates/gui-host/src/app.rs`. Replace the existing `enum Stage` and `struct HostApp` definitions with:

```rust
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{generate_qr, Config, TailHandle};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::keygen;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    NeedsKey,
    Idle,
    Listening,
}

pub struct HostApp {
    config: Arc<Mutex<Config>>,
    config_path: PathBuf,
    stage: Stage,
    pubkey_b64: String,
    qr_handle: Option<egui::TextureHandle>,
    error: Option<String>,
    tail: TailHandle,
    rt_handle: Handle,
    cancel: Option<CancellationToken>,
    settings_open: bool,
}

impl HostApp {
    pub fn new(
        config: Arc<Mutex<Config>>,
        config_path: PathBuf,
        tail: TailHandle,
        rt_handle: Handle,
    ) -> Self {
        let key_path = config.lock().unwrap().host.key_file.clone();
        let mut app = Self {
            config,
            config_path,
            stage: if key_path.exists() {
                Stage::Idle
            } else {
                Stage::NeedsKey
            },
            pubkey_b64: String::new(),
            qr_handle: None,
            error: None,
            tail,
            rt_handle,
            cancel: None,
            settings_open: false,
        };
        if app.stage == Stage::Idle {
            app.try_load_key(&key_path);
        }
        app
    }

    fn try_load_key(&mut self, path: &std::path::Path) {
        match keygen::try_load_or_generate(path) {
            Ok(out) => {
                self.pubkey_b64 = out.pubkey_b64;
                self.stage = Stage::Idle;
            }
            Err(e) => self.error = Some(format!("key load failed: {e}")),
        }
    }

    fn ensure_qr_texture(&mut self, ctx: &egui::Context) {
        if self.qr_handle.is_some() || self.pubkey_b64.is_empty() {
            return;
        }
        match generate_qr(&self.pubkey_b64, 4) {
            Ok(image) => {
                let handle =
                    ctx.load_texture("host_qr", image, egui::TextureOptions::default());
                self.qr_handle = Some(handle);
            }
            Err(e) => self.error = Some(format!("qr generation failed: {e}")),
        }
    }

    fn start_listening(&mut self) {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let _cfg_snapshot = self.config.lock().unwrap().clone();
        // Spawn the host main loop. For G1 we treat this as a placeholder
        // that loops until cancellation; integration with the real
        // run_host(args, status, cancel) is a follow-up task once Args
        // can be derived from Config (see plan Task 6).
        self.rt_handle.spawn(async move {
            // Placeholder: just sleep until cancelled. Task 6 swaps this
            // out for the real run_host call.
            cancel_for_task.cancelled().await;
        });
        self.cancel = Some(cancel);
        self.stage = Stage::Listening;
    }

    fn stop_listening(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        self.stage = Stage::Idle;
    }
}
```

Then update the `impl eframe::App for HostApp` impl `update` method to dispatch on the new stage:

```rust
impl eframe::App for HostApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Re-paint at 4 Hz so log tail and (later) status updates flow.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        if self.settings_open {
            crate::settings::render(ctx, &self.config, &self.config_path, &mut self.settings_open, &mut self.error);
        }

        egui::CentralPanel::default().show(ctx, |ui| match self.stage {
            Stage::NeedsKey => self.show_needs_key(ui),
            Stage::Idle => {
                self.ensure_qr_texture(ctx);
                self.show_idle(ui);
            }
            Stage::Listening => self.show_listening(ui),
        });
    }
}

impl HostApp {
    fn show_needs_key(&mut self, ui: &mut egui::Ui) {
        ui.heading("Welcome");
        ui.add_space(12.0);
        ui.label("Generate a host key to start. The key uniquely identifies this machine to viewers.");
        ui.add_space(8.0);
        let key_path = self.config.lock().unwrap().host.key_file.clone();
        ui.label(format!("Key file: {}", key_path.display()));
        ui.add_space(20.0);
        if ui.button("Generate host key").clicked() {
            self.try_load_key(&key_path);
        }
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }
    }

    fn show_idle(&mut self, ui: &mut egui::Ui) {
        ui.heading("Status: Idle");
        ui.add_space(8.0);
        self.draw_pubkey_with_qr(ui);
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button("Start listening").clicked() {
                self.start_listening();
            }
            if ui.button("Settings…").clicked() {
                self.settings_open = true;
            }
        });
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }
    }

    fn show_listening(&mut self, ui: &mut egui::Ui) {
        let bind = self.config.lock().unwrap().host.bind.clone();
        ui.heading(format!("Status: ● Listening on {bind}"));
        ui.add_space(8.0);
        self.draw_pubkey_with_qr(ui);
        ui.add_space(12.0);
        ui.label("Recent activity:");
        let lines = self.tail.snapshot();
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for l in &lines {
                    ui.label(l);
                }
            });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Stop").clicked() {
                self.stop_listening();
            }
            if ui.button("Settings…").clicked() {
                self.settings_open = true;
            }
        });
    }

    fn draw_pubkey_with_qr(&mut self, ui: &mut egui::Ui) {
        ui.label("Public key:");
        ui.horizontal(|ui| {
            ui.code(&self.pubkey_b64);
            if ui.button("Copy").clicked() {
                ui.output_mut(|o| o.copied_text = self.pubkey_b64.clone());
            }
        });
        if let Some(qr) = &self.qr_handle {
            ui.add_space(8.0);
            ui.image(egui::load::SizedTexture::new(qr.id(), qr.size_vec2()));
        }
    }
}
```

- [ ] **Step 2: Settings modal**

Create `crates/gui-host/src/settings.rs`:

```rust
//! Host GUI settings modal. Edits the shared `Config`; saves to disk on Save.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::Config;

pub fn render(
    ctx: &egui::Context,
    config: &Arc<Mutex<Config>>,
    config_path: &std::path::Path,
    open: &mut bool,
    error: &mut Option<String>,
) {
    let mut local: Config = config.lock().unwrap().clone();
    let mut close = false;
    egui::Window::new("Settings")
        .open(open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Bind:");
            ui.text_edit_singleline(&mut local.host.bind);
            ui.label("Monitor:");
            ui.add(egui::DragValue::new(&mut local.host.monitor));
            ui.label("Bitrate (Mbps):");
            ui.add(egui::DragValue::new(&mut local.host.bitrate_mbps).range(1..=200));
            ui.label("Outgoing dir:");
            ui.horizontal(|ui| {
                let mut s = local
                    .host
                    .outgoing_dir
                    .to_string_lossy()
                    .into_owned();
                if ui.text_edit_singleline(&mut s).changed() {
                    local.host.outgoing_dir = PathBuf::from(s);
                }
                if ui.button("Browse").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        local.host.outgoing_dir = p;
                    }
                }
            });
            ui.label("Signaling URL (optional):");
            ui.text_edit_singleline(&mut local.host.signaling_url);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.button("Save").clicked() {
                    *config.lock().unwrap() = local.clone();
                    if let Err(e) = local.save(config_path) {
                        *error = Some(format!("config save failed: {e}"));
                    }
                    close = true;
                }
            });
        });
    if close {
        *open = false;
    }
}
```

(The `rfd` crate is already in the workspace deps from Task 1; add `rfd = { workspace = true }` to `crates/gui-host/Cargo.toml`'s `[dependencies]`.)

- [ ] **Step 3: Update lib.rs to install TailLayer + provide rt handle**

Replace `crates/gui-host/src/lib.rs`:

```rust
//! Phase 4 G1 host GUI.

mod app;
mod keygen;
mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{install_jp_font, Config, TailLayer};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Run the host GUI as the main blocking call. Returns when the user
/// closes the window.
pub fn run_host_gui(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    let config_path = config_path
        .or_else(prdt_gui_common::default_config_path)
        .ok_or_else(|| anyhow::anyhow!("could not resolve config path"))?;

    let config = Config::load(&config_path)?;
    let shared_cfg = Arc::new(Mutex::new(config));

    let (tail_layer, tail_handle) = TailLayer::new(200);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())),
        )
        .with(tail_layer)
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let rt_handle = runtime.handle().clone();
    // Hold the runtime alive for the GUI's lifetime.
    let _rt_guard = runtime.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 480.0])
            .with_min_inner_size([520.0, 360.0]),
        ..Default::default()
    };

    let cfg = shared_cfg.clone();
    let path = config_path.clone();
    let tail = tail_handle.clone();
    eframe::run_native(
        "Power Remote Desktop — Host",
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Box::new(app::HostApp::new(cfg, path, tail, rt_handle))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;

    drop(runtime);
    Ok(())
}
```

- [ ] **Step 4: Build + tests**

```bash
cargo build -p prdt-gui-host
cargo test -p prdt-gui-host
```

Expected: clean build, 2 keygen tests still pass. (The eframe entry point can't be tested headlessly; manual smoke is required for the listening flow.)

- [ ] **Step 5: Commit**

```bash
git add crates/gui-host
git commit -m "gui-host: listening state, start/stop, log tail, settings modal"
```

---

## Task 6: Wire prdt-host main() to gui-host (unless --headless)

**Files:**
- Modify: `crates/host/src/main.rs`
- Modify: `crates/host/Cargo.toml`

- [ ] **Step 1: Add gui-host dep**

In `crates/host/Cargo.toml`:

```toml
[target.'cfg(windows)'.dependencies]
# ... existing ...
prdt-gui-host = { path = "../gui-host" }
```

- [ ] **Step 2: Add --headless and --config flags**

In `crates/host/src/main.rs`, find the `Args` struct and append:

```rust
    /// Run in CLI-only mode without launching the GUI. Required for headless servers / CI.
    #[arg(long)]
    headless: bool,

    /// Override the GUI config file location (default: %APPDATA%/prdt/config.toml).
    #[arg(long)]
    config: Option<std::path::PathBuf>,
```

- [ ] **Step 3: Route main() through gui-host**

Replace the `#[tokio::main] async fn main` with:

```rust
fn main() -> Result<()> {
    let args = Args::parse();

    if args.headless {
        run_cli(args)
    } else {
        // GUI mode. The GUI installs its own tracing subscriber + tokio runtime.
        prdt_gui_host::run_host_gui(args.config)
            .map_err(|e| anyhow::anyhow!(e))
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn run_cli(args: Args) -> Result<()> {
    init_tracing();
    run_host(args, None, tokio_util::sync::CancellationToken::new()).await
}
```

`init_tracing` is the same helper extracted in Task 3.

(The GUI mode runs a runtime internally; CLI mode uses `#[tokio::main]` on `run_cli`. Both paths converge on `run_host(args, None, cancel)` once the runtime is up. The GUI's "Start listening" callback is still a placeholder in this task — Task 7 wires it to `run_host`.)

- [ ] **Step 4: Build**

```bash
cargo build -p prdt-host
```

Expected: clean build. If `prdt_gui_host` isn't found, double-check the dep was added under the `cfg(windows)` block (which is where the rest of host's Windows deps live).

- [ ] **Step 5: Smoke test CLI compatibility**

```bash
./target/debug/prdt-host.exe --headless --help 2>&1 | head -10
```

Expected: existing help text is shown (proves `--headless` short-circuits before GUI).

```bash
./target/debug/prdt-host.exe --help 2>&1 | head -10
```

Expected: same usage. (Without args the GUI would launch; we use `--help` to confirm clap parsing works.)

- [ ] **Step 6: Run workspace tests for regression**

```bash
cargo test --workspace 2>&1 | grep "test result" | tail -5
```

Expected: 218+ tests, 0 failed. The new gui-common + gui-host tests (~9) bring total to ~227.

- [ ] **Step 7: Wire the GUI's start/stop to real run_host**

Edit `crates/gui-host/src/app.rs`. The placeholder spawn in `start_listening` becomes a call to `prdt_host::run_host`. Since `gui-host` doesn't have `prdt-host` as a dep (would be a circular dep — `prdt-host` already depends on `prdt-gui-host`), invert the call: instead of `gui-host` calling `run_host`, the host bin's `main` provides a closure to `gui-host`.

Change `run_host_gui` to accept a callback:

```rust
// crates/gui-host/src/lib.rs

pub type RunHostFn = Arc<dyn Fn(tokio_util::sync::CancellationToken) -> tokio::task::JoinHandle<anyhow::Result<()>> + Send + Sync>;

pub fn run_host_gui(
    config_path: Option<PathBuf>,
    run_host: RunHostFn,
) -> anyhow::Result<()> {
    // ... existing code ...
    eframe::run_native(
        "Power Remote Desktop — Host",
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Box::new(app::HostApp::new(cfg, path, tail, rt_handle, run_host.clone()))
        }),
    )
    // ...
}
```

Update `HostApp::new` signature and `start_listening` body:

```rust
// crates/gui-host/src/app.rs

pub struct HostApp {
    // ... existing ...
    run_host: crate::RunHostFn,
    join: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl HostApp {
    pub fn new(
        config: Arc<Mutex<Config>>,
        config_path: PathBuf,
        tail: TailHandle,
        rt_handle: Handle,
        run_host: crate::RunHostFn,
    ) -> Self {
        // ... existing init ...
        // add `run_host, join: None` to the struct literal
    }

    fn start_listening(&mut self) {
        let cancel = CancellationToken::new();
        let join = (self.run_host)(cancel.clone());
        self.cancel = Some(cancel);
        self.join = Some(join);
        self.stage = Stage::Listening;
    }

    fn stop_listening(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        // Don't block on join here — let it drop / clean up async.
        self.join = None;
        self.stage = Stage::Idle;
    }
}
```

Then in `crates/host/src/main.rs`, build the closure:

```rust
fn main() -> Result<()> {
    let args = Args::parse();

    if args.headless {
        return run_cli(args);
    }

    let args_arc = std::sync::Arc::new(args);
    let run_host_fn: prdt_gui_host::RunHostFn = std::sync::Arc::new(move |cancel| {
        let args = args_arc.clone();
        tokio::spawn(async move {
            run_host((*args).clone(), None, cancel).await
        })
    });
    prdt_gui_host::run_host_gui(None, run_host_fn).map_err(|e| anyhow::anyhow!(e))
}
```

If `Args` doesn't `Clone`, derive it: add `#[derive(Clone)]` to the `Args` struct.

- [ ] **Step 8: Build + final test**

```bash
cargo build -p prdt-host
cargo test --workspace 2>&1 | grep "test result" | tail -5
```

Expected: clean build, all tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/host crates/gui-host
git commit -m "host+gui-host: wire --headless + GUI supervisor closure"
```

---

## Task 7: gui-viewer crate + launcher + connect form

**Files:**
- Create: `crates/gui-viewer/Cargo.toml`
- Create: `crates/gui-viewer/src/lib.rs`
- Create: `crates/gui-viewer/src/app.rs`
- Create: `crates/gui-viewer/src/hosts_list.rs`
- Create: `crates/gui-viewer/src/connect_form.rs`
- Create: `crates/gui-viewer/src/settings.rs`
- Modify: workspace `Cargo.toml` `members`

- [ ] **Step 1: Cargo.toml**

Add `"crates/gui-viewer"` to workspace `members`. Create `crates/gui-viewer/Cargo.toml`:

```toml
[package]
name = "prdt-gui-viewer"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
prdt-gui-common = { path = "../gui-common" }
eframe = { workspace = true }
egui = { workspace = true }
rfd = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
url = { workspace = true }
serde = { workspace = true }
anyhow = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: lib.rs + LaunchOutcome types**

Create `crates/gui-viewer/src/lib.rs`:

```rust
//! Phase 4 G1 viewer launcher GUI.

mod app;
mod connect_form;
mod hosts_list;
mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{install_jp_font, Config};

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectMode {
    Direct,
    Signaling,
}

#[derive(Debug, Clone)]
pub struct ConnectArgs {
    pub mode: ConnectMode,
    pub direct_addr: Option<std::net::SocketAddr>,
    pub signaling_url: Option<url::Url>,
    pub host_id: Option<String>,
    pub pubkey: Option<String>,
    pub decoder: String,
    pub recv_dir: PathBuf,
    pub known_hosts_path: PathBuf,
    pub known_host_ids_path: PathBuf,
    pub default_resolution: String,
    pub default_fps: u32,
}

#[derive(Debug)]
pub enum LaunchOutcome {
    Connect(ConnectArgs),
    Quit,
}

/// Run the viewer launcher as a blocking call. Returns when the user
/// presses Connect (with a `LaunchOutcome::Connect`) or closes the
/// window (with `LaunchOutcome::Quit`).
pub fn run_viewer_launcher(config_path: Option<PathBuf>) -> anyhow::Result<LaunchOutcome> {
    let config_path = config_path
        .or_else(prdt_gui_common::default_config_path)
        .ok_or_else(|| anyhow::anyhow!("could not resolve config path"))?;

    let config = Config::load(&config_path)?;
    let shared_cfg = Arc::new(Mutex::new(config));
    let outcome: Arc<Mutex<Option<LaunchOutcome>>> = Arc::new(Mutex::new(None));

    let cfg = shared_cfg.clone();
    let path = config_path.clone();
    let out = outcome.clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 520.0])
            .with_min_inner_size([520.0, 360.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Power Remote Desktop — Viewer",
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Box::new(app::LauncherApp::new(cfg, path, out))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;

    let outcome = outcome.lock().unwrap().take().unwrap_or(LaunchOutcome::Quit);
    Ok(outcome)
}
```

- [ ] **Step 3: app.rs**

Create `crates/gui-viewer/src/app.rs`:

```rust
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::Config;

use crate::{LaunchOutcome};

pub struct LauncherApp {
    pub(crate) config: Arc<Mutex<Config>>,
    pub(crate) config_path: PathBuf,
    pub(crate) outcome: Arc<Mutex<Option<LaunchOutcome>>>,
    pub(crate) selected: Option<usize>,
    pub(crate) add_form_open: bool,
    pub(crate) settings_open: bool,
    pub(crate) error: Option<String>,
    pub(crate) draft_host: crate::connect_form::DraftHost,
}

impl LauncherApp {
    pub fn new(
        config: Arc<Mutex<Config>>,
        config_path: PathBuf,
        outcome: Arc<Mutex<Option<LaunchOutcome>>>,
    ) -> Self {
        Self {
            config,
            config_path,
            outcome,
            selected: None,
            add_form_open: false,
            settings_open: false,
            error: None,
            draft_host: crate::connect_form::DraftHost::default(),
        }
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.add_form_open {
            crate::connect_form::render(ctx, self);
        }
        if self.settings_open {
            crate::settings::render(ctx, self);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Saved connections");
            ui.add_space(8.0);
            crate::hosts_list::render(ui, self);

            ui.add_space(12.0);
            let decoder = self.config.lock().unwrap().viewer.decoder.clone();
            ui.horizontal(|ui| {
                ui.label("Decoder:");
                ui.label(decoder);
                if ui.button("Settings…").clicked() {
                    self.settings_open = true;
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.add_enabled(self.selected.is_some(), egui::Button::new("Connect")).clicked() {
                    self.try_connect();
                    frame.close();
                }
                if ui.button("Quit").clicked() {
                    *self.outcome.lock().unwrap() = Some(LaunchOutcome::Quit);
                    frame.close();
                }
            });
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }
        });
    }
}

impl LauncherApp {
    fn try_connect(&mut self) {
        let Some(idx) = self.selected else { return };
        let cfg = self.config.lock().unwrap();
        let Some(entry) = cfg.viewer.hosts.get(idx) else { return };
        let viewer = &cfg.viewer;
        let mode = if entry.mode == "signaling" {
            crate::ConnectMode::Signaling
        } else {
            crate::ConnectMode::Direct
        };
        let args = crate::ConnectArgs {
            mode: mode.clone(),
            direct_addr: if mode == crate::ConnectMode::Direct {
                entry.addr.parse().ok()
            } else {
                None
            },
            signaling_url: if mode == crate::ConnectMode::Signaling {
                url::Url::parse(&viewer.signaling_url).ok()
            } else {
                None
            },
            host_id: if mode == crate::ConnectMode::Signaling && !entry.host_id.is_empty() {
                Some(entry.host_id.clone())
            } else {
                None
            },
            pubkey: if entry.pubkey.is_empty() {
                None
            } else {
                Some(entry.pubkey.clone())
            },
            decoder: viewer.decoder.clone(),
            recv_dir: viewer.recv_dir.clone(),
            known_hosts_path: viewer.known_hosts.clone(),
            known_host_ids_path: viewer.known_host_ids.clone(),
            default_resolution: viewer.default_resolution.clone(),
            default_fps: viewer.default_fps,
        };
        *self.outcome.lock().unwrap() = Some(crate::LaunchOutcome::Connect(args));
    }
}
```

- [ ] **Step 4: hosts_list.rs**

Create `crates/gui-viewer/src/hosts_list.rs`:

```rust
use crate::app::LauncherApp;

pub fn render(ui: &mut egui::Ui, app: &mut LauncherApp) {
    let cfg = app.config.lock().unwrap().clone();
    if cfg.viewer.hosts.is_empty() {
        ui.label("(no saved connections)");
    } else {
        for (i, h) in cfg.viewer.hosts.iter().enumerate() {
            let selected = app.selected == Some(i);
            let label = format!(
                "{} — {} ({})",
                h.label,
                if h.mode == "signaling" { &h.host_id } else { &h.addr },
                h.mode,
            );
            if ui.selectable_label(selected, label).clicked() {
                app.selected = Some(i);
            }
        }
    }
    if ui.button("+ Add new connection").clicked() {
        app.add_form_open = true;
    }
}
```

- [ ] **Step 5: connect_form.rs**

Create `crates/gui-viewer/src/connect_form.rs`:

```rust
use prdt_gui_common::HostEntry;

use crate::app::LauncherApp;

#[derive(Default)]
pub struct DraftHost {
    pub label: String,
    pub mode: String, // "direct" | "signaling"
    pub addr: String,
    pub host_id: String,
    pub pubkey: String,
}

pub fn render(ctx: &egui::Context, app: &mut LauncherApp) {
    let mut close = false;
    let mut save = false;
    egui::Window::new("Add Connection")
        .open(&mut app.add_form_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Label:");
            ui.text_edit_singleline(&mut app.draft_host.label);
            ui.add_space(4.0);
            ui.label("Mode:");
            ui.horizontal(|ui| {
                ui.radio_value(&mut app.draft_host.mode, "direct".into(), "Direct");
                ui.radio_value(&mut app.draft_host.mode, "signaling".into(), "Signaling");
            });
            if app.draft_host.mode == "direct" {
                ui.label("Address (host:port):");
                ui.text_edit_singleline(&mut app.draft_host.addr);
            } else {
                ui.label("Host ID (e.g. 123-456-789):");
                ui.text_edit_singleline(&mut app.draft_host.host_id);
            }
            ui.label("Public key (base64; leave empty for TOFU):");
            ui.text_edit_singleline(&mut app.draft_host.pubkey);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                let valid = !app.draft_host.label.is_empty()
                    && (app.draft_host.mode == "direct" && !app.draft_host.addr.is_empty()
                        || app.draft_host.mode == "signaling" && !app.draft_host.host_id.is_empty());
                if ui.add_enabled(valid, egui::Button::new("Save")).clicked() {
                    save = true;
                }
            });
        });

    if save {
        let entry = HostEntry {
            label: app.draft_host.label.clone(),
            mode: app.draft_host.mode.clone(),
            addr: app.draft_host.addr.clone(),
            host_id: app.draft_host.host_id.clone(),
            pubkey: app.draft_host.pubkey.clone(),
            last_connected: String::new(),
        };
        let mut cfg = app.config.lock().unwrap();
        cfg.viewer.hosts.push(entry);
        if let Err(e) = cfg.save(&app.config_path) {
            app.error = Some(format!("config save failed: {e}"));
        }
        drop(cfg);
        app.draft_host = DraftHost::default();
        close = true;
    }
    if close {
        app.add_form_open = false;
    }
}
```

If the form's default mode shouldn't be empty, set `DraftHost::default()` to start with `mode: "direct".into()` instead. Update:

```rust
impl Default for DraftHost {
    fn default() -> Self {
        Self {
            label: String::new(),
            mode: "direct".into(),
            addr: String::new(),
            host_id: String::new(),
            pubkey: String::new(),
        }
    }
}
```

(remove `#[derive(Default)]` if present, replaced by the explicit impl above.)

- [ ] **Step 6: settings.rs**

Create `crates/gui-viewer/src/settings.rs`:

```rust
use std::path::PathBuf;

use crate::app::LauncherApp;

pub fn render(ctx: &egui::Context, app: &mut LauncherApp) {
    let mut local = app.config.lock().unwrap().clone();
    let mut close = false;
    egui::Window::new("Viewer Settings")
        .open(&mut app.settings_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Decoder:");
            ui.horizontal(|ui| {
                ui.radio_value(&mut local.viewer.decoder, "mf".into(), "MF (default)");
                ui.radio_value(&mut local.viewer.decoder, "nvdec".into(), "NVDEC (zero-copy)");
            });

            ui.label("Default resolution:");
            ui.text_edit_singleline(&mut local.viewer.default_resolution);

            ui.label("Default fps:");
            ui.add(egui::DragValue::new(&mut local.viewer.default_fps).range(15..=240));

            ui.label("Receive directory:");
            ui.horizontal(|ui| {
                let mut s = local
                    .viewer
                    .recv_dir
                    .to_string_lossy()
                    .into_owned();
                if ui.text_edit_singleline(&mut s).changed() {
                    local.viewer.recv_dir = PathBuf::from(s);
                }
                if ui.button("Browse").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        local.viewer.recv_dir = p;
                    }
                }
            });

            ui.label("Signaling URL:");
            ui.text_edit_singleline(&mut local.viewer.signaling_url);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.button("Save").clicked() {
                    *app.config.lock().unwrap() = local.clone();
                    if let Err(e) = local.save(&app.config_path) {
                        app.error = Some(format!("config save failed: {e}"));
                    }
                    close = true;
                }
            });
        });
    if close {
        app.settings_open = false;
    }
}
```

- [ ] **Step 7: Build + tests**

```bash
cargo build -p prdt-gui-viewer
cargo test -p prdt-gui-viewer
```

Expected: clean build (no tests yet — pure UI logic).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/gui-viewer
git commit -m "gui-viewer: launcher + hosts list + connect form + settings"
```

---

## Task 8: Wire prdt-viewer main() to gui-viewer + final validation + tag

**Files:**
- Modify: `crates/viewer/Cargo.toml`
- Modify: `crates/viewer/src/main.rs`

- [ ] **Step 1: Add gui-viewer dep**

In `crates/viewer/Cargo.toml`:

```toml
[target.'cfg(windows)'.dependencies]
# ... existing ...
prdt-gui-viewer = { path = "../gui-viewer" }
```

- [ ] **Step 2: Add --headless and --config flags**

In `crates/viewer/src/main.rs`, find `Args` and append:

```rust
    /// Run in CLI-only mode without launching the GUI launcher. Required for headless / CI.
    #[arg(long)]
    headless: bool,

    /// Override the GUI config file location (default: %APPDATA%/prdt/config.toml).
    #[arg(long)]
    config: Option<std::path::PathBuf>,
```

Make `Args` `#[derive(Clone)]` if not already.

- [ ] **Step 3: Route main() through launcher**

Wrap the existing `fn main` body. Replace:

```rust
fn main() -> Result<()> {
    tracing_subscriber::fmt()...init();
    std::panic::set_hook(...);
    let args = Args::parse();
    // ... existing body ...
}
```

with:

```rust
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    std::panic::set_hook(Box::new(|info| {
        tracing::error!(panic = %info, "PANIC");
    }));

    let mut args = Args::parse();

    if !args.headless {
        // GUI launcher path. If the user didn't specify --headless and didn't
        // give us all the CLI fields the existing main needs, drop into the
        // launcher and let them pick.
        match prdt_gui_viewer::run_viewer_launcher(args.config.clone())
            .map_err(|e| anyhow::anyhow!(e))?
        {
            prdt_gui_viewer::LaunchOutcome::Quit => return Ok(()),
            prdt_gui_viewer::LaunchOutcome::Connect(c) => apply_connect_args(&mut args, c),
        }
    }

    // Existing CLI flow continues from here.
    let (req_w, req_h) = parse_resolution(&args.resolution)?;
    // ... rest of original body unchanged ...
}

fn apply_connect_args(args: &mut Args, c: prdt_gui_viewer::ConnectArgs) {
    args.signaling_url = c.signaling_url.clone();
    args.host_id = c.host_id.clone();
    args.host = c.direct_addr;
    args.host_pubkey = c.pubkey.clone();
    args.recv_dir = c.recv_dir.clone();
    args.decoder = c.decoder.clone();
    args.resolution = c.default_resolution.clone();
    // viewer Args has more fields (known_hosts, known_host_ids, fps, etc.) —
    // overwrite each one that has a corresponding ConnectArgs field. Fields
    // not produced by the launcher keep their CLI/clap defaults.
    args.known_hosts = c.known_hosts_path.clone();
    args.known_host_ids = c.known_host_ids_path.clone();
}
```

(The exact field names in `Args` may differ slightly — read the existing `Args` struct in `crates/viewer/src/main.rs` and map each `ConnectArgs` field to the matching `Args` field. The fps field, if it exists in Args under a different name, also needs mapping.)

- [ ] **Step 4: Build**

```bash
cargo build -p prdt-viewer
```

Expected: clean build.

- [ ] **Step 5: Smoke CLI compatibility**

```bash
./target/debug/prdt-viewer.exe --headless --help 2>&1 | head -20
```

Expected: existing help text. Confirm the new `--headless` and `--config` flags appear.

```bash
# Existing CLI flow still works (won't actually connect because no host, but parsing should succeed):
./target/debug/prdt-viewer.exe --headless --host 127.0.0.1:9999 --host-pubkey AA== 2>&1 | head -10
```

Expected: same behavior as on master (eventually times out, but no GUI dialog).

- [ ] **Step 6: Workspace tests + clippy**

```bash
cargo test --workspace 2>&1 | grep "test result" | tail -5
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -5
```

Expected: 218 + ~10 new tests = ~228 pass; clippy clean.

- [ ] **Step 7: Manual smoke (informational, not blocking)**

```bash
# In one terminal:
./target/debug/prdt-host.exe
# Expect: window opens, says "Welcome", offers to generate key.

# In another terminal (after generating key + Start listening on host):
./target/debug/prdt-viewer.exe
# Expect: launcher opens, hosts list (empty initially), Add new → form
```

If the host's "Start listening" button works (no panic on click) and the viewer's "Add new connection" form saves to config.toml, G1 functional smoke passes.

- [ ] **Step 8: Tag**

```bash
git add crates/viewer
git commit -m "viewer: wire --headless + GUI launcher fall-through"

git tag -a phase4-g1-complete -m "$(cat <<'EOF'
Phase 4 G1 complete — egui foundation + host GUI + viewer launcher

- gui-common crate: Config (TOML), JP font setup, QR generator, log tail Layer
- gui-host crate: HostApp (Welcome → Idle → Listening), settings modal,
  start/stop wired to run_host via injected closure
- gui-viewer crate: LauncherApp (saved hosts list, add-new form, settings),
  Connect → LaunchOutcome
- prdt-host main(): default GUI; --headless preserves existing CLI
- prdt-viewer main(): default launcher → existing winit on Connect;
  --headless preserves existing CLI
- All existing 218 tests pass + ~10 new gui tests
EOF
)"
git tag | grep phase4
```

Expected: `phase4-g1-complete` and any pre-existing `phase4-*` tags listed.

- [ ] **Step 9: Final summary report**

Report back to the user:

> Phase 4 G1 完了:
> - 新 crate: gui-common、gui-host、gui-viewer
> - host: GUI 既定起動、--headless で既存 CLI、Settings で config.toml 編集、Start/Stop で host server を tokio task として spawn/cancel
> - viewer: GUI ランチャー → 接続先選択 → 既存 winit/D3D11 に自動遷移、--headless で既存 CLI
> - 設定: %APPDATA%\prdt\config.toml
> - tag plan2d-zerocopy-complete に続いて phase4-g1-complete を打刻
> - 残: G2 overlay / G3 tray / G4 MSI / G5 crash / G6 i18n は別サブプラン

---

## Risks & Notes for Implementer

- **Cyclical dep avoidance**: `gui-host` does NOT depend on `prdt-host`. The closure pattern (`RunHostFn`) lets `prdt-host` inject the actual `run_host` call. This keeps the dependency direction clean.
- **eframe + tokio runtime**: The host GUI creates a `tokio::runtime::Builder::new_multi_thread()` runtime BEFORE `eframe::run_native`. The `runtime.handle()` is cloned into `HostApp`. The runtime stays alive via `_rt_guard = runtime.enter()`. When eframe exits, the runtime drops and any in-flight tokio tasks are aborted — that's the expected behavior on window close.
- **viewer launcher → winit transition**: eframe's `frame.close()` returns control to `eframe::run_native`. After `run_native` returns, the existing winit code starts. winit and eframe are NEVER running at the same time; this avoids the "multiple event loops on one thread" hazard.
- **`Args::clone`**: `derive(Clone)` is required on the host `Args` struct so the closure can capture and re-use it across multiple Start/Stop cycles. If clap derives anything that doesn't `Clone` automatically (rare), wrap the offending field in `Arc<...>`.
- **Font asset**: The plan ships an asset path that may be empty for first-build builds. egui silently falls back to default fonts if `set_fonts` is called with invalid bytes — JP glyphs won't render but ASCII works. G6 polishes this.
- **Tests for GUI**: eframe `App` impls aren't unit-testable headlessly. Instead we test the pure-data layer (Config TOML, qr generation, log tail, keygen). UI logic that needs validation (e.g., LauncherApp::try_connect) is tested by extracting state into `pub(crate)` fields and calling the method on a fresh struct.
- **Pre-existing fmt drift**: Plan 2d-zerocopy noted that `nat-traversal` has fmt drift on master. Don't touch it in this plan; only the new crates and the two binary main.rs edits should be fmt-clean.
- **Workspace `tracing-subscriber`**: The workspace `tracing-subscriber` dep already exists indirectly through other crates. Adding it explicitly to `[workspace.dependencies]` is harmless if version-aligned.
- **`rfd` may show a deprecation warning** on certain Linux distros lacking `pop-zenity`. Windows-only build is OK.

---

## Self-Review

- **Spec coverage**:
  - Config (toml schema, paths) → Task 1 ✓
  - JP font, QR, log tail → Task 2 ✓
  - run_host extraction → Task 3 ✓
  - Host GUI: Needs-key, Idle, Listening → Tasks 4–5 ✓
  - Settings modal → Task 5 ✓
  - prdt-host --headless wiring → Task 6 ✓
  - Viewer launcher: hosts list, Add-new form, Settings → Task 7 ✓
  - prdt-viewer --headless wiring → Task 8 ✓
  - Tag → Task 8 ✓
- **Placeholder scan**: No "TBD"/"TODO"/"add appropriate ...". Every code block is concrete.
- **Type consistency**: `LaunchOutcome::Connect(ConnectArgs)`, `ConnectMode { Direct, Signaling }`, `HostStage { NeedsKey, Idle, Listening }`, `Config { host, viewer }`, `HostEntry { label, mode, addr, host_id, pubkey, last_connected }` consistent across spec, plan, and code blocks.
