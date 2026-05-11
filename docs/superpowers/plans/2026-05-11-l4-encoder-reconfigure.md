# L4 Live Encoder Reconfigure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the no-op encoder bitrate-change stubs in OpenH264 (`media-sw`) and NVENC (`media-win`) with live reconfigure API calls so the L3 viewer-side AIMD controller has actual production effect, plus add the host-side `bytes_sent_window` log so the smoke procedure can verify "encoder actually shrunk frames."

**Architecture:** Three encoder backends sit behind the existing `VideoProducer::set_target_bitrate(bps)` trait — L3 already plumbs viewer→host SetBitrate end-to-end. L4 only changes encoder body implementations. OpenH264 calls `unsafe encoder.raw_api().set_option(ENCODER_OPTION_BITRATE, &mut SBitrateInfo)`. NVENC mutates the owned `Box<NV_ENC_CONFIG>` in place, then calls `nvEncReconfigureEncoder` with `forceIDR=1` + `resetEncoder=0`. MF stays as a `warn!+return` stub deferred to L5. Smoke verification uses `tc qdisc netem loss 15% delay 50ms±20ms` past the FEC threshold.

**Tech Stack:** Rust 1.85, openh264 0.9.3, openh264-sys2 0.9.6, NVIDIA Video Codec SDK (bindgen via `prdt_nvenc_bindings` cfg), tokio mpsc (existing L3), Linux `tc qdisc netem`.

---

## Pre-Task Context (T0 partially resolved)

The brainstorming + codex review pre-resolved several spec §6 open questions by reading the existing code:

| # | Question | Resolution |
|---|---|---|
| Q1 | `_init_params` field on NvencEncoder | **Confirmed exists** at `crates/media-win/src/nvenc/encoder.rs:73`. Comment says "Keep init params alive so the encodeConfig pointer inside params remains valid for the life of the session (NVENC does not copy it)." Just rename (drop `_` prefix) in T2. |
| Q4 | InitParams shape | `pub struct InitParams { params: NV_ENC_INITIALIZE_PARAMS, config: Box<NV_ENC_CONFIG> }` at `crates/media-win/src/nvenc/config.rs:83`. T2 adds `encode_config_mut`, `as_ffi`, `fps_numerator`, `fps_denominator` accessors. |
| Q6 | VBV formula at init | `vbvBufferSize = bps / fps_num.max(1)`, `vbvInitialDelay = vbvBufferSize` (inline at `config.rs:105-106`). T2 extracts to `pub(crate) fn vbv_buffer_size_for(bps, fps)` and `pub(crate) fn vbv_initial_delay_for(bps, fps)` and updates the init site to call them. |
| Q2 | `nvEncReconfigureEncoder` bindgen visibility | Open — verify in T0 by inspecting `OUT_DIR/nvenc_bindings.rs` after a Windows build, or `cargo expand`. The function pointer should be a field of `NV_ENCODE_API_FUNCTION_LIST` (allowlisted via `allowlist_type("NV_ENC.*")`). |
| Q3 | `NV_ENC_RECONFIGURE_PARAMS_VER` value | Open — read from bindgen output; if absent as a const, port the macro using `nv_enc_pic_params_ver` precedent in `crates/media-win/src/nvenc/config.rs`. |
| Q5 | openh264-sys2 import path | Open — verify `openh264_sys2::ENCODER_OPTION_BITRATE`, `openh264_sys2::SBitrateInfo`, `openh264_sys2::SPATIAL_LAYER_ALL` resolve. May need `openh264_sys2::generated::types::*` if not at crate root. |

**Key file references** (read-only context for implementers):

- `crates/media-win/src/nvenc/encoder.rs:65-76` — NvencEncoder struct (with `_init_params: InitParams`)
- `crates/media-win/src/nvenc/encoder.rs:328-340` — current no-op `set_target_bitrate` to replace
- `crates/media-win/src/nvenc/config.rs:83-130` — InitParams + VBV inline formula
- `crates/media-win/src/encoder_trait.rs:73-78` — HwHevcEncoder dispatch (no change needed)
- `crates/media-sw/src/encoder.rs:127-135` — current stash-only `set_target_bitrate` to replace
- `crates/media-sw/Cargo.toml` — needs `openh264-sys2 = "0.9.6"` added
- `crates/host/src/lib.rs:498-535` — host video task (T0 adds bytes_sent_window log here)
- `~/.cargo/registry/src/index.crates.io-*/openh264-0.9.3/src/encoder.rs:1062` — `pub const unsafe fn raw_api(&mut self) -> &mut EncoderRawAPI`
- `~/.cargo/registry/src/index.crates.io-*/openh264-sys2-0.9.6/src/generated/types.rs` — ENCODER_OPTION_BITRATE, SBitrateInfo

---

## Branch & Working Dir

Branch `phase-l4-encoder-reconfigure` already exists and is checked out. Spec is committed at `docs/superpowers/specs/2026-05-11-l4-encoder-reconfigure-design.md` (commit `e99c883`).

```bash
git status   # → "On branch phase-l4-encoder-reconfigure", clean
git log --oneline -3   # → "e99c883 L4 spec: address codex review ...", "40e03ec L4 spec: live encoder reconfigure ...", "fbc031a L3: adaptive bitrate ..."
```

---

## File Manifest

| Path | Status | Purpose |
|---|---|---|
| `crates/host/src/lib.rs` | modify | Add `bytes_sent_window: u64` accumulator to host video task; emit in `host tx stats` info! line (codex HIGH #2) |
| `crates/media-sw/Cargo.toml` | modify | Add `openh264-sys2 = "0.9.6"` direct dependency |
| `crates/media-sw/src/encoder.rs` | modify | Replace no-op `set_target_bitrate` body with `raw_api().set_option(ENCODER_OPTION_BITRATE, ...)`; add 1 unit test using xorshift noise input |
| `crates/media-win/src/nvenc/config.rs` | modify | Drop `_` from field, add `InitParams::encode_config_mut/as_ffi/fps_numerator/fps_denominator` accessors, extract VBV helpers `vbv_buffer_size_for/vbv_initial_delay_for`, add `nv_enc_reconfigure_params_ver` |
| `crates/media-win/src/nvenc/encoder.rs` | modify | Rename field `_init_params → init_params`, replace no-op `set_target_bitrate` body with `nvEncReconfigureEncoder` call, add gated `#[ignore]` integration test using xorshift noise input |
| `scripts/l4-netem-smoke.sh` | **new** | `tc qdisc add/del/status` helper with `loss 15% delay 50ms±20ms` defaults |
| `docs/superpowers/STATUS.md` | modify | Add L4 entry under B2 + smoke walkthrough record + update header tag |

---

## Task 1: Host `bytes_sent_window` log + open-question verification

**Files:**
- Modify: `crates/host/src/lib.rs` (host video task, around lines 498-535)

**Why this comes first:** The smoke procedure (T6) cannot verify "encoder actually shrunk frames" without this log. T0 also needs to verify Q2/Q3/Q5 before T1/T3 dispatch. Bundle them.

- [ ] **Step 1: Verify Q2 — `nvEncReconfigureEncoder` in bindgen output**

This crate only builds on Windows, but the bindgen output should be inspectable from the build cache. From Linux, just confirm the build.rs allowlist is permissive enough. Run:

```bash
grep -n "allowlist_type\|allowlist_function\|nvEncReconfigure" /home/ubuntu/project/power-remote-dt/crates/media-win/build.rs
```

Expected: the allowlist includes `allowlist_type("NV_ENC.*")` (which pulls in the function pointer field) but does NOT include an explicit `allowlist_function("nvEncReconfigure.*")`. That's fine — the function pointer is a struct field, not a free function. T3 will exercise it via `self.fn_table.nvEncReconfigureEncoder` which is `Option<unsafe extern "C" fn(...)>`.

If you have access to a Windows build cache or `cargo expand`, additionally verify by inspecting `target/x86_64-pc-windows-msvc/.../out/nvenc_bindings.rs`. If unavailable on this host, defer the runtime check to Windows CI in T5.

Record: **Q2 outcome:** "Function pointer pulled in via `allowlist_type("NV_ENC.*")`; runtime existence confirmed by Windows CI in T5."

- [ ] **Step 2: Verify Q3 — `NV_ENC_RECONFIGURE_PARAMS_VER` value**

From the NVIDIA SDK header convention, the version macro is typically:

```c
#define NV_ENC_RECONFIGURE_PARAMS_VER (NVENCAPI_STRUCT_VERSION(1) | (1<<31))
```

Compare to existing helpers in `crates/media-win/src/nvenc/config.rs`:

```bash
grep -n "fn nv_enc_.*_ver\b\|nvenc_struct_version\|STRUCT_VERSION" /home/ubuntu/project/power-remote-dt/crates/media-win/src/nvenc/config.rs | head -10
```

Confirm there is a `pub(crate) fn nvenc_struct_version(struct_id: u32) -> u32` helper (or similar). If yes, the L4 helper is:

```rust
pub(crate) const fn nv_enc_reconfigure_params_ver() -> u32 {
    nvenc_struct_version(1) | (1 << 31)
}
```

The `(1 << 31)` is the SDK 13 convention requiring the high bit set on `NV_ENC_RECONFIGURE_PARAMS_VER` and `NV_ENC_INITIALIZE_PARAMS_VER` (per nvEncodeAPI.h comments). T2 will add the helper and T3 will use it. Record: **Q3 outcome:** value is `nvenc_struct_version(1) | (1 << 31)`; verify via bindgen on Windows or by reading the SDK header.

- [ ] **Step 3: Verify Q5 — openh264-sys2 import paths**

Inspect the openh264-sys2 generated types:

```bash
grep -n "ENCODER_OPTION_BITRATE\|SBitrateInfo\|SPATIAL_LAYER_ALL" \
  ~/.cargo/registry/src/index.crates.io-*/openh264-sys2-0.9.6/src/generated/types.rs | head -10
```

Expected: all three present. They will be re-exported at the crate root (`openh264_sys2::ENCODER_OPTION_BITRATE` etc.) via the crate's `lib.rs` glob `pub use generated::types::*;`. If not, the explicit path is `openh264_sys2::generated::types::ENCODER_OPTION_BITRATE`.

Record: **Q5 outcome:** importable as `openh264_sys2::ENCODER_OPTION_BITRATE`, `openh264_sys2::SBitrateInfo`, `openh264_sys2::SPATIAL_LAYER_ALL` (likely; T1 falls back to `generated::types::*` path if needed).

- [ ] **Step 4: Read host video task at `crates/host/src/lib.rs:492-545`**

```bash
sed -n '492,545p' /home/ubuntu/project/power-remote-dt/crates/host/src/lib.rs
```

Confirm the structure matches the snippet in this plan's Pre-Task Context.

- [ ] **Step 5: Add `bytes_sent_window` accumulator to host video task**

Edit `crates/host/src/lib.rs`. Find the video task body around line 497-535. Replace the entire `let video = tokio::spawn(async move { ... });` block (or, more safely, use targeted Edit tool calls) so the body looks like the version below. The diff is:

1. Add `let mut bytes_sent_window: u64 = 0;` after `let mut last_log = std::time::Instant::now();`
2. In the `Ok(frame)` arm, compute `let bytes_in_frame: u64 = frame.nal_units.iter().map(|n| n.len() as u64).sum();` BEFORE `tx_video.send_video(frame).await` (the frame moves into send_video)
3. In the success branch (`else { frames_sent += 1; }`), add `bytes_sent_window += bytes_in_frame;`
4. In the 1-second log block, change `info!(frames_sent, send_errors, "host tx stats");` to `info!(frames_sent, send_errors, bytes_sent_window, "host tx stats");` and add `bytes_sent_window = 0;` after `last_log = std::time::Instant::now();`

Use the Edit tool with this exact `old_string`:

```rust
        let video = tokio::spawn(async move {
            let mut frames_sent = 0u64;
            let mut send_errors = 0u64;
            let mut last_log = std::time::Instant::now();
            let mut first_frame_logged = false;
            loop {
                tokio::select! {
                    _ = cancel_video.cancelled() => break,
                    _ = async {
                        // L3: drain bitrate channel to newest, apply to encoder.
                        let mut latest_bps: Option<u32> = None;
                        while let Ok(bps) = bitrate_rx.try_recv() {
                            latest_bps = Some(bps);
                        }
                        if let Some(bps) = latest_bps {
                            producer.set_target_bitrate(bps);
                            debug!(target_bps = bps, "applied viewer-requested bitrate");
                        }
                        if video_force_idr.swap(false, Ordering::AcqRel) {
                            producer.request_idr();
                            info!("viewer requested IDR; producer.request_idr() called");
                        }
                        match producer.next_frame().await {
                            Ok(frame) => {
                                if !first_frame_logged {
                                    let elapsed_ms = handshake_complete_at.elapsed().as_millis();
                                    info!(elapsed_ms = elapsed_ms as u64, "first frame ready");
                                    first_frame_logged = true;
                                }
                                let nal_len = frame.nal_units.len();
                                let is_kf = frame.is_keyframe;
                                if let Err(e) = tx_video.send_video(frame).await {
                                    send_errors += 1;
                                    warn!(?e, nal_len, is_kf, "send_video error; continuing");
                                } else {
                                    frames_sent += 1;
                                }
                                if last_log.elapsed() >= std::time::Duration::from_secs(1) {
                                    info!(frames_sent, send_errors, "host tx stats");
```

and replace with:

```rust
        let video = tokio::spawn(async move {
            let mut frames_sent = 0u64;
            let mut send_errors = 0u64;
            // L4: 1-second window byte counter so smoke can verify
            // "encoder actually shrunk frames" alongside L3's target_bps log.
            let mut bytes_sent_window: u64 = 0;
            let mut last_log = std::time::Instant::now();
            let mut first_frame_logged = false;
            loop {
                tokio::select! {
                    _ = cancel_video.cancelled() => break,
                    _ = async {
                        // L3: drain bitrate channel to newest, apply to encoder.
                        let mut latest_bps: Option<u32> = None;
                        while let Ok(bps) = bitrate_rx.try_recv() {
                            latest_bps = Some(bps);
                        }
                        if let Some(bps) = latest_bps {
                            producer.set_target_bitrate(bps);
                            debug!(target_bps = bps, "applied viewer-requested bitrate");
                        }
                        if video_force_idr.swap(false, Ordering::AcqRel) {
                            producer.request_idr();
                            info!("viewer requested IDR; producer.request_idr() called");
                        }
                        match producer.next_frame().await {
                            Ok(frame) => {
                                if !first_frame_logged {
                                    let elapsed_ms = handshake_complete_at.elapsed().as_millis();
                                    info!(elapsed_ms = elapsed_ms as u64, "first frame ready");
                                    first_frame_logged = true;
                                }
                                let nal_len = frame.nal_units.len();
                                let is_kf = frame.is_keyframe;
                                let bytes_in_frame: u64 =
                                    frame.nal_units.iter().map(|n| n.len() as u64).sum();
                                if let Err(e) = tx_video.send_video(frame).await {
                                    send_errors += 1;
                                    warn!(?e, nal_len, is_kf, "send_video error; continuing");
                                } else {
                                    frames_sent += 1;
                                    bytes_sent_window += bytes_in_frame;
                                }
                                if last_log.elapsed() >= std::time::Duration::from_secs(1) {
                                    info!(frames_sent, send_errors, bytes_sent_window, "host tx stats");
                                    bytes_sent_window = 0;
```

Note: only the changed-region `info!(...)` line is shown in the new_string. The rest of the loop (the `last_log = std::time::Instant::now();` reset and the `Err(e)` arm and the closing braces) stays unchanged after the matching point.

- [ ] **Step 6: Build host (Linux)**

```bash
cargo build -p prdt-host --target x86_64-unknown-linux-gnu 2>&1 | tail -5
```

Expected: clean build (no new warnings).

- [ ] **Step 7: Run host tests + clippy**

```bash
cargo test -p prdt-host --target x86_64-unknown-linux-gnu 2>&1 | tail -3
cargo clippy -p prdt-host --target x86_64-unknown-linux-gnu --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: tests pass, no warnings.

- [ ] **Step 8: cargo fmt + commit**

```bash
cargo fmt --all
git add crates/host/src/lib.rs
git commit -m "$(cat <<'EOF'
L4 T0: host video task bytes_sent_window log

Adds a u64 byte accumulator to the host video task that gets emitted in
the existing "host tx stats" info! line every second. Resets after each
emission. Codex review HIGH #2: the L4 smoke procedure DoD references
"host tx stats bytes_sent_window" as the ground-truth signal that
encoder reconfigure actually shrinks frames, but the log field did not
exist before this commit. Without it, smoke would only verify the
viewer-side controller (already proven in L3) and not the L4 encoder
side.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: NVENC InitParams accessors + VBV helpers + version helper

**Files:**
- Modify: `crates/media-win/src/nvenc/config.rs` — accessors + VBV helpers + reconfigure version
- Modify: `crates/media-win/src/nvenc/encoder.rs` — rename `_init_params` → `init_params`

**Why this comes before T3:** T3 needs `init_params.encode_config_mut()`, `init_params.as_ffi()`, `init_params.fps_numerator()`, `init_params.fps_denominator()`, `vbv_buffer_size_for(bps, fps)`, `vbv_initial_delay_for(bps, fps)`, and `nv_enc_reconfigure_params_ver()`. None of these exist today.

- [ ] **Step 1: Read current InitParams definition**

```bash
sed -n '83,135p' /home/ubuntu/project/power-remote-dt/crates/media-win/src/nvenc/config.rs
```

Confirm shape:
```rust
pub struct InitParams {
    pub(crate) params: ffi::NV_ENC_INITIALIZE_PARAMS,
    pub(crate) config: Box<ffi::NV_ENC_CONFIG>,
}
```

(Field visibility may be `pub(crate)` or private — adjust accessors accordingly.)

- [ ] **Step 2: Add accessors to `impl InitParams`**

Find the `impl InitParams { ... }` block at `crates/media-win/src/nvenc/config.rs:89`. Inside the impl block, after the existing constructor `pub(crate) fn new(...) -> Self`, append:

```rust
    /// L4: live access to the embedded NV_ENC_CONFIG so reconfigure can
    /// mutate rate-control params (averageBitRate, maxBitRate, vbvBufferSize,
    /// vbvInitialDelay) without copying the Box (which would invalidate
    /// the encodeConfig pointer NVENC holds).
    pub(crate) fn encode_config_mut(&mut self) -> &mut ffi::NV_ENC_CONFIG {
        &mut self.config
    }

    /// L4: by-reference view of the outer NV_ENC_INITIALIZE_PARAMS so the
    /// caller can copy the POD struct (whose encodeConfig pointer remains
    /// valid because self owns the underlying Box).
    pub(crate) fn as_ffi(&self) -> &ffi::NV_ENC_INITIALIZE_PARAMS {
        &self.params
    }

    pub(crate) fn fps_numerator(&self) -> u32 {
        self.params.frameRateNum
    }

    pub(crate) fn fps_denominator(&self) -> u32 {
        self.params.frameRateDen
    }
```

- [ ] **Step 3: Extract VBV helpers**

Append to the same file (top-level, near other `pub(crate) const fn nv_enc_*_ver()` helpers):

```rust
/// L4: VBV buffer size at the given bitrate and FPS. Mirrors the inline
/// formula at the call site in `InitParams::new` (1-frame buffer at target
/// bitrate for low-latency screen-share). Used by both init and L4
/// reconfigure so the values stay coupled.
pub(crate) const fn vbv_buffer_size_for(bps: u32, fps: u32) -> u32 {
    bps / if fps == 0 { 1 } else { fps }
}

/// L4: VBV initial delay matches buffer size for the same low-latency
/// reasoning.
pub(crate) const fn vbv_initial_delay_for(bps: u32, fps: u32) -> u32 {
    vbv_buffer_size_for(bps, fps)
}
```

- [ ] **Step 4: Replace inline VBV at `config.rs:105-106` with the helper**

Find this block (around line 100-107):

```rust
        config.rcParams.averageBitRate = cfg.bitrate_bps;
        config.rcParams.maxBitRate = cfg.bitrate_bps;
        // VBV buffer = 1 frame at target bitrate for low-latency.
        config.rcParams.vbvBufferSize = cfg.bitrate_bps / cfg.fps_numerator.max(1);
        config.rcParams.vbvInitialDelay = config.rcParams.vbvBufferSize;
```

Replace with:

```rust
        config.rcParams.averageBitRate = cfg.bitrate_bps;
        config.rcParams.maxBitRate = cfg.bitrate_bps;
        // VBV buffer = 1 frame at target bitrate for low-latency. Helper
        // shared with L4 reconfigure so init and reconfigure stay coupled.
        config.rcParams.vbvBufferSize =
            vbv_buffer_size_for(cfg.bitrate_bps, cfg.fps_numerator);
        config.rcParams.vbvInitialDelay =
            vbv_initial_delay_for(cfg.bitrate_bps, cfg.fps_numerator);
```

- [ ] **Step 5: Add `nv_enc_reconfigure_params_ver()` helper**

Find the existing `pub(crate) const fn nv_enc_initialize_params_ver()` (or equivalent). Right after it, append:

```rust
/// L4: NV_ENC_RECONFIGURE_PARAMS version. SDK 13 nvEncodeAPI.h:
/// `#define NV_ENC_RECONFIGURE_PARAMS_VER (NVENCAPI_STRUCT_VERSION(1) | (1<<31))`
/// The high bit is required for this struct (and a few others); see SDK
/// header comments. If `ffi::NV_ENC_RECONFIGURE_PARAMS_VER` is exposed by
/// bindgen as a const, prefer that — but as of SDK 12+ the macro form is
/// usually the only thing pulled in.
pub(crate) const fn nv_enc_reconfigure_params_ver() -> u32 {
    nvenc_struct_version(1) | (1 << 31)
}
```

If T0/Q3 found that `ffi::NV_ENC_RECONFIGURE_PARAMS_VER` IS exposed by bindgen as a const, replace the body with `ffi::NV_ENC_RECONFIGURE_PARAMS_VER` instead. Either way the function signature stays the same so T3 doesn't change.

- [ ] **Step 6: Rename `_init_params` → `init_params` on `NvencEncoder`**

Edit `crates/media-win/src/nvenc/encoder.rs`. Find the struct at line 65-76:

```rust
pub struct NvencEncoder {
    fn_table: ffi::NV_ENCODE_API_FUNCTION_LIST,
    session: *mut std::ffi::c_void,
    bitstream_buffer: ffi::NV_ENC_OUTPUT_PTR,
    #[allow(dead_code)]
    config: NvencEncoderConfig,
    /// Keep init params alive so the `encodeConfig` pointer inside `params`
    /// remains valid for the life of the session (NVENC does not copy it).
    _init_params: InitParams,
    _dev: D3d11Device,
}
```

Change to:

```rust
pub struct NvencEncoder {
    fn_table: ffi::NV_ENCODE_API_FUNCTION_LIST,
    session: *mut std::ffi::c_void,
    bitstream_buffer: ffi::NV_ENC_OUTPUT_PTR,
    #[allow(dead_code)]
    config: NvencEncoderConfig,
    /// Keep init params alive so the `encodeConfig` pointer inside `params`
    /// remains valid for the life of the session (NVENC does not copy it).
    /// L4 also mutates this in place via `set_target_bitrate` to call
    /// `nvEncReconfigureEncoder` without copying the Box.
    init_params: InitParams,
    _dev: D3d11Device,
}
```

Then find the constructor `impl NvencEncoder { pub fn new(...) -> Result<Self> { ... } }` and locate the `Self { ... _init_params: ..., _dev: ... }` initialization at the end. Rename `_init_params:` to `init_params:` there too.

There may not be other reads of `_init_params` (the underscore prefix marks it unused-by-design). If clippy complains about unused field after rename, it shouldn't because T3 will use it next. If T2 ships before T3 and clippy fires, add `#[allow(dead_code)]` temporarily on the field — but the cleaner path is to land T2 + T3 in the same PR before clippy runs in CI.

- [ ] **Step 7: Build (Linux check)**

The crate is gated `#[cfg(windows)]` so `cargo check` from Linux just verifies the file compiles syntactically:

```bash
cargo check -p prdt-media-win --target x86_64-unknown-linux-gnu 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 8: cargo fmt + commit**

```bash
cargo fmt --all
git add crates/media-win/src/nvenc/config.rs crates/media-win/src/nvenc/encoder.rs
git commit -m "$(cat <<'EOF'
L4 T2: NVENC InitParams accessors + VBV helpers + reconfigure version

Adds the building blocks T3 needs to call nvEncReconfigureEncoder:
- InitParams::encode_config_mut/as_ffi/fps_numerator/fps_denominator
- vbv_buffer_size_for(bps, fps) / vbv_initial_delay_for(bps, fps)
  helpers; init site at config.rs:105 now uses them too (codex MEDIUM #6
  — VBV must track bitrate or rate control becomes loose)
- nv_enc_reconfigure_params_ver() with the SDK 13 high-bit convention
  (codex MEDIUM #5)
- NvencEncoder._init_params renamed to init_params (codex LOW #7 —
  the field already existed; just removing the unused prefix since
  T3 will consume it)

No behavior change yet; T3 wires up the actual reconfigure call.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: NVENC `set_target_bitrate` body + gated integration test

**Files:**
- Modify: `crates/media-win/src/nvenc/encoder.rs:328-340` (replace no-op stub)
- Modify: `crates/media-win/src/nvenc/encoder.rs` (`#[cfg(test)] mod tests` — add 1 new gated test)

- [ ] **Step 1: Read current no-op stub**

```bash
sed -n '320,345p' /home/ubuntu/project/power-remote-dt/crates/media-win/src/nvenc/encoder.rs
```

Expected: `fn set_target_bitrate(&mut self, bps: u32) { tracing::warn!(...); }` body.

- [ ] **Step 2: Replace the no-op body**

Use Edit tool. Find this exact block (approximate line 328-340; adjust for any drift after T2's rename):

```rust
    fn set_target_bitrate(&mut self, bps: u32) {
        // The current NVENC implementation does not yet support live
        // bitrate reconfiguration; record the requested value for the
        // next session restart. This matches the existing behaviour:
        // bitrate is set in `NvencEncoderConfig::bitrate_bps` at
        // construction time.
        tracing::warn!(
            target = "nvenc",
            requested_bps = bps,
            "set_target_bitrate is currently a no-op for NVENC \
             (rate-control reconfiguration is a follow-up)"
        );
    }
```

Replace with:

```rust
    fn set_target_bitrate(&mut self, bps: u32) {
        // L4: live reconfigure via nvEncReconfigureEncoder. Mutates the
        // owned encode_config in place (the Box stays alive on self), then
        // copies the outer NV_ENC_INITIALIZE_PARAMS POD by value into the
        // reconfigure params. The encodeConfig pointer in that POD copy
        // refers to self's Box and remains valid for the duration of the
        // FFI call.
        let fps_num = self.init_params.fps_numerator();
        let fps_den = self.init_params.fps_denominator().max(1);
        let fps = (fps_num / fps_den).max(1);
        {
            let cfg = self.init_params.encode_config_mut();
            cfg.rcParams.averageBitRate = bps;
            cfg.rcParams.maxBitRate = bps;
            cfg.rcParams.vbvBufferSize = vbv_buffer_size_for(bps, fps);
            cfg.rcParams.vbvInitialDelay = vbv_initial_delay_for(bps, fps);
        }
        let mut reconf = ffi::NV_ENC_RECONFIGURE_PARAMS::default();
        reconf.version = nv_enc_reconfigure_params_ver();
        // SAFETY: by-value copy of NV_ENC_INITIALIZE_PARAMS POD. Its
        // encodeConfig pointer refers to self.init_params's Box which
        // outlives this synchronous call.
        reconf.reInitEncodeParams = *self.init_params.as_ffi();
        reconf.set_resetEncoder(0); // keep DPB / ref frames
        reconf.set_forceIDR(1);     // clean cut so viewer doesn't see ref-loss
        let reconfigure_fn = match self.fn_table.nvEncReconfigureEncoder {
            Some(f) => f,
            None => {
                tracing::warn!("nvEncReconfigureEncoder not present in fn_table");
                return;
            }
        };
        let status =
            unsafe { reconfigure_fn(self.session, &mut reconf as *mut _) };
        if status != ffi::NVENCSTATUS::NV_ENC_SUCCESS {
            tracing::warn!(?status, requested_bps = bps,
                "NVENC nvEncReconfigureEncoder failed");
            return;
        }
        tracing::info!(target_bps = bps, "NVENC bitrate reconfigured");
    }
```

You will likely need to add imports at the top of `encoder.rs` for the new helpers. Find the existing `use crate::nvenc::config::{...};` block (around line 38-44) and add `nv_enc_reconfigure_params_ver, vbv_buffer_size_for, vbv_initial_delay_for` to the list.

- [ ] **Step 3: Add gated integration test**

Find the existing `#[cfg(test)] mod tests` block (around line 461) in `encoder.rs`. Add this test inside the module:

```rust
    /// L4: prove that `set_target_bitrate` actually changes the emitted
    /// bitstream size on a real NVENC GPU. Gated by `prdt_nvenc_bindings`
    /// (NVIDIA Video Codec SDK present at build time) AND `#[ignore]`
    /// (requires Windows GPU at test time). Windows CI invokes with
    /// `cargo test -- --ignored` to fire it.
    ///
    /// Uses a pseudo-random Y/UV pattern (xorshift64 seeded to
    /// 0xDEADBEEF) so the encoder sees high spatial entropy and rate
    /// control has actual work — a constant grey input would compress
    /// so well at both bitrates that the size delta would be noise-bound.
    #[cfg(prdt_nvenc_bindings)]
    #[test]
    #[ignore]
    fn nvenc_set_target_bitrate_changes_emitted_size() {
        use crate::d3d11::{D3d11Device, D3d11Texture, TextureFormat};

        const W: u32 = 1920;
        const H: u32 = 1080;
        const HI_BPS: u32 = 30_000_000;
        const LO_BPS: u32 = 2_000_000;
        const FRAMES_PER_BATCH: u64 = 60;

        let dev = D3d11Device::create_default().expect("d3d11 device");
        let cfg = NvencEncoderConfig {
            width: W, height: H,
            fps_numerator: 60, fps_denominator: 1,
            bitrate_bps: HI_BPS, gop_length: 60,
        };
        let mut enc = NvencEncoder::new(&dev, &cfg).expect("nvenc new");

        // xorshift64 noise generator for high-entropy BGRA fill.
        let mut state: u64 = 0xDEADBEEF;
        let mut next_byte = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        };

        let fill_noise = |tex: &mut D3d11Texture, gen: &mut dyn FnMut() -> u8| {
            let mut bgra = vec![0u8; (W * H * 4) as usize];
            for b in bgra.iter_mut() { *b = gen(); }
            tex.upload_bgra(&bgra).expect("upload");
        };

        let mut tex = D3d11Texture::new_default(&dev, W, H, TextureFormat::Bgra8)
            .expect("texture");

        let mut hi_total: u64 = 0;
        for i in 0..FRAMES_PER_BATCH {
            fill_noise(&mut tex, &mut next_byte);
            let f = enc.encode(&tex, /*force_idr=*/ i == 0, i * 16_667).unwrap();
            hi_total += f.nal_bytes.len() as u64;
        }
        let hi_avg = hi_total / FRAMES_PER_BATCH;

        enc.set_target_bitrate(LO_BPS);

        let mut lo_total: u64 = 0;
        for i in FRAMES_PER_BATCH..(2 * FRAMES_PER_BATCH) {
            fill_noise(&mut tex, &mut next_byte);
            let f = enc.encode(&tex, /*force_idr=*/ false, i * 16_667).unwrap();
            lo_total += f.nal_bytes.len() as u64;
        }
        let lo_avg = lo_total / FRAMES_PER_BATCH;

        assert!(
            lo_avg < hi_avg * 70 / 100,
            "L4 NVENC reconfigure ineffective: lo_avg={lo_avg} should be \
             <70% of hi_avg={hi_avg} (hi_total={hi_total} lo_total={lo_total})"
        );
    }
```

If `D3d11Texture::upload_bgra` does not exist (likely — that name was assumed), use whatever the actual upload API in `crates/media-win/src/d3d11.rs` is. Common names: `upload_subresource`, `update_subresource`, `write_pixels`. Read the file and adapt:

```bash
grep -n "pub fn.*upload\|pub fn.*write\|UpdateSubresource" /home/ubuntu/project/power-remote-dt/crates/media-win/src/d3d11.rs | head -5
```

If no upload helper exists, the test can use the existing `Default::default()` BGRA texture (zeros) plus skip `fill_noise` — but the assertion margin must then widen because zero-filled input compresses to nearly nothing at any bitrate. In that fallback, change the assertion to `lo_avg <= hi_avg` (just monotone-non-increasing) and document the limitation in the test doc comment.

- [ ] **Step 4: Linux check + clippy**

```bash
cargo check -p prdt-media-win --target x86_64-unknown-linux-gnu 2>&1 | tail -5
cargo clippy -p prdt-media-win --target x86_64-unknown-linux-gnu --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: clean (the test body is gated `#[cfg(prdt_nvenc_bindings)]` so it doesn't compile on Linux at all).

- [ ] **Step 5: cargo fmt + commit**

```bash
cargo fmt --all
git add crates/media-win/src/nvenc/encoder.rs
git commit -m "$(cat <<'EOF'
L4 T3: NVENC set_target_bitrate live reconfigure

Replaces the warn!+return stub with nvEncReconfigureEncoder. Mutates
self.init_params.encode_config in place (no Copy on InitParams — the
embedded Box<NV_ENC_CONFIG> would double-free), then by-value-copies
the outer NV_ENC_INITIALIZE_PARAMS POD into NV_ENC_RECONFIGURE_PARAMS.
The encodeConfig pointer in that copy still refers to self's Box.

forceIDR=1 emits a clean cut at the new bitrate so the viewer doesn't
see reference-frame loss. resetEncoder=0 keeps DPB across the call.
VBV updated alongside averageBitRate/maxBitRate via the helpers from T2.

Adds nvenc_set_target_bitrate_changes_emitted_size test, gated
#[cfg(prdt_nvenc_bindings)] #[ignore]. Uses xorshift64 noise input so
high/low bitrates produce visibly different sizes (codex HIGH #3).
Windows CI runs with --ignored.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: OpenH264 `set_target_bitrate` body + dep + xorshift unit test

**Files:**
- Modify: `crates/media-sw/Cargo.toml` (add `openh264-sys2 = "0.9.6"`)
- Modify: `crates/media-sw/src/encoder.rs:127-135` (replace stash-only body)
- Modify: `crates/media-sw/src/encoder.rs` (`#[cfg(test)] mod tests` — add 1 new test)

- [ ] **Step 1: Add openh264-sys2 to media-sw Cargo.toml**

Read current deps:

```bash
grep -n "\[dependencies\]\|openh264" /home/ubuntu/project/power-remote-dt/crates/media-sw/Cargo.toml
```

Find the `[dependencies]` section. After the existing `openh264 = ...` line, add:

```toml
openh264-sys2 = "0.9.6"
```

(Same version that openh264 0.9.3 already pulls in transitively — making it a direct dep just exposes the FFI types we need.)

- [ ] **Step 2: Verify the dep resolves**

```bash
cargo build -p prdt-media-sw --target x86_64-unknown-linux-gnu 2>&1 | tail -5
```

Expected: clean. If cargo complains about a version conflict, check `cargo tree -p prdt-media-sw` and pin to whatever openh264 0.9.3 transitively requires.

- [ ] **Step 3: Write the failing test first (TDD)**

Add this test to the existing `#[cfg(test)] mod tests` block in `crates/media-sw/src/encoder.rs`. If the block doesn't exist yet, create it at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::nv12::I420Frame;

    /// L4: prove `set_target_bitrate` actually shrinks emitted frame
    /// size at runtime (not just stash-for-reinit). Uses xorshift64
    /// noise to fill the Y plane so the encoder sees high spatial
    /// entropy — a constant grey input compresses so well at both
    /// bitrates that the size delta would be noise-bound (codex HIGH #3).
    #[test]
    fn set_target_bitrate_runtime_changes_emitted_size() {
        const W: u32 = 1920;
        const H: u32 = 1080;
        const HI_BPS: u32 = 30_000_000;
        const LO_BPS: u32 = 2_000_000;
        const FRAMES_PER_BATCH: u64 = 60;

        let cfg = Openh264EncoderConfig {
            width: W, height: H,
            target_bitrate_bps: HI_BPS,
            max_fps: 60.0,
        };
        let mut enc = Openh264Encoder::new(cfg).expect("encoder new");

        let mut state: u64 = 0xDEADBEEF;
        let mut next_byte = || -> u8 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        };

        let make_frame = |gen: &mut dyn FnMut() -> u8| -> I420Frame {
            let y_len = (W * H) as usize;
            let uv_len = ((W / 2) * (H / 2)) as usize;
            let mut y = vec![0u8; y_len];
            for b in y.iter_mut() { *b = gen(); }
            // U/V kept neutral grey so encoder focuses on Y entropy
            // (more realistic for screen-share content than coloured noise).
            let u = vec![128u8; uv_len];
            let v = vec![128u8; uv_len];
            I420Frame {
                width: W, height: H,
                y, u, v,
            }
        };

        let mut hi_total: u64 = 0;
        for i in 0..FRAMES_PER_BATCH {
            let f = make_frame(&mut next_byte);
            let enc_f = enc.encode(&f, /*force_idr=*/ i == 0, i * 16_667).unwrap();
            hi_total += enc_f.nal_units.iter().map(|n| n.len() as u64).sum::<u64>();
        }
        let hi_avg = hi_total / FRAMES_PER_BATCH;

        enc.set_target_bitrate(LO_BPS);

        let mut lo_total: u64 = 0;
        for i in FRAMES_PER_BATCH..(2 * FRAMES_PER_BATCH) {
            let f = make_frame(&mut next_byte);
            let enc_f = enc.encode(&f, /*force_idr=*/ false, i * 16_667).unwrap();
            lo_total += enc_f.nal_units.iter().map(|n| n.len() as u64).sum::<u64>();
        }
        let lo_avg = lo_total / FRAMES_PER_BATCH;

        assert!(
            lo_avg < hi_avg * 70 / 100,
            "L4 OpenH264 set_target_bitrate ineffective: lo_avg={lo_avg} \
             should be <70% of hi_avg={hi_avg}",
        );
    }
}
```

Note: the test uses `I420Frame { width, height, y, u, v }` — confirm the struct shape matches `crates/media-sw/src/nv12.rs`. If field names differ, adjust the literal accordingly. Also confirm `Openh264Encoder::encode` signature matches `(&I420Frame, bool, u64) -> Result<EncodedFrame>`.

```bash
grep -n "pub struct I420Frame\|pub fn encode\b\|pub struct Openh264EncoderConfig" /home/ubuntu/project/power-remote-dt/crates/media-sw/src/{encoder,nv12}.rs | head -10
```

If `Openh264EncoderConfig` lacks `max_fps` and uses a different name (e.g. `fps`), adapt the constructor too.

- [ ] **Step 4: Run the test — expect FAIL**

```bash
cargo test -p prdt-media-sw --target x86_64-unknown-linux-gnu --lib set_target_bitrate_runtime_changes 2>&1 | tail -20
```

Expected: test fails because the current `set_target_bitrate` body only updates `self.cfg.target_bitrate_bps` (no live SDK call). The encoder keeps emitting at the original bitrate, so `lo_avg` stays close to `hi_avg`.

If the test passes immediately, the encoder is somehow already reacting (very unlikely given the L3 final review confirmed it as a stash-only stub). Investigate: read the actual current body and the encoder behaviour. Do not proceed to Step 5 until you've confirmed the failing baseline.

- [ ] **Step 5: Replace the no-op body**

Find this exact block in `crates/media-sw/src/encoder.rs:127-135`:

```rust
    fn set_target_bitrate(&mut self, bps: u32) {
        // OpenH264 takes the new bitrate via the next reinit; without
        // reaching into the unsafe raw API there is no in-place setter
        // exposed by openh264 0.9.3. Stash the request — it will take
        // effect on the next call to `encode` after the encoder is
        // reinitialised (which currently only happens on dimension
        // change). Treat as best-effort per the trait doc.
        self.cfg.target_bitrate_bps = bps;
    }
```

Replace with:

```rust
    fn set_target_bitrate(&mut self, bps: u32) {
        self.cfg.target_bitrate_bps = bps;
        // L4: live reconfigure via the SDK's ENCODER_OPTION_BITRATE.
        // openh264 0.9.3 exposes raw_api() (pub const unsafe fn) which
        // returns &mut EncoderRawAPI; set_option takes a *mut c_void to
        // the option payload (SBitrateInfo here). Effective on the next
        // encode() call.
        let mut info = openh264_sys2::SBitrateInfo {
            iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
            iBitrate: bps as std::os::raw::c_int,
        };
        // SAFETY: raw_api() returns a &mut to a field of self.inner; the
        // FFI call is synchronous and the &mut info pointer outlives the
        // call. set_option does not retain the pointer past return.
        let rc = unsafe {
            self.inner.raw_api().set_option(
                openh264_sys2::ENCODER_OPTION_BITRATE,
                &mut info as *mut _ as *mut std::ffi::c_void,
            )
        };
        if rc != 0 {
            tracing::warn!(
                rc,
                requested_bps = bps,
                "OpenH264 set_option(BITRATE) failed",
            );
        }
    }
```

Note: depending on the openh264-sys2 0.9.6 layout (verified in T0/Q5), the import path may need adjustment. If the test fails to compile because `openh264_sys2::ENCODER_OPTION_BITRATE` does not resolve, try `openh264_sys2::generated::types::ENCODER_OPTION_BITRATE` (and likewise for `SBitrateInfo` and `SPATIAL_LAYER_ALL`). The grep in T0 Step 3 will tell you which form is correct.

You may also need to add `use tracing;` at the top of encoder.rs if it isn't already imported (search for it; existing code likely has it via `use tracing::warn;` or similar).

- [ ] **Step 6: Run the test — expect PASS**

```bash
cargo test -p prdt-media-sw --target x86_64-unknown-linux-gnu --lib set_target_bitrate_runtime_changes 2>&1 | tail -15
```

Expected: test passes with `lo_avg < hi_avg * 0.7`. Print the actual values from the assertion message if it fails — OpenH264 may need a few more frames to converge after a step change. If `lo_avg / hi_avg` is in the 0.70-0.85 range, increase `FRAMES_PER_BATCH` to 90 to give rate-control more time. Don't relax the assertion below 70% — that would mask a broken reconfigure.

- [ ] **Step 7: Run full media-sw tests + clippy**

```bash
cargo test -p prdt-media-sw --target x86_64-unknown-linux-gnu 2>&1 | tail -3
cargo clippy -p prdt-media-sw --target x86_64-unknown-linux-gnu --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: all media-sw tests pass (existing 6 + 1 new = 7), no clippy warnings.

- [ ] **Step 8: cargo fmt + commit**

```bash
cargo fmt --all
git add crates/media-sw/Cargo.toml crates/media-sw/src/encoder.rs
git commit -m "$(cat <<'EOF'
L4 T4: OpenH264 set_target_bitrate live reconfigure

Replaces the stash-only body (which only took effect on encoder reinit
i.e. dimension change) with a live SDK call via raw_api().set_option(
ENCODER_OPTION_BITRATE, &SBitrateInfo). Adds openh264-sys2 = "0.9.6"
as a direct dep so the FFI types are accessible without going through
the high-level openh264 crate.

Adds set_target_bitrate_runtime_changes_emitted_size unit test using
xorshift64 noise input on the Y plane (codex HIGH #3 — constant grey
compresses so well at both bitrates that the assertion margin would
be noise-bound).

Test asserts lo_avg < 70% of hi_avg after stepping 30M → 2M, which
confirms the encoder actually honored the runtime change rather than
stashing it for a reinit that never came.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: tc qdisc netem smoke script

**Files:**
- Create: `scripts/l4-netem-smoke.sh`

- [ ] **Step 1: Write the script**

Create `scripts/l4-netem-smoke.sh`:

```bash
#!/usr/bin/env bash
# L4 smoke: inject controlled packet loss to verify L3 controller +
# L4 encoder reconfigure path. Run host with --bitrate-mbps 30 first,
# then this script during the viewer connect.
#
# Usage:
#   sudo ./scripts/l4-netem-smoke.sh add <iface> <loss_pct>
#   sudo ./scripts/l4-netem-smoke.sh del <iface>
#   sudo ./scripts/l4-netem-smoke.sh status <iface>
#
# Note on egress-only (codex MEDIUM #4 / spec §8 risk):
#   tc qdisc on a regular interface only affects EGRESS (host→viewer).
#   That is the correct direction for this smoke because L3's controller
#   measures viewer-perceived loss in the host→viewer path. Viewer→host
#   KeepAlive is unaffected and the host watchdog stays quiet.
#
# Note on loss percentage (codex MEDIUM #4):
#   The transport's default FEC (k=64, m=6) recovers up to ~8.5% loss.
#   Pure 5% loss therefore yields purge=0 and the controller never sees
#   loss. Default to 15% loss + 50ms±20ms delay/jitter to push past the
#   FEC threshold and burst-amplify; this reliably triggers
#   purge_assembler() in the viewer.
set -euo pipefail
ACTION="${1:?usage: $0 <add|del|status> <iface> [loss_pct]}"
IFACE="${2:?missing iface (e.g. eth0)}"
case "$ACTION" in
  add)
    LOSS="${3:?missing loss percent (e.g. 15)}"
    tc qdisc add dev "$IFACE" root netem \
      loss "${LOSS}%" delay 50ms 20ms distribution normal
    echo "netem added: $IFACE loss ${LOSS}% delay 50ms±20ms"
    ;;
  del)
    tc qdisc del dev "$IFACE" root || true
    echo "netem removed: $IFACE"
    ;;
  status)
    tc qdisc show dev "$IFACE"
    ;;
  *) echo "unknown action: $ACTION"; exit 1 ;;
esac
```

- [ ] **Step 2: chmod +x**

```bash
chmod +x scripts/l4-netem-smoke.sh
ls -la scripts/l4-netem-smoke.sh
```

Expected: `-rwxr-xr-x ...` (executable bit set).

- [ ] **Step 3: Smoke-test the script syntax (without sudo)**

```bash
bash -n scripts/l4-netem-smoke.sh && echo "syntax ok"
```

Expected: `syntax ok`. (We can't run it without sudo + real tc, that happens in T7.)

- [ ] **Step 4: Commit**

```bash
git add scripts/l4-netem-smoke.sh
git commit -m "$(cat <<'EOF'
L4 T5: tc qdisc netem smoke helper

scripts/l4-netem-smoke.sh wraps `tc qdisc add/del/show` with a
loss + delay/jitter default that's tuned to the transport's FEC
threshold (k=64 m=6 tolerates ~8.5%, so default 15% loss is past it
and 50ms±20ms jitter amplifies bursts to make sure the viewer sees
purge events).

Documented in the header: egress-only is correct (matches L3's
viewer-perceived-loss measurement direction) and the loss% choice
is calibrated to the FEC budget (codex MEDIUM #4).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Workspace build/clippy/test sweep + draft PR

**Files:** none (validation task)

- [ ] **Step 1: Workspace build (Linux)**

```bash
cargo build --workspace --target x86_64-unknown-linux-gnu --all-targets 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 2: Workspace clippy (Linux)**

```bash
cargo clippy --workspace --target x86_64-unknown-linux-gnu --all-targets -- -D warnings 2>&1 | tail -10
```

Expected: zero warnings.

- [ ] **Step 3: Workspace tests (Linux)**

```bash
cargo test --workspace --target x86_64-unknown-linux-gnu 2>&1 | tail -20
```

Expected: 366 baseline (post-L3) + 1 new (T4 OpenH264 unit test) = **367 passed** (excl pre-existing flaky `transport::probe_test::two_transports_find_each_other`). The NVENC test is `#[cfg(prdt_nvenc_bindings)]` so it's skipped on Linux.

If the count differs, capture exact numbers. The L4 T4 test must pass.

- [ ] **Step 4: cargo fmt --check**

```bash
cargo fmt --all -- --check
```

Expected: no diff. If diff exists, run `cargo fmt --all`, commit with message `style(L4): cargo fmt`, and continue.

- [ ] **Step 5: Push branch**

```bash
git push -u origin phase-l4-encoder-reconfigure 2>&1 | tail -5
```

- [ ] **Step 6: Open draft PR**

```bash
gh pr create --draft --title "L4: live encoder reconfigure (OpenH264 + NVENC)" --body "$(cat <<'EOF'
## Summary
- OpenH264: replace stash-only `set_target_bitrate` with `raw_api().set_option(ENCODER_OPTION_BITRATE, ...)` — live SDK call, applies on next encode
- NVENC: replace `warn!+return` stub with `nvEncReconfigureEncoder` + `forceIDR=1` + `resetEncoder=0`. Mutates owned `Box<NV_ENC_CONFIG>` in place (no `Copy` on `InitParams` — would double-free). VBV updated alongside bitrate.
- Host: `bytes_sent_window` u64 added to `host tx stats` log so smoke can verify "encoder actually shrunk frames" (codex HIGH #2)
- New test: OpenH264 unit test asserts emitted-size drop on runtime bitrate change. NVENC integration test gated `#[cfg(prdt_nvenc_bindings)] #[ignore]` for Windows CI.
- Smoke helper `scripts/l4-netem-smoke.sh` calibrated to FEC threshold (15% loss + 50ms±20ms jitter)
- MF stays as `warn!+return` — deferred to L5 (requires AMD/Intel Windows test host)

Spec: `docs/superpowers/specs/2026-05-11-l4-encoder-reconfigure-design.md` (commit e99c883, addresses codex review)
Plan: `docs/superpowers/plans/2026-05-11-l4-encoder-reconfigure.md`

## Test plan
- [x] `cargo test --workspace` Linux: 367 passed (366 baseline + 1 new)
- [x] `cargo clippy --workspace -- -D warnings` Linux green
- [ ] Windows CI green (this PR)
- [ ] Manual smoke: WSLg host (--bitrate-mbps 30) + tc netem 15% + real Wayland viewer → `target_bps` ≤ 5 Mbps within 30s, `bytes_sent_window` drops ≤50%, AI recovery on netem del

## Codex review fixes from spec v1 → v2
- HIGH #1: InitParams Copy double-free → in-place mutation pattern
- HIGH #2: missing bytes/sec log → bytes_sent_window added in T0
- HIGH #3: constant-grey test input → xorshift64 noise
- MEDIUM #4: tc 5% under FEC threshold → 15% + jitter
- MEDIUM #5: NV_ENC_RECONFIGURE_PARAMS_VER → SDK 13 high-bit form, T0/Q3 verifies
- MEDIUM #6: VBV not updated → helpers used at both init and reconfigure
- LOW #7: `_init_params` already exists → renamed in T2

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 7: Verify PR was created and capture number**

```bash
gh pr list --head phase-l4-encoder-reconfigure 2>&1
```

Capture the PR number for use in T8 commit messages and the squash-merge step.

---

## Task 7: Linux Wayland smoke (DoD #1)

**Files:** none (manual verification)

This validates the entire L3 + L4 stack: viewer detects loss → SetBitrate to host → host reconfigures encoder → emitted bytes drop → viewer recovers when loss subsides.

- [ ] **Step 1: Build release binary on WSLg**

```bash
cargo build --release --target x86_64-unknown-linux-gnu -p prdt-host -p prdt-viewer 2>&1 | tail -5
```

- [ ] **Step 2: Trigger GitHub release workflow for the real-Wayland viewer**

Real Wayland machine needs to download the L4 binary. Use the same approach as L3 smoke:

```bash
gh workflow run release.yml --ref phase-l4-encoder-reconfigure -f ref=phase-l4-encoder-reconfigure
sleep 3
gh run list --workflow=release.yml --branch phase-l4-encoder-reconfigure --limit 2
```

Capture the run ID. Build takes ~5 minutes.

- [ ] **Step 3: Start host on WSLg**

```bash
RUST_LOG=info ./target/x86_64-unknown-linux-gnu/release/prdt host \
  --bind 0.0.0.0:9000 \
  --bitrate-mbps 30 \
  --encoder openh264 \
  --silent-allow > /tmp/prdt-host-l4.log 2>&1 &
echo "host PID: $!"
sleep 2
grep -i "pubkey" /tmp/prdt-host-l4.log | head -3
hostname -I 2>/dev/null
```

Capture WSL IP + pubkey for the viewer command.

- [ ] **Step 4: Connect viewer from real Wayland machine**

On the Wayland machine, after the GitHub release workflow finishes:

```bash
gh run download <run_id> -R drian320/power-remote-dt -n prdt-linux-x86_64
chmod +x prdt-linux-x86_64
RUST_LOG=info ./prdt-linux-x86_64 connect \
  --host <wsl-ip>:9000 \
  --host-pubkey <captured-pubkey> \
  --codec h264 --decoder openh264 2>&1 | tee /tmp/prdt-viewer-l4.log
```

Wait until handshake completes and frames are flowing — should see `viewer rx stats frames_received=N textures_decoded=N` lines climbing.

- [ ] **Step 5: Capture baseline (30s)**

Let the session run for 30 seconds with no induced loss. Record:
- Host log: `host tx stats frames_sent=N send_errors=0 bytes_sent_window=B` — note B at steady state (should be roughly 30 Mbps / 8 = ~3.75 MB/s = ~3,750,000 bytes/s)
- Viewer log: `viewer rx stats frames_received=N textures_decoded=N` — N climbing smoothly
- No `L3 sent SetBitrate` lines (controller in AI ceiling region)

- [ ] **Step 6: Inject loss (60s)**

On WSLg, in a separate terminal:

```bash
sudo ./scripts/l4-netem-smoke.sh add eth0 15
```

Watch logs for ~60 seconds. Observe:
- Viewer log: `L3 sent SetBitrate target_bps=N` repeating with N descending — record the sequence (e.g. 30M → 21M → 14M → 10M → 7M → 5M → 3M)
- Host log: `viewer requested bitrate change target_bps=N` matching each
- Host log: `NVENC bitrate reconfigured target_bps=N` (if NVENC) or no warn from OpenH264 path
- Host log: `bytes_sent_window` should drop sharply to ≤ 50% of baseline (e.g. baseline ~3.75M → loss ~1.5M or less)

If `bytes_sent_window` does NOT drop with `target_bps`, **L4 reconfigure is broken**. The DoD fails — diagnose before continuing. Likely causes: encoder backend's `set_target_bitrate` errored silently (check warn! lines), or the producer.set_target_bitrate path isn't routing.

- [ ] **Step 7: Remove loss + recovery (60s)**

```bash
sudo ./scripts/l4-netem-smoke.sh del eth0
```

Observe:
- Viewer log: `target_bps` rises monotonically by ~200 kbps every 1-2s (AI step from L3)
- Host log: `bytes_sent_window` rises to track
- After ~60s the controller should be back near max_bps

- [ ] **Step 8: Confirm session survival**

Total session ≥ 5 minutes. Watch for:
- `host watchdog … session kill` — should NOT appear (the L4 reconfigure should have kept the stream alive through the loss window)
- Viewer disconnect — should NOT happen

- [ ] **Step 9: Capture metrics for STATUS update**

Note for T8 STATUS write-up:
- Total session duration
- frames_sent / frames_received final counts
- target_bps low watermark (worst-case bitrate during loss inject)
- bytes_sent_window baseline vs loss-inject ratio (should be ≤ 50%)
- AI recovery time (time from netem del to target_bps == max_bps)
- Number of L3 SetBitrate events sent
- Whether host watchdog killed the session

- [ ] **Step 10: Stop host cleanly**

```bash
pkill -f "target/x86_64-unknown-linux-gnu/release/prdt host"
```

(The exit-144 cascade is benign per L2/L3 precedent.)

If WSLg's `tc` is still active, also clean it:

```bash
sudo ./scripts/l4-netem-smoke.sh del eth0
```

---

## Task 8: STATUS update + tag

**Files:**
- Modify: `docs/superpowers/STATUS.md` — header tag + L4 entry under B2 + L4 smoke walkthrough record

- [ ] **Step 1: Read current header**

```bash
sed -n '1,12p' docs/superpowers/STATUS.md
```

Confirm: `Last updated: 2026-05-11`, `Latest tag: phase-l3-adaptive-bitrate-complete`.

- [ ] **Step 2: Update header**

Edit `docs/superpowers/STATUS.md` lines 3-4. Replace:

```markdown
**Last updated:** 2026-05-11
**Latest tag:** `phase-l3-adaptive-bitrate-complete`
```

with:

```markdown
**Last updated:** 2026-05-11
**Latest tag:** `phase-l4-encoder-reconfigure-complete`
```

(Date may be the same calendar day, depending on when smoke ran. If different, update.)

- [ ] **Step 3: Find insertion point under B2**

```bash
grep -n "L3 smoke walkthrough\|L2 残候補" docs/superpowers/STATUS.md
```

The L4 bullet goes immediately AFTER the `L3 smoke walkthrough` bullet (which ends with `... 4-step RequestIdr loop fully working in logs`-style content), and BEFORE the `**L2 残候補**` line.

- [ ] **Step 4: Insert L4 bullets**

Use the Edit tool. Find the `**L2 残候補**` line and insert the L4 block immediately above it. The L4 block (matching the 2-space indent of the parent bullet, with 4-space indents for sub-bullets):

```markdown
  - **L4 (`phase-l4-encoder-reconfigure-complete`, 2026-05-11)**: L3 で残った encoder reconfigure 問題を解消。NVENC + OpenH264 の `set_target_bitrate` を no-op stub から live reconfigure に置換し、L3 controller が production で初めて意味を持つようになる。MF は L5 へ defer (AMD/Intel Windows test host 必要)。Cross-platform、~430 LoC。
    - **OpenH264** (`crates/media-sw/src/encoder.rs:127`): `unsafe encoder.raw_api().set_option(ENCODER_OPTION_BITRATE, &SBitrateInfo)` で live SDK 呼び出し、次回 `encode()` から効く。`openh264-sys2 = "0.9.6"` を直接 dep に追加 (既存 `openh264 0.9.3` が transitively pull していたバージョン)。新 unit test `set_target_bitrate_runtime_changes_emitted_size` で xorshift64 noise input → 30M→2M reconfigure → 後続 60 frames の avg size が前 60 frames の <70% であることを assert (codex review HIGH #3)
    - **NVENC** (`crates/media-win/src/nvenc/encoder.rs:328`): `nvEncReconfigureEncoder` + `forceIDR=1` + `resetEncoder=0` で live reconfigure。`InitParams` は `Box<NV_ENC_CONFIG>` 所有のため Copy 不可 (codex HIGH #1)、よって `self.init_params.encode_config_mut()` で in-place mutate + outer POD を by-value copy する pattern。VBV (vbvBufferSize, vbvInitialDelay) も同時更新 (codex MEDIUM #6 — bitrate と VBV を coupled に保つ helper を `nvenc/config.rs` に extract)。`nv_enc_reconfigure_params_ver()` は SDK 13 high-bit convention `nvenc_struct_version(1) | (1 << 31)` (codex MEDIUM #5)。新 integration test gated `#[cfg(prdt_nvenc_bindings)] #[ignore]`、Windows CI で `--ignored` 起動
    - **Host** (`crates/host/src/lib.rs` video task): `bytes_sent_window: u64` accumulator を追加し既存 `host tx stats` info! line に出力 (codex HIGH #2 — DoD で `bytes/sec` を参照していたが現行ログに無くて検証不可能だった)。1 秒ごとに reset
    - **Smoke helper** (`scripts/l4-netem-smoke.sh` 新): `tc qdisc add/del/status` ラッパー、デフォルト `loss 15% delay 50ms±20ms distribution normal`。FEC k=64 m=6 の tolerance 8.5% を超える 15% + jitter で確実に purge を発火させる (codex MEDIUM #4)
    - **MF** (`crates/media-win/src/mf/encoder.rs:204`): warn!+return のまま、コメントに「L5 candidate — MFT vendor-specific behaviour requires AMD/Intel Windows test host」明記
    - **Tests**: 1 new (OpenH264 unit) + 1 new gated (NVENC integration) = 2 new tests cross-platform。Linux `cargo test --workspace` 367 passed (366 baseline + 1 new、excl pre-existing flaky `probe_test`)
    - **Linux regression bar**: `cargo build/clippy --workspace -- -D warnings` 両 target green
    - **Windows regression bar**: GitHub Actions release workflow PR で green (PR `<FILL FROM T6>`)
  - **L4 smoke walkthrough (2026-05-11)**: WSLg host (`--bitrate-mbps 30 --encoder openh264 --silent-allow`) + 実機 Wayland viewer (`--codec h264 --decoder openh264`、GitHub Actions release artifact から DL) + `tc qdisc netem loss 15% delay 50ms±20ms` で loss inject。**spec §1 DoD #1 達成 ✅** — <FILL FROM T7 OBSERVATIONS: target_bps low watermark, bytes_sent_window baseline:loss ratio, AI recovery time, total session duration>。L4 encoder reconfigure 完全動作確認: target_bps の遷移と `bytes_sent_window` の追従が観測でき、controller→encoder→bitstream の end-to-end loop が production で機能することを実証。
```

The placeholders `<FILL FROM T6>` and `<FILL FROM T7 OBSERVATIONS>` should be replaced with actual values captured in those tasks.

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/STATUS.md
git commit -m "$(cat <<'EOF'
docs(STATUS): record L4 encoder reconfigure completion + smoke walkthrough

L4 OpenH264 + NVENC live reconfigure makes the L3 controller actually
have an effect in production. Smoke walkthrough verified end-to-end
controller→encoder→bitstream loop with tc qdisc netem 15% loss inject;
target_bps descended and bytes_sent_window tracked.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push origin phase-l4-encoder-reconfigure
```

- [ ] **Step 6: Mark PR ready (if still draft)**

```bash
gh pr ready <PR#>
```

- [ ] **Step 7: User squash-merges (auto-mode classifier blocks gh pr merge)**

Ask user to run:

```bash
!gh pr merge <PR#> --squash --delete-branch
```

Wait for confirmation.

- [ ] **Step 8: Pull master + tag**

```bash
git checkout master
git pull origin master
git log --oneline -3   # capture squash sha
git tag -a phase-l4-encoder-reconfigure-complete <squash-sha> -m "L4: live encoder reconfigure (OpenH264 + NVENC)

Spec: docs/superpowers/specs/2026-05-11-l4-encoder-reconfigure-design.md
Plan: docs/superpowers/plans/2026-05-11-l4-encoder-reconfigure.md
PR:   <pr-url>
Smoke: <FILL — short summary, e.g. 5min session, 15% loss, target dropped to N Mbps, bytes_sent_window dropped M%>

L3 viewer-side AIMD controller now has actual production effect on
OpenH264 (Linux) and NVENC (Windows). MF deferred to L5."
git push origin phase-l4-encoder-reconfigure-complete
```

- [ ] **Step 9: Verify**

```bash
git tag -l "phase-l*-complete" | sort
gh pr view <PR#> --json state,mergeCommit
```

Expected: tag list now includes `phase-l4-encoder-reconfigure-complete`; PR is `MERGED`.

---

## Done Criteria (mirrors spec §1)

1. **Linux Wayland smoke (DoD #1)**: tc netem 15% loss inject → viewer log shows target_bps ≤ 5 Mbps within 30s, host log shows `bytes_sent_window` ≤ 50% of baseline (encoder actually shrunk frames), AI recovery on netem del, session survives 5+ min
2. **OpenH264 unit test passes**: `cargo test -p prdt-media-sw set_target_bitrate_runtime_changes_emitted_size` — `lo_avg < hi_avg * 70 / 100`
3. **NVENC integration test exists (gated)**: src tree has `#[cfg(prdt_nvenc_bindings)] #[ignore]` test for Windows CI
4. **Linux + Windows CI green**: 366 baseline + 1 new = 367 pass, no clippy warnings
5. **STATUS.md updated**, `phase-l4-encoder-reconfigure-complete` tag pushed

---

## Risk Notes for Implementer

- **Auto-mode classifier**: blocks `0.0.0.0:9000` bind, `gh pr merge`, `sudo tc`. Use `.claude/settings.local.json` permission rules (already added for L2/L3) or surface to user via `!` prefix
- **Windows CI nuances**: any change to `media-win/src/nvenc/encoder.rs` may surface NVENC-cfg-gating issues even when Linux passes. Reference the L1.5b `prdt_nvenc_bindings` pattern at `crates/media-win/build.rs:9-10`
- **Pre-existing flaky test**: `transport::probe_test::two_transports_find_each_other` fails on master too; ignore it
- **OpenH264 sys path**: if T0/Q5 finds the constants are not at the crate root, T4 must use `openh264_sys2::generated::types::*` instead. Either path produces the same FFI call; only the `use` line changes
- **NVENC test on Linux**: the test is gated `#[cfg(prdt_nvenc_bindings)]` — it won't even compile on Linux unless the SDK is present. Linux CI just sees an empty `mod tests` for this test, which is fine
- **Smoke gotcha — egress only**: tc qdisc on a regular interface only affects EGRESS direction (host→viewer). That's the correct direction for L3's controller (which measures viewer-perceived loss in the host→viewer path). KeepAlive direction is unaffected, host watchdog stays quiet. Documented in script header comment (codex MEDIUM #4)
- **If `bytes_sent_window` doesn't drop during smoke**: L4 reconfigure is broken. Diagnose by checking host log for `NVENC nvEncReconfigureEncoder failed` or `OpenH264 set_option(BITRATE) failed rc=N` warnings. Most likely cause: `nv_enc_reconfigure_params_ver()` returned the wrong value (codex MEDIUM #5) → NVENC returns `NV_ENC_ERR_INVALID_VERSION`. Adjust the `(1 << 31)` portion or read directly from bindgen output
- **Encoder reset on reconfigure**: NVENC's `resetEncoder=0` keeps the DPB. If the test ever shows ref-frame loss after a reconfigure, flip to `resetEncoder=1` (more expensive but unambiguous). The smoke procedure in T7 should reveal this if it's an issue
