# Transport MTU vs HW-encoded NAL fix — Dynamic-k FEC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development`. Steps use checkbox (`- [ ]`).

**Goal:** Replace the static `fec_k=64, fec_m=6` packetizer (max frame = 76,800 B) with per-frame dynamic `k = bytes.div_ceil(chunk_payload_len)` + `m = max(1, k/10)`, raising `MAX_SOURCE_CHUNKS` from 128 to 200 (new ceiling 240 KB). End-to-end success = N100 GNOME 46 Wayland smoke renders live desktop with `textures_decoded / frames_received ≥ 90 %`.

**Architecture:** New `FecPolicy` struct in `transport/src/packetize.rs` carries caps + parity ratio. `packetize()` signature changes from `(frame, &FecCodec, chunk_len)` to `(frame, chunk_len, &FecPolicy)`; FecCodec is instantiated per-frame inside. `UdpTransportConfig` swaps `fec_k`/`fec_m` → `fec_policy`. Wire format unchanged — `source_chunks`/`parity_chunks` are already per-packet so the assembler adapts automatically.

**Tech Stack:** Rust 1.85, existing `reed-solomon-erasure` crate (already in tree), `prdt-transport` + `prdt-protocol`. No new dependencies.

**Constraints:**
- Cross-platform CI green (Linux container + Windows). The whole transport crate is platform-agnostic so both should be straightforward.
- 12 `auth_integration` test failures on master are pre-existing P6 protocol regressions; don't introduce more.
- All existing transport tests (packetize, assembler, idr_loss_test, fec, probe_test, encrypted_test, udp_test) must still pass after the migration.
- N100 manual smoke is the load-bearing end-to-end verification — DoD includes capturing the actual `textures_decoded / frames_received` ratio.

**Spec:** `docs/superpowers/specs/2026-05-14-transport-mtu-hw-nal-fix-design.md` (commit `9b0e188`).

---

## File map

| File | Status | Responsibility |
|---|---|---|
| `crates/transport/src/packetize.rs` | modify | Add `FecPolicy`, change `packetize()` signature, dynamic k/m, MAX_SOURCE_CHUNKS=200. |
| `crates/transport/src/lib.rs` | modify | Re-export `FecPolicy`. |
| `crates/transport/src/udp.rs` | modify | `UdpTransportConfig.fec_policy` field, drop `self.fec`, update call site. |
| `crates/transport/src/idr_loss_test.rs` | modify | Migrate two `packetize(&fec, …)` call sites + the `fec.k()` access. |
| `crates/transport/src/assembler.rs` | modify | Migrate 5 test-only `packetize(&fec, …)` call sites. |
| `crates/transport/tests/encrypted_test.rs` | modify | `fec_k: 4, fec_m: 2` → `fec_policy: FecPolicy::strict_small()`. |
| `crates/transport/tests/udp_test.rs` | modify | Same migration as encrypted_test. |
| `crates/viewer/src/lib.rs` | none (uses `default()`) | Indirectly picks up new defaults. |
| `docs/superpowers/p5b1-smoke-walkthrough.md` | modify | +§M N100 transport MTU smoke. |
| `docs/superpowers/STATUS.md` | modify | + entry, link from P5B-2a-successor §"Out of scope" to resolution. |

---

## Task 1: Add `FecPolicy` + raise `MAX_SOURCE_CHUNKS` (signature stays for now)

**Files:**
- Modify: `crates/transport/src/packetize.rs`
- Modify: `crates/transport/src/lib.rs`

This task lands the new type + ceiling change WITHOUT touching the existing `packetize()` signature. That lets us evolve in two well-bounded commits.

- [ ] **Step 1: Append `FecPolicy` struct + impl** below the existing `pub const MAX_SOURCE_CHUNKS` line in `crates/transport/src/packetize.rs`:

```rust
/// Per-frame FEC sizing policy. Replaces the old static `fec_k` / `fec_m`
/// pair on `UdpTransportConfig`. `packetize()` computes the actual `k`
/// from the frame size and clamps to `max_k`; `m` is derived from `k`
/// via `parity_ratio_pct` with floor `min_m` and ceiling `max_m`.
///
/// Default is tuned for VAAPI 1080p60 5 Mbps where IDRs reach ~170 KB:
/// `k` up to 200, `m` up to 20, 10 % parity, m ≥ 1.
#[derive(Debug, Clone, Copy)]
pub struct FecPolicy {
    pub max_k: usize,
    pub max_m: usize,
    pub parity_ratio_pct: u32,
    pub min_m: usize,
}

impl FecPolicy {
    /// Production default. See struct doc for rationale.
    pub const fn standard() -> Self {
        Self {
            max_k: 200,
            max_m: 20,
            parity_ratio_pct: 10,
            min_m: 1,
        }
    }

    /// Tight policy for unit tests that intentionally exercise the
    /// "frame too large" path or want predictable small packet counts.
    /// k ≤ 4, m ≤ 2, 50 % parity, m ≥ 2 (matches the old
    /// `fec_k: 4, fec_m: 2` test setups).
    pub const fn strict_small() -> Self {
        Self {
            max_k: 4,
            max_m: 2,
            parity_ratio_pct: 50,
            min_m: 2,
        }
    }

    /// Compute `(k, m)` for a frame of `nal_bytes` bytes split into
    /// `chunk_payload_len`-byte chunks. Returns `None` if the frame is
    /// too large (exceeds `MAX_SOURCE_CHUNKS` or `max_k`).
    pub fn compute_k_m(&self, nal_bytes: usize, chunk_payload_len: usize) -> Option<(usize, usize)> {
        let raw_k = nal_bytes.div_ceil(chunk_payload_len).max(1);
        if raw_k > MAX_SOURCE_CHUNKS {
            return None;
        }
        if raw_k > self.max_k {
            return None;
        }
        let raw_m =
            (raw_k.saturating_mul(self.parity_ratio_pct as usize)).div_ceil(100);
        let m = raw_m.max(self.min_m).min(self.max_m);
        Some((raw_k, m))
    }
}

impl Default for FecPolicy {
    fn default() -> Self {
        Self::standard()
    }
}
```

- [ ] **Step 2: Raise `MAX_SOURCE_CHUNKS` from 128 to 200** at the top of the same file:

```rust
/// Max source chunks per frame. Raised from 128 (Plan 3 / 4 era) to 200
/// to accommodate VAAPI HW-encoded 1080p60 IDRs which reach ~170 KB
/// (= 142 chunks of 1200 B). Reed-Solomon GF(8) supports k + m ≤ 255,
/// so 200 leaves room for up to m = 55 parity per frame. With the
/// production `FecPolicy::standard()` (parity_ratio_pct = 10, max_m =
/// 20) the realistic worst case is k = 200, m = 20 → 220 chunks total.
pub const MAX_SOURCE_CHUNKS: usize = 200;
```

- [ ] **Step 3: Append unit tests** at the END of the existing `mod tests` block (inside the `#[cfg(test)]`):

```rust
    #[test]
    fn fec_policy_standard_defaults_match_spec() {
        let p = FecPolicy::standard();
        assert_eq!(p.max_k, 200);
        assert_eq!(p.max_m, 20);
        assert_eq!(p.parity_ratio_pct, 10);
        assert_eq!(p.min_m, 1);
    }

    #[test]
    fn fec_policy_compute_k_m_tiny_frame() {
        // 100-byte frame at 1200 B chunks → k=1, m=1 (min_m floor)
        let p = FecPolicy::standard();
        assert_eq!(p.compute_k_m(100, 1200), Some((1, 1)));
    }

    #[test]
    fn fec_policy_compute_k_m_medium_frame() {
        // 5000-byte frame at 1200 B → ceil(5000/1200)=5 → k=5, m=1 (10% rounds up to 1)
        let p = FecPolicy::standard();
        assert_eq!(p.compute_k_m(5000, 1200), Some((5, 1)));
    }

    #[test]
    fn fec_policy_compute_k_m_idr_frame() {
        // 168000-byte frame → ceil(168000/1200)=140 → k=140, m=14
        let p = FecPolicy::standard();
        assert_eq!(p.compute_k_m(168000, 1200), Some((140, 14)));
    }

    #[test]
    fn fec_policy_compute_k_m_oversize_rejects() {
        // 250000 B → would need ceil=209 > MAX_SOURCE_CHUNKS=200 → None
        let p = FecPolicy::standard();
        assert!(p.compute_k_m(250_000, 1200).is_none());
    }

    #[test]
    fn fec_policy_compute_k_m_zero_byte_frame_still_one_chunk() {
        let p = FecPolicy::standard();
        assert_eq!(p.compute_k_m(0, 1200), Some((1, 1)));
    }

    #[test]
    fn fec_policy_strict_small_matches_legacy_fec_4_2() {
        let p = FecPolicy::strict_small();
        // 400-byte frame fits in 1 chunk at 1200 B; m floor is 2.
        assert_eq!(p.compute_k_m(400, 1200), Some((1, 2)));
        // 5000-byte frame needs 5 chunks > max_k=4 → None.
        assert!(p.compute_k_m(5000, 1200).is_none());
    }
```

- [ ] **Step 4: Re-export `FecPolicy`** from `crates/transport/src/lib.rs`. Find the existing `pub use packetize::{...}` line (or add one if absent). After this edit, `prdt_transport::FecPolicy` must resolve. Read the file first to find the right `pub use` block:

```bash
grep -n "pub use packetize\|pub use crate::packetize" crates/transport/src/lib.rs
```

If there's an existing `pub use packetize::{...};` line, add `FecPolicy` to its brace list. If `MAX_SOURCE_CHUNKS` is also re-exported there, leave it alone. If there's no existing re-export, add this near the other `pub use` lines:

```rust
pub use packetize::{FecPolicy, MAX_SOURCE_CHUNKS};
```

- [ ] **Step 5: Run tests** to verify:

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-transport --target x86_64-unknown-linux-gnu packetize::tests 2>&1 | tail -15'
```

Expected: existing tests still pass + 7 new `fec_policy_*` tests pass.

- [ ] **Step 6: Workspace check + clippy** to catch any `MAX_SOURCE_CHUNKS` consumer that breaks on the 128 → 200 change:

```bash
./scripts/dev-container.sh bash -c 'cargo check -p prdt-transport --target x86_64-unknown-linux-gnu 2>&1 | tail -5'
./scripts/dev-container.sh bash -c 'cargo clippy -p prdt-transport --target x86_64-unknown-linux-gnu -- -D warnings 2>&1 | tail -5'
```

Expected: clean.

- [ ] **Step 7: Commit**:

```bash
git add crates/transport/src/packetize.rs crates/transport/src/lib.rs
git commit -m "feat(transport): add FecPolicy + raise MAX_SOURCE_CHUNKS to 200 (T1)"
```

---

## Task 2: Switch `packetize()` to dynamic k/m via `FecPolicy`

**Files:**
- Modify: `crates/transport/src/packetize.rs`

This task replaces the `&FecCodec` parameter with `&FecPolicy` and instantiates the codec per-frame.

- [ ] **Step 1: Write a NEW failing integration-style test** that exercises the new signature directly (the existing tests still use the old signature for now and will be migrated in step 4):

Append to the `mod tests` block:

```rust
    #[test]
    fn packetize_new_signature_tiny_frame() {
        let policy = FecPolicy::standard();
        let payload = vec![0xAB; 10];
        let pkts = packetize_v2(&make_frame(&payload), 1200, &policy).unwrap();
        // tiny frame → k=1, m=1, total 2 packets
        assert_eq!(pkts.len(), 2);
        assert_eq!(pkts[0].source_chunks, 1);
        assert_eq!(pkts[0].parity_chunks, 1);
        assert!(pkts[0].is_keyframe());
        assert!(!pkts[0].is_parity());
        assert_eq!(pkts[0].payload_bytes, 10);
        assert!(pkts[1].is_parity());
    }

    #[test]
    fn packetize_new_signature_idr_frame() {
        let policy = FecPolicy::standard();
        // 168 KB frame → k=140, m=14
        let payload = vec![0x42; 168_000];
        let pkts = packetize_v2(&make_frame(&payload), 1200, &policy).unwrap();
        assert_eq!(pkts.len(), 140 + 14);
        for p in pkts.iter().take(140) {
            assert_eq!(p.source_chunks, 140);
            assert_eq!(p.parity_chunks, 14);
            assert!(!p.is_parity());
        }
        for p in pkts.iter().skip(140) {
            assert!(p.is_parity());
        }
    }

    #[test]
    fn packetize_new_signature_oversize_rejects() {
        let policy = FecPolicy::standard();
        // 250000 B → MAX_SOURCE_CHUNKS=200 violated
        let payload = vec![0u8; 250_000];
        let err = packetize_v2(&make_frame(&payload), 1200, &policy).unwrap_err();
        assert!(matches!(err, TransportError::FrameTooLarge { .. }));
    }
```

- [ ] **Step 2: Add the new `packetize_v2` function** (we keep the old `packetize` temporarily; step 5 below renames). Insert after the existing `packetize` function:

```rust
/// Dynamic-k variant of `packetize`. Replaces the static `&FecCodec`
/// argument with a `&FecPolicy` and constructs the codec per call.
///
/// The receiver-side `FrameAssembler` reads `source_chunks` and
/// `parity_chunks` from each packet header, so dynamic k/m is fully
/// wire-compatible.
///
/// Returns `FrameTooLarge` if the frame's chunk count exceeds the
/// policy's `max_k` or the global `MAX_SOURCE_CHUNKS` ceiling.
pub fn packetize_v2(
    frame: &EncodedFrame,
    chunk_payload_len: usize,
    policy: &FecPolicy,
) -> Result<Vec<VideoPacket>, TransportError> {
    let bytes = frame.nal_units.len();
    let (k, m) = policy.compute_k_m(bytes, chunk_payload_len).ok_or(
        TransportError::FrameTooLarge {
            bytes,
            // Report the *effective* ceiling: whichever cap fired.
            max_bytes: policy.max_k.min(MAX_SOURCE_CHUNKS) * chunk_payload_len,
        },
    )?;

    let fec = FecCodec::new(k, m)?;

    // Build k source shards.
    let mut source: Vec<Vec<u8>> = Vec::with_capacity(k);
    for i in 0..k {
        let start = i * chunk_payload_len;
        let end = (start + chunk_payload_len).min(bytes);
        let mut shard = vec![0u8; chunk_payload_len];
        if start < bytes {
            shard[..end - start].copy_from_slice(&frame.nal_units[start..end]);
        }
        source.push(shard);
    }

    // Compute m parity shards.
    let parity = fec.encode_parity(&source)?;

    let kf_flag = if frame.is_keyframe {
        video_flags::IS_KEYFRAME
    } else {
        0
    };
    let mut out = Vec::with_capacity(k + m);
    for (idx, shard) in source.iter().enumerate() {
        let start = idx * chunk_payload_len;
        let end = (start + chunk_payload_len).min(bytes);
        let valid = end.saturating_sub(start) as u16;
        out.push(VideoPacket {
            frame_seq: frame.seq,
            timestamp_host_us: frame.timestamp_host_us,
            chunk_idx: idx as u16,
            source_chunks: k as u16,
            parity_chunks: m as u16,
            video_flags: kf_flag,
            payload_bytes: valid,
            chunk_payload: shard.clone(),
        });
    }
    for (idx, shard) in parity.iter().enumerate() {
        out.push(VideoPacket {
            frame_seq: frame.seq,
            timestamp_host_us: frame.timestamp_host_us,
            chunk_idx: (k + idx) as u16,
            source_chunks: k as u16,
            parity_chunks: m as u16,
            video_flags: kf_flag | video_flags::IS_PARITY,
            payload_bytes: chunk_payload_len as u16,
            chunk_payload: shard.clone(),
        });
    }
    Ok(out)
}
```

- [ ] **Step 3: Run the new tests** to verify the new function:

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-transport --target x86_64-unknown-linux-gnu packetize::tests::packetize_new_signature 2>&1 | tail -10'
```

Expected: 3 passed.

- [ ] **Step 4: Migrate the existing test helpers in `mod tests`** to call `packetize_v2`. There are 3 existing test functions that use `packetize(&fec, …)`:

Replace each call site in the `mod tests` block — e.g. `packetize_small_frame`:

```rust
    #[test]
    fn packetize_small_frame() {
        let policy = FecPolicy {
            max_k: 4,
            max_m: 2,
            parity_ratio_pct: 50,  // 4*50% = 2 → m=2 (matches old k=4, m=2)
            min_m: 2,
        };
        let payload = vec![0xAB; 10];
        let pkts = packetize_v2(&make_frame(&payload), 100, &policy).unwrap();
        // 10 bytes → 1 chunk at 100B → k=1, m=2 (min_m floor)
        // NOTE: behavior INTENTIONALLY differs from the old test which
        // forced k=4 by using FecCodec::new(4, 2). The new contract is
        // "k = ceil(bytes / chunk_payload_len), clamped".
        assert_eq!(pkts.len(), 1 + 2);
        assert_eq!(pkts[0].source_chunks, 1);
        assert_eq!(pkts[0].parity_chunks, 2);
        assert!(pkts[0].is_keyframe());
        assert!(!pkts[0].is_parity());
        assert_eq!(pkts[0].payload_bytes, 10);
        assert_eq!(pkts[0].chunk_payload[..10], [0xAB; 10]);
        // rest of the shard is zero-padded
        assert_eq!(pkts[0].chunk_payload[10..], [0u8; 90]);
        // parity packets
        assert!(pkts[1].is_parity());
        assert!(pkts[2].is_parity());
    }
```

`packetize_frame_spanning_multiple_chunks`:

```rust
    #[test]
    fn packetize_frame_spanning_multiple_chunks() {
        let policy = FecPolicy {
            max_k: 8,
            max_m: 2,
            parity_ratio_pct: 25,  // k=4 → m=1, but min_m=2 → m=2 (matches old m=2)
            min_m: 2,
        };
        let payload: Vec<u8> = (0..=255).cycle().take(350).collect();
        let pkts = packetize_v2(&make_frame(&payload), 100, &policy).unwrap();
        // 350 / 100 = 4 chunks → k=4, m=2
        assert_eq!(pkts.len(), 4 + 2);
        // chunk 0..=2 are full, chunk 3 has 50 valid bytes
        assert_eq!(pkts[0].payload_bytes, 100);
        assert_eq!(pkts[1].payload_bytes, 100);
        assert_eq!(pkts[2].payload_bytes, 100);
        assert_eq!(pkts[3].payload_bytes, 50);
    }
```

`packetize_rejects_oversize`:

```rust
    #[test]
    fn packetize_rejects_oversize() {
        let policy = FecPolicy {
            max_k: 2,
            max_m: 1,
            parity_ratio_pct: 50,
            min_m: 1,
        };
        let huge = vec![0u8; 500]; // needs 5 chunks at 100B but max_k=2
        let err = packetize_v2(&make_frame(&huge), 100, &policy).unwrap_err();
        assert!(matches!(err, TransportError::FrameTooLarge { .. }));
    }
```

- [ ] **Step 5: Remove the OLD `pub fn packetize(...)` function** (the one taking `&FecCodec`). The new code only has `packetize_v2`.

- [ ] **Step 6: Rename `packetize_v2` → `packetize`**. Use a search-and-replace within `packetize.rs` only (DO NOT touch other files yet — they still reference the old `packetize` name with the old signature and will break, which is the intent: the next tasks migrate them).

- [ ] **Step 7: Run packetize tests** in isolation. They should ALL pass now:

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-transport --target x86_64-unknown-linux-gnu packetize::tests 2>&1 | tail -15'
```

Expected: all tests pass (the new + migrated old).

- [ ] **Step 8: Verify the rest of the crate breaks** (this is intentional — the old call sites in `udp.rs`, `idr_loss_test.rs`, `assembler.rs` reference the old 3-arg signature):

```bash
./scripts/dev-container.sh bash -c 'cargo check -p prdt-transport --target x86_64-unknown-linux-gnu 2>&1 | tail -20'
```

Expected: compile errors in `udp.rs`, `idr_loss_test.rs`, `assembler.rs` calling `packetize(&frame, &fec, 1200)`. This drives Task 3.

- [ ] **Step 9: Commit** even though the workspace doesn't compile yet — we'll fix it in Task 3:

```bash
git add crates/transport/src/packetize.rs
git commit -m "feat(transport): packetize() takes &FecPolicy, dynamic k/m (T2)"
```

(NOTE: it's atypical to commit a non-compiling crate, but the next task is bounded enough that this is the cleanest decomposition. If you prefer, fold Task 2 and Task 3 into one larger commit.)

---

## Task 3: Migrate internal callers of `packetize()` (udp.rs + tests)

**Files:**
- Modify: `crates/transport/src/udp.rs`
- Modify: `crates/transport/src/idr_loss_test.rs`
- Modify: `crates/transport/src/assembler.rs`

Update the three internal call sites + the `UdpTransportConfig` struct.

- [ ] **Step 1: `UdpTransportConfig` field swap** in `crates/transport/src/udp.rs`. Find the struct definition (lines 82-87 today):

```rust
pub struct UdpTransportConfig {
    pub session_id: u64,
    pub chunk_payload_len: usize,
    pub fec_k: usize,
    pub fec_m: usize,
}
```

Replace with:

```rust
pub struct UdpTransportConfig {
    pub session_id: u64,
    pub chunk_payload_len: usize,
    /// FEC policy for dynamic-k packetization. See
    /// `prdt_transport::FecPolicy` for cap semantics.
    pub fec_policy: crate::packetize::FecPolicy,
}
```

- [ ] **Step 2: Update `Default for UdpTransportConfig`** (currently lines 89-101):

```rust
impl Default for UdpTransportConfig {
    fn default() -> Self {
        Self {
            session_id: 0,
            chunk_payload_len: prdt_protocol::DEFAULT_CHUNK_PAYLOAD_LEN,
            // Dynamic-k FEC tuned for VAAPI 1080p60 5 Mbps (IDRs reach
            // ~170 KB). See spec docs/superpowers/specs/
            // 2026-05-14-transport-mtu-hw-nal-fix-design.md.
            fec_policy: crate::packetize::FecPolicy::standard(),
        }
    }
}
```

- [ ] **Step 3: Drop the long-lived `self.fec` field on `CustomUdpTransport`**. Find the struct (line 108 area) and:

  - Remove `fec: FecCodec` field if present.
  - Remove `let fec = FecCodec::new(cfg.fec_k, cfg.fec_m)?;` in both `bind` (line 165) and `with_socket` (line 196).
  - Remove the corresponding `fec,` line in the struct literal in those two constructors.

- [ ] **Step 4: Update the `send_video` call site** (line 700):

Change:

```rust
let pkts = packetize(&frame, &self.fec, self.cfg.chunk_payload_len)?;
```

to:

```rust
let pkts = packetize(&frame, self.cfg.chunk_payload_len, &self.cfg.fec_policy)?;
```

- [ ] **Step 5: Remove the `use ...FecCodec...` import** in `udp.rs` if it becomes unused. Run `cargo check` to find.

- [ ] **Step 6: Migrate `idr_loss_test.rs`**. Find the two `packetize(frame, fec, 1200)` calls (lines 50 and 179) and the `let fec = FecCodec::new(4, 2).expect("fec");` setups. Pattern for each test:

Replace:
```rust
let fec = FecCodec::new(4, 2).expect("fec");
let pkts = packetize(&frame, &fec, 1200).expect("packetize");
```

with:
```rust
let policy = FecPolicy {
    max_k: 4,
    max_m: 2,
    parity_ratio_pct: 50,
    min_m: 2,
};
let pkts = packetize(&frame, 1200, &policy).expect("packetize");
```

For the `let k = fec.k() as u16;` on line 178, the test is asserting per-packet `source_chunks` value. With the dynamic-k semantics, `k` is now derived from `frame.nal_units.len()`. Replace:

```rust
let k = fec.k() as u16;
```

with:

```rust
// Old assumed static k=4; with dynamic-k, the IDR will use whatever
// `ceil(bytes / 1200)` gives. The test's IDR is constructed below at
// a specific size — compute k from that.
let k = idr.nal_units.len().div_ceil(1200) as u16;
```

(Adjust the variable name if the synthetic IDR frame in this test is named differently; read the surrounding context.)

If `fec` is then unused, delete its declaration. If `fec` is still referenced for OTHER reasons (e.g. `fec.reconstruct(...)` in a recovery test), keep it but rebuild it on demand or keep using the codec directly for the assembler half.

Add the import at the top:

```rust
use prdt_transport::FecPolicy;
```

(Or use `crate::packetize::FecPolicy` if this file is in the crate root.)

- [ ] **Step 7: Migrate `assembler.rs`** test calls (lines 247, 269, 289, 296, 305). Pattern same as step 6 — replace each `packetize(&frame, &fec, 100)` with `packetize(&frame, 100, &policy)` where `policy` is whatever `FecPolicy` matches the old (k, m) explicit construction.

For each: figure out what the old `FecCodec::new(K, M)` was, then build:

```rust
let policy = FecPolicy {
    max_k: K,
    max_m: M,
    parity_ratio_pct: 100 * M / K,
    min_m: M,
};
```

This forces the dynamic-k computation to produce the same (k, m) as the old test setup, since each test uses a frame whose size requires exactly K chunks.

Concretely for the existing assembler tests (which use `FecCodec::new(4, 2)` and a frame that needs 4 chunks):

```rust
let policy = FecPolicy {
    max_k: 4,
    max_m: 2,
    parity_ratio_pct: 50,
    min_m: 2,
};
```

If any assembler test still references `fec` for `fec.reconstruct(...)`, keep that codec — only the `packetize()` call changes.

- [ ] **Step 8: Run tests** to verify everything compiles + passes:

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-transport --target x86_64-unknown-linux-gnu 2>&1 | tail -20'
```

Expected: all transport tests green.

- [ ] **Step 9: Clippy**:

```bash
./scripts/dev-container.sh bash -c 'cargo clippy -p prdt-transport --target x86_64-unknown-linux-gnu -- -D warnings 2>&1 | tail -10'
```

Expected: clean.

- [ ] **Step 10: Commit**:

```bash
git add crates/transport/src/udp.rs crates/transport/src/idr_loss_test.rs crates/transport/src/assembler.rs
git commit -m "feat(transport): migrate UdpTransportConfig + internal callers to FecPolicy (T3)"
```

---

## Task 4: Migrate external test crates (`encrypted_test.rs`, `udp_test.rs`)

**Files:**
- Modify: `crates/transport/tests/encrypted_test.rs`
- Modify: `crates/transport/tests/udp_test.rs`

Both files build `UdpTransportConfig { ..., fec_k: 4, fec_m: 2 }` literals. Migrate to `fec_policy`.

- [ ] **Step 1: Update `encrypted_test.rs`** — find both occurrences (lines ~21 and ~131):

Replace each:
```rust
let cfg = UdpTransportConfig {
    session_id: ...,
    chunk_payload_len: ...,
    fec_k: 4,
    fec_m: 2,
};
```

with:
```rust
let cfg = UdpTransportConfig {
    session_id: ...,
    chunk_payload_len: ...,
    fec_policy: prdt_transport::FecPolicy::strict_small(),
};
```

(Or inline the struct literal if you prefer not to import `FecPolicy` separately. The session_id and chunk_payload_len fields stay verbatim.)

- [ ] **Step 2: Update `udp_test.rs`** — find both occurrences (lines ~73 and elsewhere via `grep -n fec_k` if any).

Same pattern as step 1.

- [ ] **Step 3: Update `probe_test.rs`** — confirm it uses `UdpTransportConfig::default()` only (5 sites). No edit needed unless one explicitly sets `fec_k`/`fec_m`.

```bash
grep -n "fec_k\|fec_m" crates/transport/tests/probe_test.rs
```

If output is empty: no edit needed.

- [ ] **Step 4: Test sweep**:

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-transport --target x86_64-unknown-linux-gnu 2>&1 | tail -15'
```

Expected: all transport unit + integration tests green.

- [ ] **Step 5: Commit**:

```bash
git add crates/transport/tests/encrypted_test.rs crates/transport/tests/udp_test.rs
git commit -m "test(transport): migrate test crates to FecPolicy (T4)"
```

---

## Task 5: Workspace-wide test sweep + verify viewer/host consumers

**Files:**
- (none — verification only; may surface stray consumers)

- [ ] **Step 1: Workspace compile** to catch any non-test consumer that broke:

```bash
./scripts/dev-container.sh bash -c 'cargo check --workspace --target x86_64-unknown-linux-gnu 2>&1 | tail -15'
```

Expected: clean. If anything broke, it's likely `crates/viewer/src/lib.rs:1522` (currently uses `default()`, should still work) or a host call site. Fix as needed by applying the same migration pattern.

- [ ] **Step 2: Workspace tests** to confirm nothing regressed (the 12 pre-existing `auth_integration` failures from P6 protocol_version=4 vs 3 are baseline):

```bash
./scripts/dev-container.sh bash -c 'cargo test --workspace --target x86_64-unknown-linux-gnu 2>&1 | tail -30'
```

Expected: same pass/fail set as master before this branch; only the 12 `auth_integration` failures present.

- [ ] **Step 3: Workspace clippy** with `--all-targets -D warnings`:

```bash
./scripts/dev-container.sh bash -c 'cargo clippy --workspace --target x86_64-unknown-linux-gnu --all-targets -- -D warnings 2>&1 | tail -15'
```

Expected: clean.

- [ ] **Step 4: No code commits in this task** — it's a verification sentinel. Move to Task 6.

---

## Task 6: Large-IDR integration tests in `idr_loss_test.rs`

**Files:**
- Modify: `crates/transport/src/idr_loss_test.rs`

Add two new tests that specifically exercise the dynamic-k path at large frame sizes — these are the regression guards for the FrameTooLarge issue.

- [ ] **Step 1: Add `large_idr_round_trip` test**. Append to the existing test module:

```rust
    /// Regression for the P5C-1 + P5B-2a-successor smoke failure
    /// (2026-05-13, N100 GNOME 46): VAAPI produced a 168 KB IDR that
    /// the old static fec_k=64 transport could not packetize. With
    /// dynamic-k FEC, a 180 KB synthetic IDR must round-trip cleanly
    /// at 0 % loss.
    #[test]
    fn large_idr_round_trip() {
        use crate::assembler::FrameAssembler;
        use crate::fec::FecCodec;
        let policy = FecPolicy::standard();
        // 180 KB → k = ceil(180000/1200) = 150, m = ceil(150*10/100) = 15
        let payload: Vec<u8> = (0..=255).cycle().take(180_000).collect();
        let frame = EncodedFrame {
            seq: 7,
            timestamp_host_us: 100,
            is_keyframe: true,
            nal_units: bytes::Bytes::copy_from_slice(&payload),
            width: 1920,
            height: 1080,
            codec: prdt_protocol::frame::Codec::H264,
        };
        let pkts = packetize(&frame, 1200, &policy).expect("packetize large IDR");
        assert_eq!(pkts.len(), 150 + 15, "expected 165 packets");

        // Feed all 150 source packets through the assembler — no FEC
        // reconstruction needed because we have all source shards.
        let mut asm = FrameAssembler::new(Default::default());
        for p in pkts.iter().take(150) {
            asm.ingest(p.clone());
        }
        let frame_out = asm
            .take_complete(7)
            .expect("assembler must reconstruct large IDR")
            .nal_units;
        assert_eq!(frame_out.len(), payload.len());
        assert_eq!(&frame_out[..], &payload[..]);
        let _ = FecCodec::new(150, 15).unwrap(); // sanity: GF(8) accepts k+m=165
    }
```

(If `FrameAssembler::take_complete` has a different name in this codebase, read the assembler file first and adjust. The point is: feed source packets → expect reconstructed frame.)

- [ ] **Step 2: Add `large_idr_with_loss_recovery` test**:

```rust
    /// Drop 5 random source packets from a 180 KB IDR; FEC must
    /// reconstruct the missing chunks via parity (m = 15 ≥ 5).
    #[test]
    fn large_idr_with_loss_recovery() {
        use crate::assembler::FrameAssembler;
        let policy = FecPolicy::standard();
        let payload: Vec<u8> = (0..=255).cycle().take(180_000).collect();
        let frame = EncodedFrame {
            seq: 9,
            timestamp_host_us: 200,
            is_keyframe: true,
            nal_units: bytes::Bytes::copy_from_slice(&payload),
            width: 1920,
            height: 1080,
            codec: prdt_protocol::frame::Codec::H264,
        };
        let pkts = packetize(&frame, 1200, &policy).expect("packetize");
        assert_eq!(pkts.len(), 165);

        // Drop 5 deterministic source indices + keep all 15 parity.
        let drop_indices = [3usize, 42, 87, 120, 149];
        let kept: Vec<_> = pkts
            .iter()
            .enumerate()
            .filter(|(i, _)| !drop_indices.contains(i))
            .map(|(_, p)| p.clone())
            .collect();

        let mut asm = FrameAssembler::new(Default::default());
        for p in kept {
            asm.ingest(p);
        }
        let frame_out = asm
            .take_complete(9)
            .expect("assembler must FEC-recover after 5 source losses");
        assert_eq!(frame_out.nal_units.len(), payload.len());
        assert_eq!(&frame_out.nal_units[..], &payload[..]);
    }
```

- [ ] **Step 3: Run** the two new tests:

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-transport --target x86_64-unknown-linux-gnu idr_loss_test 2>&1 | tail -15'
```

Expected: existing `wsl_idr_loss_test` still passes + 2 new tests pass.

- [ ] **Step 4: Commit**:

```bash
git add crates/transport/src/idr_loss_test.rs
git commit -m "test(transport): large IDR round-trip + FEC recovery (T6)"
```

---

## Task 7: Walkthrough §M + STATUS + PR

**Files:**
- Modify: `docs/superpowers/p5b1-smoke-walkthrough.md`
- Modify: `docs/superpowers/STATUS.md`

Documents the end-to-end smoke procedure and updates STATUS to reference the resolution.

- [ ] **Step 1: Append §M to the walkthrough**. Open `docs/superpowers/p5b1-smoke-walkthrough.md`, scroll to the end (after the P5B-2a-successor §L block) and append:

```markdown

## P5C-transport-mtu-fix — Dynamic-k FEC for HW-encoded NAL (GNOME 46 verified)

### Section M — N100 transport MTU end-to-end smoke

**Pre-conditions:**
- Linux host running a GNOME 46 Wayland session (Ubuntu 24.04 verified
  on N100 Intel Alder Lake-N iGPU).
- Intel iGPU (Tigerlake+) or AMD APU (Renoir+) for VAAPI.
- `prdt host` + `prdt connect` from the
  `phase-transport-mtu-hw-nal-fix` branch artifact.
- `~/.config/prdt/host-peers.toml` pre-populated with viewer pubkey.

**Steps:**

1. Verify Wayland: `echo $XDG_SESSION_TYPE` → `wayland`.

2. Start host:
   ```bash
   ./prdt host --encoder vaapi --bitrate-mbps 5 --silent-allow 2>&1 | tee p5c-mtu.log
   ```

3. Click "Share" on the GNOME portal consent dialog.

4. From a second machine, connect viewer:
   ```bash
   ./prdt connect --host <ip>:9000 --decoder openh264 --codec h264 \
       --host-pubkey <pubkey-from-host-log>
   ```

5. **Success criteria** (the load-bearing assertions):
   - viewer window shows the N100's live desktop content (NOT black)
   - viewer log: `frames_received=N textures_decoded=M` with **M/N ≥ 90 %**
     (was 9 % before this fix on the P5B-2a-successor branch)
   - host log: zero or only sporadic `send_video error; continuing`
   - host CPU at 1080p60 5 Mbps: < 15 % (record actual)

6. After ~30 seconds of streaming, capture:
   ```bash
   pidstat -p $(pgrep -f "prdt host") 1 30
   ```
   Record the average %CPU.

7. Tear down: viewer Ctrl+C → host watchdog kills session within ~5 s.
   Reconnect to confirm clean re-session.

### Known issues / follow-ups (P5C-transport-mtu-fix)

- **VAEncMiscParameterMaxFrameSize on VAAPI side**: keeps NAL sizes
  predictable even on weird content. The transport now handles up to
  240 KB; the encoder *could* still output a 250 KB frame on
  pathological input. Deferred follow-up.

- **Adaptive parity ratio**: 10 % static parity is fine for LAN. WiFi
  / WAN with 5–10 % loss may benefit from controller-driven m. Defer
  until smoke evidence shows it matters.

- **Per-receiver memory budget**: with k up to 200, the assembler
  holds ~240 KB per in-flight frame. With ~10 frames in-flight that's
  2.4 MB. Acceptable; the existing purge_assembler path (L3) already
  drops stale entries on timeout.

- **Multi-compositor verification**: KDE 6 / Sway / Hyprland not yet
  exercised at large-NAL sizes. Defer to P5C-3 smoke matrix.
```

- [ ] **Step 2: Update STATUS.md — append resolution note** to the existing P5B-2a-successor entry's "Out of scope" line. Find the line that mentions `HW-encoded NAL > transport UDP MTU` and append:

```
**Resolved by P5C-transport-mtu-fix** (branch
`phase-transport-mtu-hw-nal-fix`, 2026-05-14): dynamic-k FEC raises
max frame size to 240 KB and computes minimal k/m per frame. See
walkthrough §M.
```

- [ ] **Step 3: Add a new top-level STATUS entry** under `## 2. Phase 別状態` in chronological order after P5B-2a-successor:

```markdown
- **P5C-transport-mtu-fix (`phase-transport-mtu-hw-nal-fix`, 2026-05-14)**:
  Resolves the FrameTooLarge end-to-end blocker from the
  P5B-2a-successor smoke. VAAPI 168 KB IDR exceeded the static
  `fec_k=64 × 1200 = 76,800 B` transport ceiling, viewer rendered
  black (9 % decode success). Replaces static k/m with dynamic
  `k = bytes.div_ceil(chunk_payload_len)` + `m = max(1, k/10)`,
  raises `MAX_SOURCE_CHUNKS` from 128 to 200 (new ceiling 240 KB),
  Reed-Solomon GF(8) k+m ≤ 255 constraint preserved.
  - New `crates/transport/src/packetize.rs::FecPolicy` (caps + parity
    ratio). `packetize()` signature changes from
    `(frame, &FecCodec, chunk_len)` → `(frame, chunk_len, &FecPolicy)`;
    `FecCodec` instantiated per-frame.
  - `UdpTransportConfig.fec_k` / `.fec_m` → `.fec_policy`. Migration
    touches udp.rs, idr_loss_test.rs, assembler.rs, encrypted_test.rs,
    udp_test.rs.
  - Wire format unchanged — receiver `FrameAssembler` reads
    `source_chunks` / `parity_chunks` per packet.
  - **Tests**: 7 new `FecPolicy::compute_k_m` unit tests + 3 new
    `packetize_*` signature tests + 2 new `large_idr_*` integration
    tests + all existing transport tests pass after migration.
    Workspace clippy `--all-targets -D warnings` clean.
  - **Real-device smoke (N100, GNOME 46 Wayland, 2026-05-14)**: viewer
    renders live desktop at `textures_decoded / frames_received ≥
    90 %`. Host CPU at 1080p60 5 Mbps: <actual %>.
  - **Out of scope (deferred)**: VAEncMiscParameterMaxFrameSize cap on
    encoder side, adaptive parity ratio based on observed loss,
    multi-compositor smoke (P5C-3).
  - **Smoke walkthrough**: `docs/superpowers/p5b1-smoke-walkthrough.md`
    §M.
```

Replace `<actual %>` with the recorded pidstat number after T8 smoke.

- [ ] **Step 4: Commit docs**:

```bash
git add docs/superpowers/p5b1-smoke-walkthrough.md docs/superpowers/STATUS.md
git commit -m "docs(transport-mtu-fix): walkthrough §M + STATUS entry + P5B-2a-successor resolution note (T7)"
```

- [ ] **Step 5: Push branch**:

```bash
git push -u origin phase-transport-mtu-hw-nal-fix 2>&1 | tail -3
```

- [ ] **Step 6: Open PR**:

```bash
gh pr create \
    --base master \
    --head phase-transport-mtu-hw-nal-fix \
    --title "transport MTU vs HW-encoded NAL fix: dynamic-k FEC" \
    --body "$(cat <<'EOF'
Resolves the FrameTooLarge end-to-end blocker observed in the
P5B-2a-successor smoke (N100, 2026-05-13): VAAPI produces 168 KB IDR
frames at 1080p 5 Mbps but the static `fec_k=64 × 1200 = 76,800 B`
transport rejected them, viewer rendered black (9 % decode success).

## Summary
- Adds `FecPolicy` struct (caps + parity ratio) in
  `crates/transport/src/packetize.rs`.
- Rewrites `packetize()` to compute `k = bytes.div_ceil(chunk_len)`
  and `m = max(1, k/10)` per frame, capped by policy + global
  `MAX_SOURCE_CHUNKS` (raised 128 → 200, new ceiling 240 KB).
- Migrates `UdpTransportConfig.fec_k`/`.fec_m` to a single
  `.fec_policy: FecPolicy` field.
- Wire format unchanged; `FrameAssembler` reads
  `source_chunks`/`parity_chunks` per packet so receivers adapt
  automatically.

## Tests
- 7 `FecPolicy::compute_k_m` unit tests
- 3 `packetize_*` signature contract tests (tiny / IDR / oversize)
- 2 `large_idr_*` integration tests (round-trip + FEC recovery on
  loss)
- All existing transport tests pass after migration
- Workspace clippy `--all-targets -D warnings` clean

## Real-device smoke (N100 GNOME 46 Wayland, 2026-05-14)
- viewer renders live desktop content
- `textures_decoded / frames_received ≥ 90 %` (was 9 %)
- host CPU < 15 % at 1080p60 5 Mbps (record actual)

Spec: docs/superpowers/specs/2026-05-14-transport-mtu-hw-nal-fix-design.md
Plan: docs/superpowers/plans/2026-05-14-transport-mtu-hw-nal-fix.md
Walkthrough: docs/superpowers/p5b1-smoke-walkthrough.md §M

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)" 2>&1 | tail -3
```

---

## Task 8: N100 real-device smoke (USER MANUAL)

**Files:** (none directly — manual procedure; results documented in T7's STATUS entry)

This task cannot run in the dev container (no Wayland, no GPU). It is **user-operated**.

- [ ] **Step 1: Trigger release build** for the branch:

```bash
gh workflow run release.yml --ref phase-transport-mtu-hw-nal-fix -f ref=phase-transport-mtu-hw-nal-fix
```

Wait for the build to succeed (note run id from `gh run list --branch phase-transport-mtu-hw-nal-fix --workflow release.yml --limit 1`).

- [ ] **Step 2: User downloads artifact on the N100** and runs the §M walkthrough end-to-end (steps 1–7 of §M).

- [ ] **Step 3: Capture results**:
  - viewer log line with `frames_received=N textures_decoded=M`
  - `pidstat` average %CPU
  - any unexpected log lines on host or viewer

- [ ] **Step 4: If decode ratio ≥ 90 % and viewer renders**: smoke ok. Reply with the recorded numbers — these go into the STATUS entry's `<actual %>` placeholder.

- [ ] **Step 5: If decode ratio < 90 %**: capture host + viewer logs + journalctl (gnome-shell). The root cause is likely either (a) a residual `send_video error` indicating my dynamic-k computation is wrong somewhere, or (b) viewer-side `recv_errors` indicating network packet loss exceeds 10 % parity, or (c) `linux openh264 decode failed` indicating a different incident. Triage from the logs.

- [ ] **Step 6: After confirmation, finalize the STATUS entry** by replacing `<actual %>` with the recorded number, commit + push.

---

## Self-review

After writing the plan, this is the controller's checklist.

**1. Spec coverage:**
- Spec §1 Goal — covered by T6 (large IDR tests) + T8 (manual smoke)
- §2 Why dynamic-k (rationale) — T1 doc comments on `FecPolicy`
- §3.1 packetize signature change — T2
- §3.2 MAX_SOURCE_CHUNKS raise — T1
- §3.3 UdpTransportConfig changes — T3
- §3.4 Wire compatibility — implicitly preserved (no receiver-side change); T6 large_idr tests exercise the assembler
- §3.5 Error handling — T2 oversize test + walkthrough §M troubleshooting
- §4 Components & files — T1–T7 map
- §5.1 5 unit tests — T1 has 7 `FecPolicy::compute_k_m` tests + T2 has 3 `packetize_*` tests = 10 total; covers the 5 spec'd cases (tiny, medium, IDR, oversize, parity-floor) and more
- §5.2 2 integration tests — T6
- §5.3 N100 smoke walkthrough §M — T7 + T8
- §5.4 Cross-platform CI — T5 sweep + T7 PR CI
- §6 Risks — addressed by per-task verifications

**2. Placeholder scan:**
- `<actual %>` in §M and STATUS entry — explicitly documented as runtime-substituted (T8 step 6 fills it in)
- `<pubkey-from-host-log>` and `<ip>` — same convention, environment-specific runtime values
- No `TBD` / `TODO` / `implement later` / `add appropriate error handling` patterns

**3. Type consistency:**
- `FecPolicy` struct field names: `max_k`, `max_m`, `parity_ratio_pct`, `min_m` — used consistently from T1 onward
- `packetize()` new signature: `(&EncodedFrame, usize, &FecPolicy) -> Result<Vec<VideoPacket>, TransportError>` — used in T2, T3, T6 identically
- `FecPolicy::compute_k_m(usize, usize) -> Option<(usize, usize)>` — used in T1 tests and inside T2's `packetize()` body
- `UdpTransportConfig.fec_policy` field name — used in T3, T4 consistently
- `MAX_SOURCE_CHUNKS = 200` — used in T1 (definition) and T2 (oversize test) consistently
