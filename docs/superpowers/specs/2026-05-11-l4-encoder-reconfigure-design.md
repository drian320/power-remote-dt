# L4 — Live Encoder Reconfigure (OpenH264 + NVENC)

**Date:** 2026-05-11
**Phase:** L4 (post-L3 adaptive bitrate)
**Branch (suggested):** `phase-l4-encoder-reconfigure`
**Cross-platform:** Linux (OpenH264) + Windows (NVENC), regression bar = 0
**Estimated LoC:** ~400 across 2 modify + 1 new + 1 new script
**Predecessor:** L3 adaptive bitrate (`docs/superpowers/specs/2026-05-11-l3-adaptive-bitrate-design.md`, master `fbc031a`)

---

## 1. Goal & Non-Goals

### Goal
L3 final review (commit `fbc031a`) revealed the L3 viewer-side AIMD controller and host-side SetBitrate wire are fully functional, but **all three encoder backends accept the bitrate change and discard it**:

- `crates/media-win/src/nvenc/encoder.rs:328` — `warn!` + return (no-op)
- `crates/media-win/src/mf/encoder.rs:204` — `warn!` + return (no-op)
- `crates/media-sw/src/encoder.rs:127` — stashes value but only applies on encoder reinit, which today happens only on dimension change

L4 makes the L3 controller actually have an effect in production by wiring the two primary backends (OpenH264 for Linux, NVENC for Windows) to their respective live-reconfigure APIs. MF is deferred to L5 because it requires a non-NVIDIA Windows test host.

### Definition of Done
1. **Linux Wayland smoke (DoD #1)**: WSLg host + real Wayland viewer + `tc qdisc netem loss 5%` inject mid-session → viewer log shows `target_bps` descending below 5 Mbps within 30s, host log shows `host tx stats bytes/sec` tracking `target_bps` (encoder actually emitted smaller frames), loss removal recovers via AI, session survives 5+ minutes
2. **OpenH264 unit test**: `cargo test -p prdt-media-sw set_target_bitrate_runtime_changes_emitted_size` passes — 60 frames at 30 Mbps then `set_target_bitrate(2_000_000)` then 60 more frames, asserts the second batch's average emitted size is < 70% of the first
3. **NVENC integration test**: Same shape as the OpenH264 test but `#[cfg(prdt_nvenc_bindings)] #[ignore]`, exists in src tree so Windows CI can run with `--ignored`
4. **L3 regression bar**: 366 baseline tests + 1 new = 367 passed (excl pre-existing flaky `transport::probe_test::two_transports_find_each_other`); Linux + Windows CI green

### Non-Goals (deferred)
- MF live reconfigure (L5 — requires AMD/Intel Windows test host)
- Viewer↔Host bitrate cap handshake negotiation (L5 — L3 final review MEDIUM)
- L3 polish items (anonymous Arc args naming, warmup-guard helper test) — separate L4.5 if material
- NVENC test execution on real Windows hardware (follow-up after L4 Linux smoke)
- AV1 / SVC layered encoding
- Encoder reconfigure under quality presets (e.g. low-latency vs quality preset switching)

---

## 2. Background

### L3 final review findings (commit `fbc031a`)

The opus-tier final reviewer of L3 confirmed:

| Backend | Current `set_target_bitrate` body | Effect on production |
|---|---|---|
| NVENC (`media-win/src/nvenc/encoder.rs:328`) | `tracing::warn!(...); return;` | None |
| MF (`media-win/src/mf/encoder.rs:204`) | `tracing::warn!(...); return;` | None |
| OpenH264 (`media-sw/src/encoder.rs:127`) | `self.cfg.target_bitrate_bps = bps;` (stash only) | Only applied on next `Encoder::new()` call which happens only on dimension change |

L3 STATUS bullet under B2 documents the gap explicitly: *"L3 SetBitrate 未送信 = controller AI ceiling 維持 = 期待動作 (loss < 0.5% 領域)。…L4 で encoder reconfigure を実装することで L3 controller が production で初めて意味を持つ"*

### Reconfigure API surface

**OpenH264** (`openh264 = "0.9.3"`, dep at `crates/media-sw/Cargo.toml`):
- `Encoder::raw_api()` exists and is `pub const unsafe fn` (returns `&mut EncoderRawAPI`)
- `EncoderRawAPI::set_option(eOptionId, pOption)` is `pub unsafe fn`
- `openh264-sys2 = "0.9.6"` provides `ENCODER_OPTION_BITRATE` constant and `SBitrateInfo` struct
- Verified by reading `~/.cargo/registry/src/.../openh264-0.9.3/src/encoder.rs:1062` and `~/.cargo/registry/src/.../openh264-sys2-0.9.6/src/generated/types.rs`

**NVENC** (`prdt_nvenc_bindings`, bindgen of NVIDIA Video Codec SDK):
- `NV_ENCODE_API_FUNCTION_LIST.nvEncReconfigureEncoder: Option<unsafe extern "C" fn(...)>` is part of the function table
- `NV_ENC_RECONFIGURE_PARAMS` struct with `version`, `reInitEncodeParams`, `resetEncoder`, `forceIDR` bit-field
- The build.rs allowlist (`allowlist_type("NV_ENC.*")`) should already pull in both, but T0 must verify via `cargo expand` or the OUT_DIR'd `nvenc_bindings.rs`

**MF** (`crates/media-win/src/mf/encoder.rs`):
- Already calls `ICodecAPI::SetValue(&CODECAPI_AVEncCommonMeanBitRate, ...)` at init (line 374)
- Runtime `SetValue` may or may not be honored by the MFT — depends on vendor (NVIDIA's MF MFT historically ignores; AMD/Intel untested)
- Out of scope for L4

---

## 3. Architecture

### 3.1 No external API change

L3 already plumbed `VideoProducer::set_target_bitrate(&mut self, bps: u32)` end-to-end:
- Viewer controller decides target_bps and sends `ControlMessage::SetBitrate`
- Host control loop forwards to mpsc
- Host video loop drains mpsc per frame and calls `producer.set_target_bitrate(bps)`
- Producer forwards to encoder via the trait

L4 only changes encoder body implementations. Trait, wire, host, viewer all unchanged.

### 3.2 OpenH264 reconfigure (`crates/media-sw/src/encoder.rs:127`)

Replace the stash-only body with a live SDK call:

```rust
fn set_target_bitrate(&mut self, bps: u32) {
    self.cfg.target_bitrate_bps = bps;
    let mut info = openh264_sys2::SBitrateInfo {
        iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
        iBitrate: bps as std::os::raw::c_int,
    };
    // SAFETY: raw_api() returns &mut EncoderRawAPI bound to self.inner's lifetime;
    // set_option is FFI but takes a *mut c_void to a stack-allocated struct we own.
    let rc = unsafe {
        self.inner.raw_api().set_option(
            openh264_sys2::ENCODER_OPTION_BITRATE,
            &mut info as *mut _ as *mut std::ffi::c_void,
        )
    };
    if rc != 0 {
        tracing::warn!(rc, requested_bps = bps, "OpenH264 set_option(BITRATE) failed");
    }
}
```

**Apply timing**: Effective on the next `Encoder::encode_at()` call (OpenH264 SDK semantics).

**Dependency add**: `crates/media-sw/Cargo.toml` adds `openh264-sys2 = "0.9.6"` (matches the version `openh264 0.9.3` already pulls in transitively).

### 3.3 NVENC reconfigure (`crates/media-win/src/nvenc/encoder.rs:328`)

Replace the warn-only body with `nvEncReconfigureEncoder`:

```rust
fn set_target_bitrate(&mut self, bps: u32) {
    let mut new_params = self.init_params; // requires self.init_params: InitParams field
    {
        let cfg = new_params.encode_config_mut();
        cfg.rcParams.averageBitRate = bps;
        cfg.rcParams.maxBitRate = bps;
    }
    let mut reconf = ffi::NV_ENC_RECONFIGURE_PARAMS::default();
    reconf.version = nv_enc_reconfigure_params_ver();
    reconf.reInitEncodeParams = new_params.into_inner();
    // Bit-fields: keep encoder state (DPB), force a clean IDR cut.
    reconf.set_resetEncoder(0);
    reconf.set_forceIDR(1);
    let reconfigure_fn = match self.fn_table.nvEncReconfigureEncoder {
        Some(f) => f,
        None => {
            tracing::warn!("nvEncReconfigureEncoder not present in fn_table");
            return;
        }
    };
    let status = unsafe { reconfigure_fn(self.session, &mut reconf as *mut _) };
    if status != ffi::NVENCSTATUS::NV_ENC_SUCCESS {
        tracing::warn!(?status, requested_bps = bps,
            "NVENC nvEncReconfigureEncoder failed");
        return;
    }
    self.init_params = new_params;
    tracing::info!(target_bps = bps, "NVENC bitrate reconfigured");
}
```

**Required struct change**: `NvencEncoder` must hold `init_params: InitParams` as a field so the L4 reconfigure can rebuild a modified copy. T0 verifies whether the field exists today (likely consumed by `new()` and discarded, so will need adding).

**Apply timing**: With `forceIDR=1`, the next `EncodePicture` call emits an IDR carrying SPS/PPS at the new rate. With `forceIDR=0`, would apply at the next natural IDR boundary (gop_length=60 → up to 1 second delay). Force-IDR is preferred to give the viewer an immediate clean cut without ref-frame loss.

**State preservation**: `resetEncoder=0` keeps DPB/refs across the reconfigure — encoder doesn't re-initialize, so no spawn cost.

### 3.4 MF unchanged

`crates/media-win/src/mf/encoder.rs:204` keeps the `warn!+return` body. Comment update: "L5 candidate — MFT vendor-specific behaviour, requires AMD/Intel Windows test host."

### 3.5 NVENC `init_params` storage (open question Q1)

Current `NvencEncoder` struct (read at `crates/media-win/src/nvenc/encoder.rs:65-100`) likely has shape:

```rust
pub struct NvencEncoder {
    fn_table: ffi::NV_ENCODE_API_FUNCTION_LIST,
    session: *mut std::ffi::c_void,
    bitstream_buffer: *mut std::ffi::c_void,
    width: u32,
    height: u32,
    // ... but NOT init_params
}
```

T0 verifies. If absent, T1 adds `init_params: InitParams` and stashes it at the end of `new()` after the `nvEncInitializeEncoder` call succeeds. No Drop impact (InitParams owns no FFI resources directly — they're inline structs).

### 3.6 Bindgen visibility (open question Q2)

The bindgen invocation in `crates/media-win/build.rs` currently lists:
- `allowlist_function("NvEncodeAPICreateInstance")` — only this one function
- `allowlist_function("NvEncodeAPIGetMaxSupportedVersion")`
- `allowlist_type("NV_ENC.*")` — pulls in struct types
- `allowlist_var("NV_ENC.*")` and `allowlist_var("NVENC.*")` — constants

`nvEncReconfigureEncoder` is not in `allowlist_function`, but it doesn't need to be: it's a function pointer (`Option<unsafe extern "C" fn(...)>`) inside the `NV_ENCODE_API_FUNCTION_LIST` struct, which IS allowlisted via `allowlist_type("NV_ENC.*")`. Bindgen pulls in the struct's fields (function pointer types are part of the struct definition).

T0 verifies by reading `target/x86_64-pc-windows-msvc/.../out/nvenc_bindings.rs` (Windows build) or `cargo expand` from the relevant module. If absent, add `allowlist_function("nvEncReconfigure.*")` to build.rs.

### 3.7 Reconfigure params version helper (open question Q3)

NVENC SDK uses version macros like `NVENCAPI_STRUCT_VERSION(7)` for each struct version. Existing helpers in `crates/media-win/src/nvenc/config.rs`:

```rust
pub const fn nv_enc_pic_params_ver() -> u32 { ... }
pub const fn nv_enc_open_encode_session_ex_params_ver() -> u32 { ... }
// (etc)
```

T0 adds:

```rust
pub const fn nv_enc_reconfigure_params_ver() -> u32 {
    nvenc_struct_version(1)  // NV_ENC_RECONFIGURE_PARAMS_VER from SDK
}
```

The exact version number comes from SDK header `nvEncodeAPI.h` macro `NV_ENC_RECONFIGURE_PARAMS_VER` — typically `NVENCAPI_STRUCT_VERSION(1)`.

### 3.8 InitParams accessor (open question Q4)

`InitParams` is the Rust newtype wrapping `ffi::NV_ENC_INITIALIZE_PARAMS`. T0 verifies whether it exposes:
- A `Copy` derive (needed to clone for L4)
- A way to mutate the embedded `encodeConfig` (the `NV_ENC_CONFIG` it points to)
- An `into_inner() -> ffi::NV_ENC_INITIALIZE_PARAMS` accessor

If `Copy` is missing, add it (the underlying FFI struct is plain old data with no pointers we own). If the encode_config is allocated separately and reached by pointer, the L4 modification path needs a guarded mut accessor (`encode_config_mut()`).

---

## 4. Smoke recipe (`scripts/l4-netem-smoke.sh`)

L3 smoke could not demonstrate MD because the environmental loss was zero. L4 introduces a controlled-loss harness using Linux `tc qdisc netem`.

### Script

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
set -euo pipefail
ACTION="${1:?usage: $0 <add|del|status> <iface> [loss_pct]}"
IFACE="${2:?missing iface (e.g. eth0)}"
case "$ACTION" in
  add)
    LOSS="${3:?missing loss percent (e.g. 5)}"
    tc qdisc add dev "$IFACE" root netem loss "${LOSS}%"
    echo "netem added: $IFACE loss ${LOSS}%"
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

Permissions: `chmod +x scripts/l4-netem-smoke.sh`. Requires `sudo` because `tc qdisc` modifies kernel netfilter state.

### Smoke procedure

| Step | Duration | Action | Expected observation |
|---|---|---|---|
| 1 | — | Start host: `RUST_LOG=info prdt host --bind 0.0.0.0:9000 --bitrate-mbps 30 --encoder openh264 --silent-allow` | host listens, pubkey printed |
| 2 | — | Start viewer: `RUST_LOG=info prdt connect --host <wsl-ip>:9000 --host-pubkey ... --codec h264 --decoder openh264 2>&1 \| tee /tmp/prdt-viewer-l4.log` | handshake complete, frames flowing |
| 3 | 30 s | (no action — baseline) | `frames_received` climbing, no `SetBitrate` log |
| 4 | — | `sudo ./scripts/l4-netem-smoke.sh add eth0 5` | netem confirmed added |
| 5 | 30 s | (loss inject active) | viewer: `L3 sent SetBitrate target_bps=N` repeats with N descending 30M → 21M → 14M → 10M → 7M → 5M (or further). Host: `viewer requested bitrate change target_bps=N` matching, `OpenH264 set_option(BITRATE)` not warning, `host tx stats` `bytes_sent`/`fps_sent` reflecting smaller frames |
| 6 | — | `sudo ./scripts/l4-netem-smoke.sh del eth0` | netem removed |
| 7 | 60 s | (recovery) | viewer: `target_bps` rises by ~200 kbps every second, host bytes track upward |
| 8 | — | viewer Ctrl+C, `pkill -f prdt host` | clean shutdown, watchdog kill is normal |

### DoD #1 success criteria

- Viewer log contains at least 3 distinct `target_bps` values during the loss-inject window, with the lowest ≤ 5_000_000
- Host log contains matching `viewer requested bitrate change` for each
- Host `host tx stats bytes_sent / 1s` window during loss = ≤ 50% of baseline window (encoder actually shrunk frames)
- During recovery, `target_bps` increases monotonically for at least 5 ticks
- No `host watchdog … session kill` for 5+ minutes total session

---

## 5. Testing

### A. OpenH264 unit test (`crates/media-sw/src/encoder.rs` `#[cfg(test)] mod tests`) — 1 new test

`set_target_bitrate_runtime_changes_emitted_size`:
- Build encoder with `target_bitrate_bps = 30_000_000`, 1920×1080
- Encode 60 frames of a constant grey I420, accumulate total emitted size, average over 60 → `hi_avg`
- Call `enc.set_target_bitrate(2_000_000)`
- Encode 60 more frames, average → `lo_avg`
- Assert `lo_avg < hi_avg * 70 / 100`

The 30% margin accommodates OpenH264's gradual rate-control convergence (a few frames of overshoot are normal after a step change).

### B. NVENC integration test (`crates/media-win/src/nvenc/encoder.rs` `#[cfg(test)] mod tests`) — 1 new test, gated `#[cfg(prdt_nvenc_bindings)] #[ignore]`

`nvenc_set_target_bitrate_changes_emitted_size`:
- Same shape as A but using `D3d11Device::create_default()` and `D3d11Texture::new_default(&dev, 1920, 1080, TextureFormat::Bgra8)`
- Identical assertion

Windows CI runs with `cargo test ... -- --ignored` so this fires when SDK + GPU are present.

### C. Existing regression bar

- `cargo build --workspace --target x86_64-unknown-linux-gnu --all-targets` clean
- `cargo clippy --workspace --target x86_64-unknown-linux-gnu --all-targets -- -D warnings` clean
- `cargo test --workspace --target x86_64-unknown-linux-gnu` = 366 baseline + 1 new = **367 passed** (excl pre-existing flaky `transport::probe_test::two_transports_find_each_other`)
- Windows CI green (PR-driven)

### D. Manual smoke (DoD #1) per §4

---

## 6. Open Questions for Plan Writer (T0 resolves)

### Q1: NVENC `init_params` storage
Does `NvencEncoder` (`crates/media-win/src/nvenc/encoder.rs:65-100`) have an `init_params: InitParams` field today? If not, T1 adds it (assigned at the end of `new()` after `nvEncInitializeEncoder` succeeds). No Drop impact.

### Q2: `nvEncReconfigureEncoder` bindgen visibility
Read `OUT_DIR/nvenc_bindings.rs` (Windows build) or `cargo expand --target x86_64-pc-windows-msvc -p prdt-media-win`. Confirm the function pointer field exists in `NV_ENCODE_API_FUNCTION_LIST`. If absent, add `allowlist_function("nvEncReconfigure.*")` to `crates/media-win/build.rs:67`.

### Q3: `nv_enc_reconfigure_params_ver()` value
Add a new helper in `crates/media-win/src/nvenc/config.rs` mirroring existing `nv_enc_pic_params_ver()` etc. Look up `NV_ENC_RECONFIGURE_PARAMS_VER` in `nvEncodeAPI.h` (typically `NVENCAPI_STRUCT_VERSION(1)`).

### Q4: `InitParams` Copy + mutator surface
Read the existing `InitParams` Rust newtype definition (likely in `crates/media-win/src/nvenc/config.rs`). Verify or add: `#[derive(Copy, Clone)]`, `pub fn encode_config_mut(&mut self) -> &mut ffi::NV_ENC_CONFIG`, `pub fn into_inner(self) -> ffi::NV_ENC_INITIALIZE_PARAMS`. The `encode_config` may be reached via raw pointer (NV_ENC sets `encodeConfig: *mut NV_ENC_CONFIG`) — guard the mut accessor with the lifetime of `&mut self` to avoid undefined behaviour.

### Q5: OpenH264 `Encoder::raw_api()` on stable
`pub const unsafe fn raw_api(&mut self) -> &mut EncoderRawAPI` exists at `~/.cargo/registry/src/.../openh264-0.9.3/src/encoder.rs:1062` (verified). Confirm the `openh264-sys2` re-exports `ENCODER_OPTION_BITRATE`, `SBitrateInfo`, `SPATIAL_LAYER_ALL` at the public crate root or via `openh264_sys2::generated::types::*`. May need `use openh264_sys2 as ohsys;` then `ohsys::ENCODER_OPTION_BITRATE`.

---

## 7. Implementation Task Skeleton

| Task | Files | LoC | TDD |
|---|---|---|---|
| T0 | baseline + Q1–Q5 resolved | 0 | research only |
| T1 | OpenH264 reconfigure body + `openh264-sys2` dep + 1 unit test | ~80 | yes |
| T2 | NVENC `InitParams` field on `NvencEncoder` (Q1) + Q4 accessors | ~50 | indirect (compile only) |
| T3 | NVENC reconfigure body + `nv_enc_reconfigure_params_ver()` helper + gated `#[ignore]` test | ~120 | yes (gated) |
| T4 | `scripts/l4-netem-smoke.sh` + chmod | ~50 | manual |
| T5 | workspace build/clippy/test sweep + draft PR | 0 | manual |
| T6 | Linux Wayland smoke walkthrough (DoD #1) per §4 | 0 | manual |
| T7 | STATUS update + tag | ~20 (STATUS only) | manual |

Total: ~320 LoC src + ~50 script + ~20 docs = **~400 LoC**, 7 tasks (T0–T7).

---

## 8. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| `nvEncReconfigureEncoder` not in bindgen output → compile fail on Windows | T0/Q2 verifies via OUT_DIR or `cargo expand` before T3 starts. If missing, add `allowlist_function("nvEncReconfigure.*")` to `build.rs`. |
| `InitParams` not Copy → T2 needs unsafe field-by-field clone | T0/Q4 inspects the type. If non-Copy due to internal pointer, design a safe `clone_for_reconfigure(&self) -> InitParams` instead. Falls back to manual struct copy if needed. |
| OpenH264 `set_option(ENCODER_OPTION_BITRATE)` returns non-zero on the source-built variant (some SDK builds hard-disable runtime reconfigure) | The trait method already logs and continues — the warn is the user-facing signal. T1 unit test will catch this on Linux x86_64 release builds. If broken, fall back to encoder reinit (expensive but correct) gated on prior failure. |
| NVENC `forceIDR=1` causes a visible quality dip + large IDR fragment | Acceptable trade-off — the alternative (waiting for natural IDR up to 1 second) means viewer keeps decoding wrong-bitrate P-frames and the controller's response is invisible. Quality dip from the IDR is bounded by encoder rate-control. |
| `tc qdisc netem` unavailable on user's WSLg (`iproute2` package or NET_ADMIN cap missing) | Document the alternative: physical WiFi attenuation (move device far from AP). Script includes a `status` action so user can verify before running. |
| Auto-mode classifier blocks `sudo tc` | Surface via `!` prefix to the user (same workaround as L1.5b/L2 0.0.0.0:9000 bind and L3 PR merge). Wrap in `scripts/l4-netem-smoke.sh` to make it a single command. |
| Smoke shows controller MD but encoder doesn't actually shrink frames (encoder reconfig silently no-ops) | The `host tx stats bytes_sent / 1s` is the ground-truth signal — if it doesn't drop with `target_bps`, encoder reconfig is broken. Write the expected ratio (≤50%) into DoD #1 explicitly. |
| MF backend is now the only stub left and may surprise a future user (`--encoder mf` + adaptive bitrate doesn't react) | Update `--encoder` help text to note that adaptive bitrate is OpenH264/NVENC only on this build. STATUS L4 entry calls out MF as L5 candidate prominently. |

---

## 9. Observability changes

L3 introduced these log lines; L4 supplements:

| When | Module | Level | Message | New @ L4? |
|---|---|---|---|---|
| Viewer detects loss | `prdt_viewer::latency_task` | info | `L3 sent SetBitrate target_bps=N` | (existing) |
| Host control loop | `prdt_host::lib` | info | `viewer requested bitrate change target_bps=N` (or `clamping`) | (existing, clamp added in L3 final fix) |
| Host video loop | `prdt_host::lib` | debug | `applied viewer-requested bitrate target_bps=N` | (existing, downgraded to debug in L3 final fix) |
| **NEW @ L4** OpenH264 success | `prdt_media_sw::encoder` | (no log on success) | — | implicit |
| **NEW @ L4** OpenH264 failure | `prdt_media_sw::encoder` | warn | `OpenH264 set_option(BITRATE) failed rc=N requested_bps=B` | yes |
| **NEW @ L4** NVENC success | `prdt_media_win::nvenc::encoder` | info | `NVENC bitrate reconfigured target_bps=N` | yes |
| **NEW @ L4** NVENC failure | `prdt_media_win::nvenc::encoder` | warn | `NVENC nvEncReconfigureEncoder failed status=S requested_bps=B` | yes |

The OpenH264 success path is silent (controller-side `L3 sent SetBitrate` is enough) to keep log volume bounded under repeated reconfigures.
