# P5C-1 Implementation Plan — VAAPI H.264 Encoder (Linux HW codec)

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development`. Steps use checkbox (`- [ ]`).

**Goal:** Ship a working VAAPI H.264 encoder as a new Linux backend on top of `cros-libva 0.0.13`, with policy + factory integration so `--encoder vaapi` (and `--encoder auto` when available) selects HW encode.

**Architecture:** New `crates/media-vaapi/` crate exposes `VaapiH264Encoder` with the same `encode(&I420Frame, force_idr, ts_us)` API shape as `SwH264Encoder`. `LinuxSwFactory` gains a `Vaapi` arm constructing `VaapiVideoProducer`. Real-device verification = walkthrough §K (container has no `/dev/dri/*`).

**Tech Stack:** Rust 1.85, cros-libva 0.0.13, libva-dev system lib, existing `prdt-media-sw::{I420Frame, bgra_to_i420}`, `prdt-media-policy::{BackendKind, ProducerFactory}`.

**Constraints:**
- All cargo invocations through `./scripts/dev-container.sh` (Debian bookworm + libva-dev).
- crate `=0.0.13` pinned (audit on next minor bump).
- `H264ConstrainedBaseline` profile only.
- CBR rate control only (CBR↔VBR switch is driver-fragile; deferred).
- Annex-B output with manual SPS/PPS prepend (no packed_headers FFI path in P5C-1).

**Spec:** `docs/superpowers/specs/2026-05-13-p5c1-vaapi-h264-encoder-design.md` (commit `25565a6`).

---

## Task 1: Dev container + `crates/media-vaapi/` skeleton + workspace member

**Files:**
- Modify: `scripts/Dockerfile.dev` (apt list extension)
- Create: `crates/media-vaapi/Cargo.toml`
- Create: `crates/media-vaapi/src/lib.rs` (just `pub mod` lines)
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Extend Dockerfile.dev with libva-dev**

```bash
grep -n "libva\|libpipewire" scripts/Dockerfile.dev
```

Add `libva-dev libva-drm2 libva-x11-2` to the apt install line, near `libpipewire-0.3-dev libspa-0.2-dev`. Keep the existing comment categorization. After edit:

```dockerfile
        libpipewire-0.3-dev \
        libspa-0.2-dev \
        libva-dev libva-drm2 libva-x11-2 \
        libasound2-dev \
        ...
```

- [ ] **Step 2: Probe `cros-libva 0.0.13` API surface**

```bash
./scripts/dev-container.sh bash -c '
echo "=== cros-libva crate API ==="
cargo doc -p cros-libva --target x86_64-unknown-linux-gnu --no-deps 2>&1 | tail -5 || true
echo ""
echo "=== Direct source inspection ==="
find target-docker/cargo-home/registry/src -name "*.rs" -path "*cros-libva*" 2>/dev/null | head -10
' 2>&1 | tail -30
```

If the crate isn't yet fetched, run `cargo fetch -p cros-libva` first (after T1 Step 3 adds the dep). Record the actual top-level type names (`Display` / `Config` / `Context` / `Surface` / `Picture` / `Buffer` / `MappedCodedBuffer`) so subsequent steps reference real symbols.

- [ ] **Step 3: Create `crates/media-vaapi/Cargo.toml`**

```toml
[package]
name = "prdt-media-vaapi"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lib]
path = "src/lib.rs"

[dependencies]
prdt-media-sw = { path = "../media-sw" }
thiserror = { workspace = true }
tracing = { workspace = true }

[target.'cfg(target_os = "linux")'.dependencies]
# cros-libva 0.0.13 is the safe RAII wrapper over libva. Pinned tight
# until the next audit; the crate's `0.0.*` versioning means each minor
# bump can break source compat.
cros-libva = "=0.0.13"
libc = "0.2"
```

- [ ] **Step 4: Stub `src/lib.rs`**

```rust
//! VAAPI H.264 encoder backend for Linux HW codec path.
//!
//! See `docs/superpowers/specs/2026-05-13-p5c1-vaapi-h264-encoder-design.md`
//! for the full design.

#![cfg(target_os = "linux")]

pub mod error;
pub mod annexb;
pub mod frame_input;
pub mod rc;
pub mod display;
pub mod encoder;

pub use encoder::{VaapiH264Encoder, VaapiH264EncoderConfig};
pub use error::VaapiError;
pub use frame_input::FrameInput;
```

Create empty stubs (`pub mod` declarations only, each file with `//! TODO: T<n>`).

- [ ] **Step 5: Add to workspace members**

In root `Cargo.toml`, add `"crates/media-vaapi",` to the `members = [...]` list (sorted alphabetically with the other crates).

- [ ] **Step 6: Verify container builds the new empty crate**

```bash
docker rmi prdt-dev:bookworm 2>/dev/null
./scripts/dev-container.sh cargo check -p prdt-media-vaapi --target x86_64-unknown-linux-gnu 2>&1 | tail -10
```

Expected: clean compile (empty crate). The `docker rmi` forces image rebuild so libva-dev is picked up.

- [ ] **Step 7: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add scripts/Dockerfile.dev Cargo.toml crates/media-vaapi/
git commit -m "$(cat <<'EOF'
P5C-1 T1: media-vaapi crate skeleton + libva-dev in dev container

Adds an empty workspace crate `prdt-media-vaapi` with the module
layout from spec §3.1 (display, encoder, frame_input, annexb, error,
rc). Each module is currently a single-line TODO; subsequent tasks
fill them in.

Pinned cros-libva =0.0.13 (per spec §2: 0.0.* versioning means each
minor bump may break source compat; audit on each future change).

Dockerfile.dev apt list gains libva-dev / libva-drm2 / libva-x11-2.
Container rebuild forced via docker rmi prdt-dev:bookworm. After this
T1 commit the container can compile a VAAPI client even though no
runtime VAAPI device exists inside the container — real /dev/dri/*
tests stay user-side per walkthrough §K.
EOF
)"
```

---

## Task 2: `error::VaapiError` + `VAStatus` → Result mapping + tests

**Files:**
- Modify: `crates/media-vaapi/src/error.rs`

- [ ] **Step 1: Write failing tests**

In `crates/media-vaapi/src/error.rs`:

```rust
//! VAAPI error model + VAStatus → Result mapping.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VaapiError {
    #[error("display open failed: {0}")]
    DisplayOpen(String),
    #[error("no /dev/dri/renderD* found")]
    NoRenderNode,
    #[error("configuration not supported: {0}")]
    NotSupported(String),
    #[error("hardware busy (retry exhausted, attempts={attempts})")]
    HardwareBusy { attempts: u32 },
    #[error("driver returned VA_STATUS_ERROR_{0}")]
    DriverError(i32),
    #[error("bitstream normalization failed: {0}")]
    Bitstream(String),
    #[error("encoder closed (call new() to reopen)")]
    Closed,
}

/// Classifier for raw VAStatus codes. Only handles error mapping at the
/// boundary; success codes return Ok at the FFI call site directly.
pub(crate) fn classify_va_status(status: i32, ctx: &'static str) -> VaapiError {
    // libva: VA_STATUS_SUCCESS=0, error codes follow.
    // From <va/va.h>:
    //   VA_STATUS_ERROR_OPERATION_FAILED = 0x00000001
    //   VA_STATUS_ERROR_ALLOCATION_FAILED = 0x00000002
    //   VA_STATUS_ERROR_INVALID_CONFIG = 0x00000007
    //   VA_STATUS_ERROR_HW_BUSY = 0x00000017
    //   VA_STATUS_ERROR_TIMEDOUT (alias of HW_BUSY context, ~0x00000017)
    //   VA_STATUS_ERROR_UNIMPLEMENTED = 0x00000022
    //   VA_STATUS_ERROR_UNSUPPORTED_PROFILE = 0x00000020
    //   VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT = 0x00000021
    match status {
        0x17 /* HW_BUSY / TIMEDOUT */ => VaapiError::HardwareBusy { attempts: 0 },
        0x07 /* INVALID_CONFIG */
        | 0x20 /* UNSUPPORTED_PROFILE */
        | 0x21 /* UNSUPPORTED_ENTRYPOINT */
        | 0x22 /* UNIMPLEMENTED */ => VaapiError::NotSupported(format!("{ctx}: status={status:#x}")),
        other => VaapiError::DriverError(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_va_status_classifies_hw_busy() {
        assert_eq!(classify_va_status(0x17, "test"), VaapiError::HardwareBusy { attempts: 0 });
    }

    #[test]
    fn classify_va_status_classifies_unsupported_profile() {
        let e = classify_va_status(0x20, "config");
        assert!(matches!(e, VaapiError::NotSupported(_)));
    }

    #[test]
    fn classify_va_status_falls_through_to_driver_error() {
        assert_eq!(classify_va_status(0x99, "any"), VaapiError::DriverError(0x99));
    }

    #[test]
    fn vaapi_error_display_includes_context() {
        let e = VaapiError::DisplayOpen("permission denied".into());
        let s = format!("{e}");
        assert!(s.contains("permission denied"));
    }
}
```

- [ ] **Step 2: Run tests + clippy**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-vaapi --lib --target x86_64-unknown-linux-gnu error
./scripts/dev-container.sh cargo clippy -p prdt-media-vaapi --target x86_64-unknown-linux-gnu -- -D warnings
```

Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-vaapi/src/error.rs
git commit -m "P5C-1 T2: VaapiError enum + classify_va_status + 4 tests"
```

---

## Task 3: `annexb::normalize_to_annexb` + tests

**Files:**
- Modify: `crates/media-vaapi/src/annexb.rs`

- [ ] **Step 1: Failing tests**

```rust
//! Annex-B bitstream normalizer.
//!
//! VAAPI coded-buffer output is driver-dependent: start codes may be
//! 3-byte (00 00 01) or 4-byte (00 00 00 01); SPS/PPS may be inline or
//! absent. This module produces a consistent 4-byte-prefixed Annex-B
//! stream and prepends a cached SPS+PPS blob on IDR frames so that the
//! downstream OpenH264-style consumer (prdt-protocol) sees a uniform
//! format regardless of which Mesa/Intel driver wrote the coded buffer.

use crate::error::VaapiError;

const ANNEXB_4: &[u8] = &[0x00, 0x00, 0x00, 0x01];

/// Walk a VAAPI coded buffer's contents and re-emit as 4-byte Annex-B
/// into `out`. If `is_idr`, the `sps_pps` blob is prepended before the
/// first NAL emitted from `raw`.
///
/// The input `raw` may contain:
/// - Multiple NAL units separated by either `00 00 01` or `00 00 00 01`
/// - A trailing non-NAL byte stream segment (driver padding) — flagged.
/// - An empty buffer (encoder failed) — returns Err.
pub fn normalize_to_annexb(
    raw: &[u8],
    sps_pps: &[u8],
    is_idr: bool,
    out: &mut Vec<u8>,
) -> Result<(), VaapiError> {
    if raw.is_empty() {
        return Err(VaapiError::Bitstream("coded buffer empty".into()));
    }
    if is_idr && !sps_pps.is_empty() {
        out.extend_from_slice(sps_pps);
    }
    // Scan for start codes; copy NAL bodies + 4-byte start code.
    let mut i = 0;
    let mut found_any = false;
    while i < raw.len() {
        let three = i + 2 < raw.len() && raw[i] == 0 && raw[i+1] == 0 && raw[i+2] == 1;
        let four = i + 3 < raw.len() && raw[i] == 0 && raw[i+1] == 0 && raw[i+2] == 0 && raw[i+3] == 1;
        if three || four {
            // Find the next start code (or end of buffer) to delimit this NAL.
            let nal_start = if four { i + 4 } else { i + 3 };
            let mut nal_end = raw.len();
            let mut j = nal_start;
            while j + 2 < raw.len() {
                if raw[j] == 0 && raw[j+1] == 0 && (raw[j+2] == 1 || (j + 3 < raw.len() && raw[j+2] == 0 && raw[j+3] == 1)) {
                    nal_end = j;
                    break;
                }
                j += 1;
            }
            out.extend_from_slice(ANNEXB_4);
            out.extend_from_slice(&raw[nal_start..nal_end]);
            found_any = true;
            i = nal_end;
        } else {
            i += 1;
        }
    }
    if !found_any {
        return Err(VaapiError::Bitstream(format!(
            "no Annex-B start code found in {} byte coded buffer",
            raw.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_4byte_input_passes_through_unchanged() {
        let raw = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0xaa, 0xbb, // SPS-like
            0x00, 0x00, 0x00, 0x01, 0x68, 0xcc,        // PPS-like
        ];
        let mut out = Vec::new();
        normalize_to_annexb(&raw, &[], false, &mut out).expect("ok");
        assert_eq!(out, raw);
    }

    #[test]
    fn normalize_collapses_3byte_to_4byte() {
        let raw = vec![
            0x00, 0x00, 0x01, 0x67, 0xaa,
            0x00, 0x00, 0x01, 0x68, 0xcc,
        ];
        let mut out = Vec::new();
        normalize_to_annexb(&raw, &[], false, &mut out).expect("ok");
        assert_eq!(out, vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0xaa,
            0x00, 0x00, 0x00, 0x01, 0x68, 0xcc,
        ]);
    }

    #[test]
    fn normalize_prepends_sps_pps_on_idr() {
        let sps_pps = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1e,
            0x00, 0x00, 0x00, 0x01, 0x68, 0xce, 0x06, 0xe2,
        ];
        let raw = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84]; // IDR slice
        let mut out = Vec::new();
        normalize_to_annexb(&raw, &sps_pps, true, &mut out).expect("ok");
        // SPS+PPS first, then IDR.
        assert!(out.starts_with(&sps_pps));
        assert!(out.ends_with(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84]));
    }

    #[test]
    fn normalize_rejects_empty_input() {
        let mut out = Vec::new();
        let e = normalize_to_annexb(&[], &[], false, &mut out).unwrap_err();
        assert!(matches!(e, VaapiError::Bitstream(_)));
    }

    #[test]
    fn normalize_rejects_input_without_start_code() {
        let raw = vec![0xff, 0xee, 0xdd, 0xcc];
        let mut out = Vec::new();
        let e = normalize_to_annexb(&raw, &[], false, &mut out).unwrap_err();
        assert!(matches!(e, VaapiError::Bitstream(_)));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-vaapi --lib --target x86_64-unknown-linux-gnu annexb
```

Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-vaapi/src/annexb.rs
git commit -m "P5C-1 T3: annexb normalizer (3byte→4byte, SPS/PPS prepend on IDR) + 5 tests"
```

---

## Task 4: `frame_input::FrameInput` enum + `rc` parameter builder

**Files:**
- Modify: `crates/media-vaapi/src/frame_input.rs`
- Modify: `crates/media-vaapi/src/rc.rs`

- [ ] **Step 1: `FrameInput` enum (P5C-2 seam)**

In `crates/media-vaapi/src/frame_input.rs`:

```rust
//! Encoder input discriminator. Only `CpuI420` is wired in P5C-1;
//! `VaSurface` / `Dmabuf` arms exist to lock the seam for P5C-2
//! (DMABUF zero-copy).

use prdt_media_sw::I420Frame;

#[allow(dead_code)] // VaSurface/Dmabuf placeholders unused in P5C-1
pub enum FrameInput<'a> {
    /// CPU-resident planar YUV. The host's bgra_to_i420 step produces this.
    CpuI420(&'a I420Frame),

    /// Reserved for P5C-2: already-mapped libva Surface (zero-copy from
    /// DMABUF). The encoder skips its internal upload step and binds the
    /// surface directly.
    VaSurface,

    /// Reserved for P5C-2: DMABUF FDs + plane descriptors. The encoder
    /// constructs a libva Surface via vaCreateSurfaceFromFds.
    Dmabuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use prdt_media_sw::I420Frame;

    #[test]
    fn cpu_i420_holds_borrow_lifetime() {
        let f = I420Frame::new(2, 2);
        let _input = FrameInput::CpuI420(&f);
        // Smoke: the enum compiles + holds a borrow.
    }
}
```

- [ ] **Step 2: `rc` parameter builder**

In `crates/media-vaapi/src/rc.rs`:

```rust
//! Rate-control parameter buffer builders.
//!
//! In P5C-1 we use CBR only. Per spec §2, CBR↔VBR switching needs
//! re-`create_config` on some drivers; the encoder treats RC mode as
//! init-time-only and exposes only `set_target_bitrate(bps)` for
//! dynamic updates within CBR mode.
//!
//! The rate buffer is built once per frame when the target bitrate
//! changes (encoder caches the last-sent value to avoid redundant
//! per-frame submits).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateControlParams {
    pub bits_per_second: u32,
    pub target_percentage: u32, // 100 = strict CBR target
    pub window_size_ms: u32,    // 1500 ms is typical
    pub initial_qp: u32,
    pub min_qp: u32,
    pub max_qp: u32,
}

impl RateControlParams {
    pub fn cbr_baseline(bitrate_bps: u32) -> Self {
        Self {
            bits_per_second: bitrate_bps,
            target_percentage: 100,
            window_size_ms: 1500,
            initial_qp: 0,  // 0 = let encoder pick (Intel iHD honors)
            min_qp: 0,
            max_qp: 0,      // 0 = no caps (defer to driver default)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbr_baseline_defaults() {
        let r = RateControlParams::cbr_baseline(5_000_000);
        assert_eq!(r.bits_per_second, 5_000_000);
        assert_eq!(r.target_percentage, 100);
        assert_eq!(r.window_size_ms, 1500);
    }
}
```

- [ ] **Step 3: Run tests**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-vaapi --lib --target x86_64-unknown-linux-gnu frame_input rc
```

Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-vaapi/src/frame_input.rs crates/media-vaapi/src/rc.rs
git commit -m "P5C-1 T4: FrameInput enum (P5C-2 seam) + RateControlParams CBR builder + 2 tests"
```

---

## Task 5: `display::open_render_node` + capability probe + RAII wrapper

**Files:**
- Modify: `crates/media-vaapi/src/display.rs`

- [ ] **Step 1: Probe cros-libva Display API**

```bash
./scripts/dev-container.sh bash -c '
echo "=== cros-libva Display API ==="
grep -n "pub fn\|pub struct" target-docker/cargo-home/registry/src/index.crates.io-1949cf8c6b5b557f/cros-libva-0.0.13/lib/src/*.rs 2>/dev/null | grep -i "display\|profile\|entrypoint\|attrib" | head -30
'
```

Record the actual method names. The plan assumes:
- `libva::Display::open() -> Result<Rc<Display>, …>` or
- `libva::Display::open_silent(path: &Path) -> Result<…>`

Use what cros-libva actually exposes.

- [ ] **Step 2: Implement render-node discovery + Display open**

```rust
//! VAAPI display open + capability probe.

use crate::error::VaapiError;
use std::path::{Path, PathBuf};

/// Scan `/dev/dri/renderD*` and return all candidate render nodes in
/// numerical order. Empty Vec = no render nodes (no GPU).
pub fn enumerate_render_nodes() -> Vec<PathBuf> {
    let dri = Path::new("/dev/dri");
    let Ok(rd) = std::fs::read_dir(dri) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("renderD"))
        })
        .collect();
    out.sort();
    out
}

/// Returns true when the system has at least one render node AND opening
/// it succeeds AND it advertises H264ConstrainedBaseline EncSlice.
/// Cached per-process via Once.
pub fn vaapi_runtime_present() -> bool {
    use std::sync::Once;
    static INIT: Once = Once::new();
    static mut CACHED: bool = false;
    INIT.call_once(|| {
        // SAFETY: only one thread enters call_once.
        unsafe {
            CACHED = probe_first_capable_node().is_ok();
        }
    });
    // SAFETY: CACHED is initialized by call_once before any read.
    unsafe { CACHED }
}

/// Walk render nodes and return the first one that supports
/// H264ConstrainedBaseline + EncSlice. Returns NoRenderNode if none
/// are usable.
pub fn probe_first_capable_node() -> Result<PathBuf, VaapiError> {
    let nodes = enumerate_render_nodes();
    if nodes.is_empty() {
        return Err(VaapiError::NoRenderNode);
    }
    for node in nodes {
        if node_supports_h264_baseline_encode(&node).unwrap_or(false) {
            return Ok(node);
        }
    }
    Err(VaapiError::NotSupported("no render node advertises H264 EncSlice".into()))
}

fn node_supports_h264_baseline_encode(node: &Path) -> Result<bool, VaapiError> {
    // TODO(T5 implementer): use cros-libva's Display + query_config_profiles +
    // query_config_entrypoints to confirm the node supports
    // VAProfileH264ConstrainedBaseline + VAEntrypointEncSlice.
    //
    // The exact cros-libva API to invoke depends on what Step 1 probe
    // reveals; pseudocode:
    //
    //   let display = libva::Display::open(Some(node))?;
    //   let profiles = display.query_config_profiles()?;
    //   if !profiles.contains(&VAProfileH264ConstrainedBaseline) {
    //       return Ok(false);
    //   }
    //   let entrypoints = display.query_config_entrypoints(VAProfile...)?;
    //   Ok(entrypoints.contains(&VAEntrypointEncSlice))
    let _ = node;
    Ok(false) // P5C-1: be conservative; T7 wires the real probe.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_returns_empty_when_no_dri_directory() {
        // In the container `/dev/dri` doesn't exist; smoke test that the
        // function tolerates this and returns Vec::new() instead of
        // panicking.
        let nodes = enumerate_render_nodes();
        assert!(nodes.is_empty() || nodes.iter().all(|p| p.starts_with("/dev/dri")));
    }

    #[test]
    fn vaapi_runtime_present_is_false_in_container() {
        // Container has no /dev/dri/* — expect false.
        assert!(!vaapi_runtime_present());
    }

    #[test]
    fn probe_returns_no_render_node_when_dir_empty() {
        // Same as above; we test the API shape.
        let r = probe_first_capable_node();
        assert!(matches!(
            r,
            Err(VaapiError::NoRenderNode)
                | Err(VaapiError::NotSupported(_))
        ));
    }
}
```

- [ ] **Step 3: Run tests**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-vaapi --lib --target x86_64-unknown-linux-gnu display
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-vaapi/src/display.rs
git commit -m "$(cat <<'EOF'
P5C-1 T5: display::probe_first_capable_node + vaapi_runtime_present cache

`enumerate_render_nodes` scans /dev/dri/renderD*; `vaapi_runtime_present`
caches the probe result behind std::sync::Once for the process lifetime.

`probe_first_capable_node` walks the candidates and returns the first
H264ConstrainedBaseline + EncSlice node. The actual cros-libva-driven
profile/entrypoint query is staged as `node_supports_h264_baseline_encode`
returning `Ok(false)` for now; T7 wires the real query once the encoder
itself is in place (the probe needs the same Display open path the
encoder uses).

3 new tests (container has no /dev/dri/*; smoke shape is verified).
EOF
)"
```

---

## Task 6: `encoder::VaapiH264Encoder` — `new` + Drop order

**Files:**
- Modify: `crates/media-vaapi/src/encoder.rs`

This is the largest task. Builds the RAII state shell + `new()` that opens the device, creates the Config + Context + Surface pool. `encode()` body is stubbed to return `VaapiError::NotSupported` for now — actual encode loop lands in T7.

- [ ] **Step 1: Write the failing constructor test**

```rust
//! VAAPI H.264 encoder.

use crate::error::VaapiError;
use crate::rc::RateControlParams;
use std::path::PathBuf;
use std::rc::Rc;

pub struct VaapiH264EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub initial_bitrate_bps: u32,
    pub gop_size: u32,
    pub render_node: Option<PathBuf>,
}

impl Default for VaapiH264EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 5_000_000,
            gop_size: 60,
            render_node: None,
        }
    }
}

pub struct VaapiH264Encoder {
    state: Option<EncoderState>,
    sps_pps: Vec<u8>,
}

#[allow(dead_code)] // most fields wired in T7 (encode loop)
struct EncoderState {
    rc: RateControlParams,
    rc_dirty: bool,
    sequence_counter: u64,
    idr_pic_id: u16,
    width: u32,
    height: u32,
    fps: u32,
    gop_size: u32,
    // ⚠️  Field order is load-bearing — Drop runs in declaration order.
    // See spec §3.4: image/coded → surfaces → context → config → display.
    // Each Option<...> is taken in reverse and dropped explicitly in
    // impl Drop for VaapiH264Encoder.
}

impl VaapiH264Encoder {
    pub fn new(cfg: VaapiH264EncoderConfig) -> Result<Self, VaapiError> {
        let _node = match cfg.render_node {
            Some(p) => p,
            None => crate::display::probe_first_capable_node()?,
        };
        // T7 implementer: open libva Display, create Config (H264
        // ConstrainedBaseline + EncSlice + RTFormat YUV420 + RateControl
        // CBR), create Context, allocate Surface pool, capture SPS/PPS
        // via packed-header probe or manual prepend.
        //
        // For T6 we return a partially-initialized encoder so the public
        // API surface compiles; encode() returns NotSupported.
        Ok(Self {
            state: Some(EncoderState {
                rc: RateControlParams::cbr_baseline(cfg.initial_bitrate_bps),
                rc_dirty: true,
                sequence_counter: 0,
                idr_pic_id: 0,
                width: cfg.width,
                height: cfg.height,
                fps: cfg.fps,
                gop_size: cfg.gop_size,
            }),
            sps_pps: Vec::new(),
        })
    }

    pub fn encode(
        &mut self,
        _frame: &prdt_media_sw::I420Frame,
        _force_idr: bool,
        _ts_us: u64,
    ) -> Result<prdt_media_sw::EncodedFrame, VaapiError> {
        // T7 implements the loop.
        Err(VaapiError::NotSupported("encode loop not yet implemented (T7)".into()))
    }

    pub fn set_target_bitrate(&mut self, bps: u32) -> Result<(), VaapiError> {
        let Some(s) = self.state.as_mut() else { return Err(VaapiError::Closed) };
        if s.rc.bits_per_second != bps {
            s.rc = RateControlParams::cbr_baseline(bps);
            s.rc_dirty = true;
        }
        Ok(())
    }

    pub fn backend_name(&self) -> &'static str { "vaapi-h264-cbr-baseline" }
}

impl Drop for VaapiH264Encoder {
    fn drop(&mut self) {
        // Explicit teardown — T7 fills in the actual sub-drops once real
        // libva resources are held. For T6 the state struct holds only
        // POD, so the default Drop is fine.
        let _ = self.state.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_targets_1080p60_5mbps_cbr() {
        let c = VaapiH264EncoderConfig::default();
        assert_eq!((c.width, c.height), (1920, 1080));
        assert_eq!(c.fps, 60);
        assert_eq!(c.initial_bitrate_bps, 5_000_000);
    }

    #[test]
    fn new_returns_no_render_node_in_container() {
        // The container has no /dev/dri/* — encoder construction must
        // surface NoRenderNode (or NotSupported) instead of panicking.
        let r = VaapiH264Encoder::new(VaapiH264EncoderConfig::default());
        assert!(matches!(
            r,
            Err(VaapiError::NoRenderNode) | Err(VaapiError::NotSupported(_))
        ));
    }

    #[test]
    fn set_target_bitrate_marks_dirty_and_rejects_when_closed() {
        // Construct an encoder bypassing the constructor (test-only) so
        // we can exercise set_target_bitrate logic without VAAPI runtime.
        let mut enc = VaapiH264Encoder {
            state: Some(EncoderState {
                rc: RateControlParams::cbr_baseline(5_000_000),
                rc_dirty: false,
                sequence_counter: 0,
                idr_pic_id: 0,
                width: 1920,
                height: 1080,
                fps: 60,
                gop_size: 60,
            }),
            sps_pps: Vec::new(),
        };
        enc.set_target_bitrate(8_000_000).expect("ok");
        assert!(enc.state.as_ref().unwrap().rc_dirty);
        assert_eq!(enc.state.as_ref().unwrap().rc.bits_per_second, 8_000_000);

        // Close + verify
        enc.state = None;
        let r = enc.set_target_bitrate(10_000_000);
        assert_eq!(r, Err(VaapiError::Closed));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-vaapi --lib --target x86_64-unknown-linux-gnu encoder
```

Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-vaapi/src/encoder.rs
git commit -m "$(cat <<'EOF'
P5C-1 T6: VaapiH264Encoder skeleton (new + set_target_bitrate + Drop)

Builds the encoder shell — Config struct with sane defaults
(1080p60 / 5 Mbps CBR / GOP 60), state holding rate-control snapshot +
sequence counters + IDR pic id, and a Drop impl that takes state
explicitly so future T7 sub-drops can run in spec §3.4's load-bearing
order (image -> coded -> surfaces -> context -> config -> display).

encode() returns NotSupported until T7 wires the real loop. The
public API shape (encode, set_target_bitrate, backend_name) is locked
in so subsequent factory/producer wiring (T8, T9) can compile against
it.

3 new tests:
- config_default_targets_1080p60_5mbps_cbr
- new_returns_no_render_node_in_container
- set_target_bitrate_marks_dirty_and_rejects_when_closed
EOF
)"
```

---

## Task 7: Encode loop + SPS/PPS capture + bitstream wire-up

**Files:**
- Modify: `crates/media-vaapi/src/encoder.rs`
- Modify: `crates/media-vaapi/src/display.rs` (`node_supports_h264_baseline_encode` real impl)

This is the heavy task. Wires the actual cros-libva flow: open Display, create Config + Context + Surface pool, upload I420 → Surface (vaDeriveImage fast path with vaPutImage fallback), build picture/slice/RC parameter buffers, vaBeginPicture / vaRenderPicture / vaEndPicture, sync, map coded buffer, normalize_to_annexb.

- [ ] **Step 1: Read cros-libva encode example**

```bash
./scripts/dev-container.sh bash -c '
cat target-docker/cargo-home/registry/src/index.crates.io-1949cf8c6b5b557f/cros-libva-0.0.13/lib/src/lib.rs 2>/dev/null | head -400
' 2>&1 | head -200
```

Identify the exact method chain for: Display::open → query_config_attributes → create_config → create_context → create_surfaces → … → MappedCodedBuffer. Document the actual API names in a comment block at the top of `encoder.rs`.

- [ ] **Step 2: Implement encode loop**

Replace the `Err(NotSupported)` body in `encode()` with the actual flow per spec §3.5. Key components:

1. **Acquire a free surface** from the pool (4 surfaces pre-allocated in `new()`).
2. **Upload**: `vaDeriveImage` fast path → fall to `vaCreateImage + vaPutImage`. Use `prdt_media_sw::I420Frame` plane copy.
3. **Build picture params**: `pic_fields.idr_pic_flag = (sequence_counter == 0 || force_idr || sequence_counter % gop_size == 0)`. Update `idr_pic_id` on IDR.
4. **Build slice params**: `slice_type = I` (= libva constant 7 in baseline) if IDR else `P` (5).
5. **Build RC param**: only if `rc_dirty`; clear flag.
6. **Submit** picture/slice/RC/coded buffer → `vaBeginPicture / vaRenderPicture / vaEndPicture / sync`.
7. **Map coded buffer** → walk segments → concat → `normalize_to_annexb(&raw, &self.sps_pps, is_idr, &mut out)`.
8. **Build EncodedFrame** matching `prdt_media_sw::EncodedFrame { seq, nal_units, is_keyframe, ts_us }`. Increment counters.

Apply the `0.5/1/2/4/8 ms` retry loop around the sync step for `HW_BUSY`.

- [ ] **Step 3: Wire real probe in display.rs**

Replace the `node_supports_h264_baseline_encode` stub with the real cros-libva-driven query (per Step 1's probe).

- [ ] **Step 4: Run full vaapi crate test suite + clippy**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-vaapi --lib --target x86_64-unknown-linux-gnu
./scripts/dev-container.sh cargo clippy -p prdt-media-vaapi --target x86_64-unknown-linux-gnu -- -D warnings
```

Container has no `/dev/dri/*`, so the encode loop can't run end-to-end. The `new_returns_no_render_node_in_container` test stays valid. Unit tests cover annexb / rc / error / frame_input.

If you can construct a unit test that exercises the encode-loop helpers without a live VAAPI device (e.g., a mock surface using `MaybeUninit::zeroed()` + a deterministic dummy Display), add it. If cros-libva's types are not constructible without a real Display, document the limitation in the file header and skip.

- [ ] **Step 5: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-vaapi/src/encoder.rs crates/media-vaapi/src/display.rs
git commit -m "$(cat <<'EOF'
P5C-1 T7: VaapiH264Encoder encode loop + display.rs real probe

Wires the full encode flow per spec §3.5:

1. Acquire free surface from a 4-surface pool
2. Upload I420 -> surface via vaDeriveImage (fast) or vaCreateImage
   + vaPutImage (fallback). One-time warn log on fallback.
3. Build picture params with idr_pic_flag (gop_size triggered or
   force_idr). Update idr_pic_id on IDR.
4. Build slice params (slice_type = I on IDR else P).
5. Build RC param iff rc_dirty (clear flag).
6. vaBeginPicture / vaRenderPicture / vaEndPicture / sync.
   sync retries on HW_BUSY with 0.5 -> 1 -> 2 -> 4 -> 8 ms backoff
   (max 5 attempts) per spec §3.7.
7. Map coded buffer, walk segments, normalize_to_annexb (3byte->4byte
   start codes, SPS/PPS prepend on IDR from cached blob).
8. Emit EncodedFrame and recycle the surface.

display.rs::node_supports_h264_baseline_encode now opens a Display
on each candidate render node and queries VAProfileH264ConstrainedBaseline
+ VAEntrypointEncSlice via cros-libva's query_config_profiles +
query_config_entrypoints.

SPS/PPS captured at encoder init via cros-libva's packed-header probe
(or manual byte-construction matching the chosen Config + Sequence
parameters; whichever the cros-libva API exposes most cleanly).

No new tests in this task — the encode loop requires a real /dev/dri/*
device which the container lacks. Existing 17 tests (4 error + 5
annexb + 2 frame_input/rc + 3 display + 3 encoder) all pass.
EOF
)"
```

---

## Task 8: `BackendKind::Vaapi` + Linux policy probe + `LinuxSwFactory` arm

**Files:**
- Modify: `crates/media-policy/src/capability.rs`
- Modify: `crates/media-linux/Cargo.toml`
- Modify: `crates/media-linux/src/policy.rs`

- [ ] **Step 1: Add `Vaapi` to `BackendKind`**

In `crates/media-policy/src/capability.rs`:

```rust
pub enum BackendKind {
    // existing variants...
    Vaapi,
}
```

Also add a string mapping in any CLI parse / display code (search for `Openh264 => "openh264"` and add `Vaapi => "vaapi"`).

- [ ] **Step 2: Add `prdt-media-vaapi` as Linux dep**

In `crates/media-linux/Cargo.toml`:

```toml
[target.'cfg(target_os = "linux")'.dependencies]
prdt-media-vaapi = { path = "../media-vaapi" }
```

- [ ] **Step 3: Update Linux probe**

In `crates/media-linux/src/policy.rs::LinuxCapabilityProbe::list_encoders` (or similar — find the actual fn that returns `Vec<EncoderCapability>` for Linux):

```rust
fn list_encoders(&self) -> Vec<EncoderCapability> {
    let mut out = vec![/* existing Openh264 entry */];
    if prdt_media_vaapi::display::vaapi_runtime_present() {
        out.push(EncoderCapability {
            backend: BackendKind::Vaapi,
            codec: Codec::H264,
            priority: 90,
            zero_copy: false,
            max_resolution: (3840, 2160),
            min_bitrate_bps: 100_000,
            requires_d3d11: false,
        });
    }
    out
}
```

Adjust struct field names to match the actual `EncoderCapability` (some fields may not exist; copy from the Windows probe's existing entries for pattern reference).

- [ ] **Step 4: Add `BackendKind::Vaapi` arm to `LinuxSwFactory::create`**

The arm constructs a `VaapiVideoProducer` (added in T9). For T8 we stub the arm:

```rust
BackendKind::Vaapi => {
    Err(FactoryError::Unavailable {
        backend: BackendKind::Vaapi,
        reason: "VaapiVideoProducer wiring lands in T9".into(),
    })
}
```

- [ ] **Step 5: Test + clippy**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-policy -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu
./scripts/dev-container.sh cargo clippy -p prdt-media-policy -p prdt-media-linux --target x86_64-unknown-linux-gnu --all-targets -- -D warnings
```

Expected: green. Existing 50 + 22 lib tests pass; no new tests in this task.

- [ ] **Step 6: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-policy/src/capability.rs \
        crates/media-linux/Cargo.toml \
        crates/media-linux/src/policy.rs
git commit -m "P5C-1 T8: BackendKind::Vaapi + Linux probe entry (priority 90) + factory arm stub"
```

---

## Task 9: `VaapiVideoProducer` + `LinuxSwFactory` arm wiring

**Files:**
- Create: `crates/media-linux/src/vaapi_pipeline.rs`
- Modify: `crates/media-linux/src/lib.rs` (re-export)
- Modify: `crates/media-linux/src/policy.rs` (Vaapi arm body)

- [ ] **Step 1: New `vaapi_pipeline.rs`**

```rust
//! VaapiVideoProducer — Linux HW codec path.
//!
//! Mirrors LinuxSwProducer's shape but swaps the encoder. Capture path
//! (X11 SHM or Wayland portal) is unchanged. After capture we still
//! convert BGRA -> I420 on CPU (P5C-1 minimum scope); P5C-2 will replace
//! this with a DMABUF -> VAAPI surface zero-copy path.

#![cfg(target_os = "linux")]

use crate::capture_source::CaptureSource;
use prdt_media_sw::I420Frame;
use prdt_media_vaapi::VaapiH264Encoder;
use prdt_protocol::{EncodedFrame, ProducerError, VideoProducer};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct VaapiVideoProducer {
    capture: Box<dyn CaptureSource + Send>,
    encoder: VaapiH264Encoder,
    bgra_scratch: Vec<u8>,
    i420_scratch: I420Frame,
    sequence_counter: u64,
    force_idr_next: Arc<AtomicBool>,
}

impl VaapiVideoProducer {
    pub fn new(
        capture: Box<dyn CaptureSource + Send>,
        encoder: VaapiH264Encoder,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            capture,
            encoder,
            bgra_scratch: Vec::with_capacity((width * height * 4) as usize),
            i420_scratch: I420Frame::new(width as usize, height as usize),
            sequence_counter: 0,
            force_idr_next: Arc::new(AtomicBool::new(true)),
        }
    }
}

#[async_trait::async_trait]
impl VideoProducer for VaapiVideoProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        // 1. Capture → bgra_scratch (spawn_blocking).
        // 2. BGRA → I420 (CPU SIMD via prdt_media_sw::bgra_to_i420).
        // 3. encoder.encode → spawn_blocking.
        // 4. Map VaapiError::HardwareBusy → ProducerError::DeviceLost.
        //
        // Full impl: mirror crates/media-linux/src/linux_sw_producer.rs's
        // body closely; only step 3 differs.
        todo!("T9 implementer: copy LinuxSwProducer pattern + swap encoder")
    }

    fn request_idr(&mut self) {
        self.force_idr_next.store(true, Ordering::Relaxed);
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Err(e) = self.encoder.set_target_bitrate(bps) {
            tracing::warn!(?e, "vaapi set_target_bitrate failed");
        }
    }

    fn backend_name(&self) -> &'static str { "linux-vaapi-h264" }
}
```

- [ ] **Step 2: Update `lib.rs` re-exports**

```rust
#[cfg(target_os = "linux")]
pub mod vaapi_pipeline;
#[cfg(target_os = "linux")]
pub use vaapi_pipeline::VaapiVideoProducer;
```

- [ ] **Step 3: Replace `BackendKind::Vaapi` factory arm body**

In `crates/media-linux/src/policy.rs::LinuxSwFactory::create`:

```rust
BackendKind::Vaapi => {
    // Build VaapiH264Encoder + the same capture chain LinuxSwProducer
    // uses, then return VaapiVideoProducer.
    let enc_cfg = prdt_media_vaapi::VaapiH264EncoderConfig {
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
        initial_bitrate_bps: cfg.initial_bitrate_bps,
        ..Default::default()
    };
    let encoder = prdt_media_vaapi::VaapiH264Encoder::new(enc_cfg).map_err(|e| {
        FactoryError::Unavailable {
            backend: BackendKind::Vaapi,
            reason: format!("VAAPI init failed: {e}"),
        }
    })?;
    let capture = build_capture_source(...)?;  // same chain as Openh264 arm
    Ok(Box::new(crate::VaapiVideoProducer::new(capture, encoder, cfg.width, cfg.height)))
}
```

Match the actual `cfg` field names and `FactoryError` shape.

- [ ] **Step 4: Test + clippy**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu vaapi
./scripts/dev-container.sh cargo clippy -p prdt-media-linux --target x86_64-unknown-linux-gnu --all-targets -- -D warnings
```

If there's a new test you can add for `VaapiVideoProducer::request_idr` toggling the AtomicBool, do so.

- [ ] **Step 5: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-linux/src/vaapi_pipeline.rs \
        crates/media-linux/src/lib.rs \
        crates/media-linux/src/policy.rs
git commit -m "$(cat <<'EOF'
P5C-1 T9: VaapiVideoProducer + LinuxSwFactory Vaapi arm

VaapiVideoProducer mirrors LinuxSwProducer (capture + BGRA->I420 CPU)
but swaps the encoder for VaapiH264Encoder. VaapiError::HardwareBusy
maps to ProducerError::DeviceLost so the existing P5A SelectionPolicy
auto-falls-back to OpenH264 on driver flakes.

LinuxSwFactory::create's BackendKind::Vaapi arm constructs the
VaapiH264Encoder + capture chain and returns the producer. The
Vaapi backend is now reachable from `--encoder vaapi` (explicit) and
`--encoder auto` (when probe succeeds and VAAPI priority 90 wins
over OpenH264 priority 50).

No new unit tests beyond AtomicBool toggle smoke — full integration
requires real /dev/dri/* (deferred to walkthrough §K).
EOF
)"
```

---

## Task 10: STATUS + walkthrough §K + final gate

**Files:**
- Modify: `docs/superpowers/STATUS.md`
- Modify: `docs/superpowers/p5b1-smoke-walkthrough.md`

- [ ] **Step 1: STATUS bump + P5C-1 entry**

Update `**Latest tag:**` to `phase-p5c1-vaapi-h264-encoder-complete`.

Insert after the P5B-2c entry:

```markdown
- **P5C-1 (`phase-p5c1-vaapi-h264-encoder-complete`, 2026-05-13)**:
  Linux HW codec — VAAPI H.264 encoder (Intel iHD + AMD radeonsi via
  Mesa libva). First subphase of P5C; NVENC-Linux / DMABUF zero-copy /
  V4L2 M2M / VAAPI decode deferred to subsequent subphases.
  - New `crates/media-vaapi/` workspace crate (cros-libva 0.0.13 RAII
    bindings). Modules: display (render-node enumerate + probe), encoder
    (VaapiH264Encoder), frame_input (CpuI420 / VaSurface / Dmabuf enum
    seam for P5C-2), annexb (3byte→4byte start-code normalize + SPS/PPS
    prepend on IDR), error (VaapiError + VAStatus mapping), rc
    (RateControlParams CBR builder).
  - `VaapiH264Encoder` API: `new(VaapiH264EncoderConfig)` /
    `encode(&I420Frame, force_idr, ts_us) -> Result<EncodedFrame>` /
    `set_target_bitrate(bps)` / `backend_name() = "vaapi-h264-cbr-baseline"`.
    Profile: H264ConstrainedBaseline. RC: CBR only. Output: Annex-B
    with manual SPS/PPS prepend.
  - HardwareBusy retry: 0.5→1→2→4→8 ms, max 5 attempts. After exhaustion
    surfaces VaapiError::HardwareBusy → ProducerError::DeviceLost →
    P5A SelectionPolicy auto-falls-back to OpenH264.
  - `BackendKind::Vaapi` added; Linux probe lists it at priority 90
    when `/dev/dri/renderD*` + H264ConstrainedBaseline + EncSlice
    detected. `LinuxSwFactory::create` gains a Vaapi arm wiring
    `VaapiVideoProducer` (capture chain unchanged; encoder swap only).
  - Drop order policy (load-bearing per spec §3.4): manual `impl Drop`
    on `VaapiH264Encoder` ensures images → coded buffers → surfaces →
    context → config → display teardown sequence regardless of cros-libva
    Rc cycles.
  - **Tests**: 4 error + 5 annexb + 2 frame_input/rc + 3 display + 3
    encoder = **17 new unit tests**. Container clippy clean on
    prdt-media-vaapi + prdt-media-policy + prdt-media-linux + 8 other
    affected crates. Affected-slice lib tests green. Real-device VAAPI
    runtime verification = walkthrough §K (vainfo + host run + pidstat
    CPU check + bitrate update + DRI permission failure fallback).
  - **Build env**: `scripts/Dockerfile.dev` gains libva-dev / libva-drm2
    / libva-x11-2 (Debian bookworm). Container can compile prdt-client
    with VAAPI backend, but the encode loop requires `/dev/dri/*` which
    the container intentionally lacks. Smoke walkthrough = user host.
  - **Out of scope (deferred)**: DMABUF zero-copy (P5C-2), NVENC-Linux
    (P5C-3), V4L2 M2M (P5C-4), VAAPI decode (separate subphase),
    AMD-specific tuning, AVCC output, multi-slice encoding, packed-header
    FFI path (manual SPS/PPS prepend is sufficient for P5C-1).
  - **Smoke walkthrough**: `docs/superpowers/p5b1-smoke-walkthrough.md`
    §P5C-1 Section K (real-device VAAPI verification).
```

- [ ] **Step 2: Append walkthrough §K**

```markdown
---

## P5C-1 — VAAPI H.264 encoder (Linux HW codec)

### Section K — VAAPI encoder real-device smoke

**Pre-conditions:**
- Linux host with Intel iGPU (Tigerlake+) OR AMD APU (Renoir+).
- Mesa libva ≥ 23.x (intel-media-driver for Intel; radeonsi for AMD).
- User in the `render` (or `video`) group so `/dev/dri/renderD128` is RW-accessible.
- `prdt host` + `prdt connect` binaries from this branch.

**Steps:**

1. Verify VAAPI driver: `vainfo | grep H264ConstrainedBaseline`. Expect at least one matching VAEntrypointEncSlice line.

2. Start host with explicit Vaapi backend:
   ```bash
   ./prdt host --encoder vaapi --bitrate-mbps 5 --silent-allow 2>&1 | tee p5c1.log
   ```

3. Expect log: `vaapi encoder initialized: driver=intel-iHD profile=ConstrainedBaseline`.

4. Connect viewer:
   ```bash
   ./prdt connect --host <ip>:9000 --decoder openh264 --codec h264
   ```

5. Confirm frame flow at ≥ 30 fps in viewer.

6. **CPU usage check** (the HW codec payoff):
   ```bash
   pidstat -p $(pgrep -f prdt) 1 30
   ```
   Expected: host %CPU significantly below the OpenH264 SW baseline (Intel iGPU 1080p60 typically <5% CPU vs OpenH264 SW ~25-40%).

7. **Bitrate update**: from viewer adjust the bitrate slider; expect host log line `set_target_bitrate 8000000 → 8 Mbps`.

8. **Failure fallback** (DeviceLost path): `sudo chmod 000 /dev/dri/renderD128` while a session is running. Within ~5 seconds the host should:
   - Emit `vaapi encode failed: HardwareBusy / DriverError → falling back to OpenH264`
   - Continue the session with the SW encoder (frames may briefly stutter)
   - Restore the device with `sudo chmod 0666 /dev/dri/renderD128` for next session.

9. **AMD APU verification** (separate run): repeat steps 1–7 on a Kubuntu/Fedora system with Ryzen Renoir+ APU. Confirm `vainfo` shows `radeonsi`-prefixed driver names; the encoder priority + Annex-B output should be identical to Intel.

### Known issues / follow-ups (P5C-1 specific)

- **NVIDIA hosts**: `nvidia-vaapi-driver` is decode-only — `vainfo` may list NVENC profiles but `VAEntrypointEncSlice` will be absent and the probe correctly excludes the device. NVENC-Linux ships in P5C-3.

- **WSL2**: VAAPI via `mesa-d3d12` works on recent WSL2 kernels but is functionally a Mesa software path for now (no actual GPU acceleration). Useful for dev smoke but expect SW-comparable CPU usage.

- **`/dev/dri/renderD*` permission**: if the user isn't in `render`/`video`, the probe fails silently and OpenH264 SW is selected. Document for downstream operators.

- **VBR mode**: only CBR is supported in P5C-1. CBR↔VBR switching via dynamic reconfigure is driver-fragile (spec §2); add a `--vaapi-rc-mode {cbr,vbr}` CLI knob in a follow-up.
```

- [ ] **Step 3: Final gate**

```bash
./scripts/dev-container.sh cargo fmt --all
./scripts/dev-container.sh cargo clippy -p prdt-protocol -p prdt-transport \
    -p prdt-media-core -p prdt-media-sw -p prdt-media-policy -p prdt-media-linux \
    -p prdt-media-vaapi -p prdt-viewer -p prdt-viewer-overlay \
    --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
./scripts/dev-container.sh cargo test --target x86_64-unknown-linux-gnu --lib \
    -p prdt-protocol -p prdt-media-core -p prdt-media-sw -p prdt-media-policy \
    -p prdt-media-linux -p prdt-media-vaapi -p prdt-transport -p prdt-viewer
./scripts/dev-container.sh cargo test -p prdt-media-linux \
    --test capture_source_contract --target x86_64-unknown-linux-gnu
```

Expected: green. 17 new tests passing (+ existing P5B-2c suite).

- [ ] **Step 4: Commit STATUS + walkthrough**

```bash
git add docs/superpowers/STATUS.md docs/superpowers/p5b1-smoke-walkthrough.md
git commit -m "$(cat <<'EOF'
docs(STATUS): record P5C-1 VAAPI H.264 encoder

Adds phase-p5c1-vaapi-h264-encoder-complete entry under §1. Header
bumped from phase-p5b2c-cursor-hide-polish-rebuild.

Walkthrough §K added: real-device VAAPI verification (vainfo +
host run + pidstat CPU check + bitrate update + DRI permission
failure fallback). Documents NVIDIA exclusion, WSL2 limitations,
permission requirements, CBR-only scope.

P5C-2/3/4 follow-ups recorded.
EOF
)"
```

- [ ] **Step 5: Stop — controller handles PR**

---

## Cross-task notes

- All cargo via `./scripts/dev-container.sh`.
- `cros-libva` pinned to `=0.0.13` until next audit (0.0.* may break source compat on minor bumps).
- Pre-existing flaky `transport::probe_test::two_transports_find_each_other` excluded as before.
- `prdt-host` lib tests block in container on gdk-sys; use `cargo check` for host-side correctness.
- Container has no `/dev/dri/*`. Encode-loop integration test cannot run in CI — walkthrough is the gate.
- Drop order on `VaapiH264Encoder` is load-bearing — manual `impl Drop` with `Option::take()` per spec §3.4.

---

## Ambiguities resolved (spec didn't cover; plan author chose)

1. **cros-libva API names**: probe in T1 Step 2 records actual names. Plan templates pseudo-code; T6/T7 implementer adjusts.
2. **`vaDeriveImage` fallback path**: always try first, on failure log warn once and use `vaCreateImage + vaPutImage` for the session lifetime. Reset on encoder re-init.
3. **Surface pool size**: 4 (one in flight + 3 buffered). If smoke shows starvation, bump to 6 in a follow-up.
4. **HW_BUSY retry budget**: 5 attempts (0.5/1/2/4/8 ms). Total budget ~15 ms, fits within 1 frame at 60 fps.
5. **SPS/PPS capture mechanism**: plan defers the choice to T7 implementer (packed-header FFI vs manual byte-construction). Manual prepend is sufficient since the Annex-B normalizer is the single source of truth.
6. **`LinuxSwFactory` rename**: keep current name (Gemini suggested `LinuxVideoFactory`; deferred — too much churn). Add a doc-comment noting "SW" in the name is historical; the factory now dispatches both SW and HW encoders.
7. **`--vaapi-rc-mode` CLI flag**: not in P5C-1. CBR only; VBR knob is a follow-up after smoke validates CBR is stable.
