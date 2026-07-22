//! Opt-in frame-tracing diagnostics for localizing where an encoded
//! frame's content and its `frame_seq` diverge (host send / transport
//! assembly / decode feed). Entirely additive: when `PRDT_FRAME_TRACE`
//! is unset, [`enabled`] is a cached `false` read and no tracing happens.
//!
//! Correlation rule: if `tx.fnv(seq=N) == feed.fnv(seq=N)` for every N,
//! the content/seq pairing survived transport intact and the fault is
//! host-side (encoder/producer). Otherwise, the earliest stage — `tx`,
//! `asm`, or `feed`, in pipeline order — whose fnv for a given seq
//! differs from the previous stage's fnv for that same seq is the
//! guilty stage.

use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();

/// Returns true if the `PRDT_FRAME_TRACE` env var is set to any
/// non-empty value. Checked once per process; every call after the
/// first is a cached atomic-free read via `OnceLock`.
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("PRDT_FRAME_TRACE")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    })
}

/// Tiny inline FNV-1a 64-bit hash over frame payload bytes. Not
/// cryptographic — used only to fingerprint frame content so the same
/// bytes hash identically across the host/transport/viewer boundary.
pub fn fnv1a64(data: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}
