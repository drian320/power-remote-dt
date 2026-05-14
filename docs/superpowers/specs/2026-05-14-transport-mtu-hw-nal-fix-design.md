# Transport MTU vs HW-encoded NAL fix — Dynamic-k FEC

**Status**: design — `phase-transport-mtu-hw-nal-fix`
**Spec date**: 2026-05-14
**Predecessor**: P5C-1 (`1a94809`) + P5B-2a-successor (`94ac04f`) on master
**Trigger**: N100 GNOME 46 Wayland smoke 2026-05-13 — VAAPI 168 KB IDR exceeds `max_bytes=76,800`, viewer renders black (decode 9 %)

## 1. Goal

Deliver VAAPI-encoded H.264 frames (observed 168 KB IDR at 1080p 5 Mbps on
Intel iHD) through the existing UDP transport so the N100 / Ubuntu 24.04
GNOME 46 Wayland end-to-end smoke renders live desktop content in the
viewer.

**Success criteria** (manual smoke):
- `frames_received / textures_decoded ≥ 90 %` (was 9 % before fix)
- viewer window shows the N100's live desktop content
- host log shows zero or only sporadic `send_video error; continuing`
- host CPU under VAAPI + transport: < 15 % at 1080p60 5 Mbps (record actual)

**Out of scope**

- VAAPI-side encoder tuning (`VAEncMiscParameterMaxFrameSize`, slice
  count): deferred. The transport-layer fix here is sufficient.
- L3 AIMD bitrate controller behavior under high parity overhead:
  deferred unless smoke reveals oscillation.
- Adaptive parity ratio based on observed loss: deferred. 10 % static
  parity is the start.
- Receiver-side `FrameAssembler` changes: none needed — the assembler
  already reads `source_chunks` / `parity_chunks` from each packet
  header, so dynamic-k is fully wire-compatible.
- Multi-compositor smoke (KDE / Sway / Hyprland): deferred to P5C-3.

## 2. Why dynamic-k

The transport's current production config (`crates/transport/src/udp.rs:99-100`):

```rust
fec_k: 64
fec_m: 6
// → max bytes per frame = k × chunk_payload_len = 64 × 1200 = 76,800
```

This was tuned for OpenH264 SW encoder output. VAAPI on Intel iHD
produces ~30× larger IDR frames at the same target bitrate. Raising
`fec_k` statically (e.g. to 150) would solve the size limit but
introduce a different problem: `packetize()` always emits exactly `k`
source shards regardless of actual frame size, so a 100-byte P-frame
would burn 150 packets × ~1226 bytes = ~180 KB on wire for a single
small frame. At 60 fps that's ~10.8 MB/sec network overhead even on
a static screen.

Dynamic-k computes `k = bytes.div_ceil(chunk_payload_len)` per frame,
producing the **minimum** number of source shards. Combined with
proportional `m` (10 % of `k`, floor 1), small P-frames send 2
packets and large IDRs send up to ~154 packets, with no wasted
bandwidth at either extreme.

Reed-Solomon GF(8) builder cost (`reed-solomon-erasure`) is
microseconds for k ≤ 200, so per-frame instantiation is acceptable
versus the existing one-shot construction.

## 3. Architecture

### 3.1 `packetize()` signature change

Current (`crates/transport/src/packetize.rs:21-25`):

```rust
pub fn packetize(
    frame: &EncodedFrame,
    fec: &FecCodec,            // ← built once with fixed k, m
    chunk_payload_len: usize,
) -> Result<Vec<VideoPacket>, TransportError>
```

New:

```rust
pub fn packetize(
    frame: &EncodedFrame,
    chunk_payload_len: usize,
    fec_policy: &FecPolicy,    // ← caps + parity ratio
) -> Result<Vec<VideoPacket>, TransportError>

pub struct FecPolicy {
    pub max_k: usize,           // ceiling on k (default 200)
    pub max_m: usize,           // ceiling on m (default 20)
    pub parity_ratio_pct: u32,  // m = max(min_m, k * pct / 100) (default 10)
    pub min_m: usize,           // floor on m (default 1)
}
```

Body becomes:

```rust
pub fn packetize(frame, chunk_payload_len, policy) -> Result<...> {
    let bytes = frame.nal_units.len();
    let k = bytes.div_ceil(chunk_payload_len);
    if k > MAX_SOURCE_CHUNKS { return Err(FrameTooLarge { ... }); }
    if k > policy.max_k       { return Err(FrameTooLarge { ... }); }
    let k = k.max(1);           // even 0-byte frames get one shard
    let m_raw = (k * policy.parity_ratio_pct as usize).div_ceil(100);
    let m = m_raw.max(policy.min_m).min(policy.max_m);
    let fec = FecCodec::new(k, m)?;  // per-frame instantiation
    // ... rest unchanged (shard build + parity + emit k+m VideoPackets)
}
```

### 3.2 `MAX_SOURCE_CHUNKS` raise

`crates/transport/src/packetize.rs:12`: `128 → 200`.
New ceiling: 200 × 1200 = 240,000 bytes per frame.
Reed-Solomon GF(8) constraint: `k + m ≤ 255`. With max_m=20, max k+m =
220 ≤ 255 ✓.

The existing comment notes this constant is the architectural cap;
update the comment to reflect the new value and the GF(8) headroom.

### 3.3 `UdpTransportConfig` changes

`crates/transport/src/udp.rs:82-101`:

Old fields:
```rust
pub chunk_payload_len: usize,    // 1200 (unchanged)
pub fec_k: usize,                // 64 (= static k)
pub fec_m: usize,                // 6  (= static m)
```

New fields:
```rust
pub chunk_payload_len: usize,    // 1200 (unchanged)
pub fec_policy: FecPolicy,       // dynamic-k cap + ratio
```

Where `FecPolicy::default()`:
```rust
FecPolicy {
    max_k: 200,
    max_m: 20,
    parity_ratio_pct: 10,
    min_m: 1,
}
```

The old `self.fec: FecCodec` field on `UdpTransport` (currently a
single long-lived codec instance) is removed — `packetize()` now
constructs the codec per call. The transport keeps only `fec_policy`.

Callers of `UdpTransport::bind` / `UdpTransport::with_socket` that
explicitly set `fec_k: 4, fec_m: 2` for test scenarios need to migrate
to `fec_policy: FecPolicy { max_k: 4, max_m: 2, parity_ratio_pct: 50,
min_m: 2 }` (or similar — match the existing test intent).

### 3.4 Wire compatibility

`VideoPacket::source_chunks` and `parity_chunks` (`u16` each, per-packet)
already carry the values dynamically. The receiver-side `FrameAssembler`
reads these from each packet header and adapts. No protocol change, no
breaking on existing viewer binaries.

### 3.5 Error handling

`FrameTooLarge` flow unchanged:
- `packetize()` returns `Err(FrameTooLarge { bytes, max_bytes })`
- host video task logs `send_video error; continuing` and drops the
  frame
- viewer's `IdrRequester` (L2) sees a gap, requests IDR
- host re-encodes a fresh IDR on next frame

The new ceiling (200 × 1200 = 240 KB) is comfortably above the worst
case we expect (168 KB observed; even 4K60 at 30 Mbps is < 200 KB per
NAL typically). Frames above 240 KB indicate a misconfigured encoder
(e.g. encoder ran without bitrate clamp) — the IDR-request loop will
keep failing in a tight loop until either the encoder produces a
smaller frame or the operator notices via logs. Acceptable for now.

## 4. Components & files

| File | Change | LoC estimate |
|---|---|---|
| `crates/transport/src/packetize.rs` | Replace signature, add `FecPolicy`, compute dynamic k/m, instantiate FecCodec per call. Raise `MAX_SOURCE_CHUNKS`. | ~80 |
| `crates/transport/src/udp.rs` | `UdpTransportConfig` field swap (`fec_k`/`fec_m` → `fec_policy`). Drop `self.fec`. Update `send_video` call site. | ~30 |
| `crates/transport/src/lib.rs` | Re-export `FecPolicy`. | +2 |
| `crates/transport/src/idr_loss_test.rs` | Migrate `FecCodec::new(4,2)` test setups to `FecPolicy`. Add `large_idr_round_trip` + `large_idr_with_loss_recovery`. | ~60 |
| `crates/transport/src/assembler.rs` | Migrate test-only `FecCodec::new(4,2)` to use the packetize wrapper. No production change. | ~20 |
| `crates/transport/src/packetize.rs::tests` | 5 new unit tests (tiny / medium / IDR / oversize / parity-ratio-floor). Update or remove `packetize_rejects_oversize` to use new threshold. | ~80 |
| `docs/superpowers/p5b1-smoke-walkthrough.md` | + §M (N100 transport MTU fix smoke). | ~70 |
| `docs/superpowers/STATUS.md` | + entry, link from P5B-2a-successor §"Out of scope" Resolution. | ~25 |

Total: ~370 LoC + docs, ~2-3 days estimate.

## 5. Testing

### 5.1 Unit tests (packetize.rs)

1. `packetize_tiny_frame_uses_minimal_k` — 100-byte frame → k=1, m=1,
   2 packets total. Assert per-packet `source_chunks=1, parity_chunks=1`.
2. `packetize_medium_frame_scales_k` — 5,000-byte frame → k=5, m=1, 6
   packets.
3. `packetize_idr_frame_scales_to_high_k` — 168,000-byte frame →
   k=140, m=14, 154 packets.
4. `packetize_oversize_rejects_at_max_chunks` — 250,000-byte frame →
   `FrameTooLarge { bytes: 250000, max_bytes: 240000 }`.
5. `packetize_parity_ratio_minimum_one` — 100-byte frame (k=1) → m=1
   (the `max(min_m, …)` floor), not 0.

### 5.2 Integration tests (idr_loss_test.rs)

1. Existing `wsl_idr_loss_test` — migrate to new signature; verify
   semantics unchanged at the small frame size it uses.
2. `large_idr_round_trip` — synthetic 180 KB encoded frame, 0 % loss,
   verify `FrameAssembler` reconstructs the original byte exactly.
3. `large_idr_with_loss_recovery` — 180 KB IDR, drop 5 random packets,
   FEC recovers (m = 18 at k=180 covers any 5 losses).

### 5.3 Real-device smoke (manual, walkthrough §M)

Pre-conditions identical to §L (N100 + GNOME 46 Wayland + VAAPI
backend). Steps:

1. host: `./prdt host --encoder vaapi --bitrate-mbps 5 --silent-allow`
2. viewer: `./prdt connect ...`
3. After ~10 seconds of streaming, capture:
   - host log: `frames_sent / send_errors / bytes_sent_window` per
     `viewer rx stats` analogue (existing telemetry from L4)
   - viewer log: `frames_received / textures_decoded / recv_errors /
     timeouts` from the existing rx stats line
   - **target**: `textures_decoded / frames_received ≥ 90 %`
4. `pidstat -p $(pgrep -f "prdt host") 1 30` — record average %CPU.
5. Cleanup: viewer Ctrl+C → host idles → reconnect verifies clean
   re-session.

If `textures_decoded < 90 %` even after the fix:
- Check `recv_errors` for UDP loss (LAN should be ~0)
- Check host log for any residual `send_video error` (should be 0)
- Suspect a bug in the dynamic-k packetize path → diagnostic phase

### 5.4 Cross-platform CI bar

- Linux container clippy + Windows clippy both green
- Workspace `cargo test --workspace` clean (excluding the 12
  pre-existing `auth_integration` failures from the P6 protocol bump)
- All wayland_portal tests still pass (44/44 from previous merge)

## 6. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Per-frame `FecCodec::new(k, m)` cost shows up in tail latency at high frame rates | low | RS GF(8) builder is microseconds; benchmark in a unit test if needed. The cost was hidden inside one long-lived instance before; now per-frame, but k/m are small. |
| Existing tests with `FecCodec::new(4, 2)` migration breaks semantics | medium | Touch each test, verify the new `FecPolicy` produces the same packet count when fed the same frame size. |
| L3 AIMD bitrate controller oscillates because parity ratio is now proportional (more bandwidth for bigger frames) | low | Static 10 % parity is what we have today (m=6 / k=64 ≈ 9.4 %), so behavior is similar. Only large frames see more parity. |
| Some IDR is > 240 KB on weird content | very low | `FrameTooLarge` + IDR-request loop is the existing degraded-mode path. Doesn't break correctness. |

## 7. Open questions

None at spec time. The adaptive parity question was deferred per the
brainstorming gate (固定 10 %).

## 8. DoD checklist

- [ ] `FecPolicy` struct + `packetize()` new signature
- [ ] `MAX_SOURCE_CHUNKS = 200`
- [ ] `UdpTransportConfig` migration (fec_k/fec_m → fec_policy)
- [ ] 5 new packetize unit tests + 2 new integration tests
- [ ] All existing transport tests pass after migration
- [ ] Workspace clippy + tests clean (Linux + Windows)
- [ ] N100 GNOME 46 smoke: `textures_decoded ≥ 90 % of frames_received`
- [ ] Viewer renders live desktop content
- [ ] Host CPU under VAAPI < 15 % at 1080p60 5 Mbps (record actual)
- [ ] Walkthrough §M added; STATUS.md updated
- [ ] PR merged to master

## 9. Follow-ups (out of scope)

- `VAEncMiscParameterMaxFrameSize` cap on encoder side (P5C-2 or
  separate). Keeps NAL sizes predictable even on weird content.
- Adaptive parity ratio based on observed loss %. Combines with L3
  AIMD for better recovery in lossy conditions.
- Per-receiver buffer budget reporting (e.g. assembler memory
  high-water mark in viewer rx stats).
- Multi-compositor smoke (P5C-3).
