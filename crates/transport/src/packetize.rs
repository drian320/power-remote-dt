use prdt_protocol::{wire::video_flags, EncodedFrame, VideoPacket};

use crate::error::TransportError;
use crate::fec::FecCodec;

/// Max source chunks per frame. Raised from 128 (Plan 3 / 4 era) to 200
/// to accommodate VAAPI HW-encoded 1080p60 IDRs which reach ~170 KB
/// (= 142 chunks of 1200 B). Reed-Solomon GF(8) supports k + m ≤ 255,
/// so 200 leaves room for up to m = 55 parity per frame. With the
/// production `FecPolicy::standard()` (parity_ratio_pct = 10, max_m =
/// 20) the realistic worst case is k = 200, m = 20 → 220 chunks total.
pub const MAX_SOURCE_CHUNKS: usize = 200;

// Compile-time consistency: FecPolicy::standard()'s worst case (k=max_k +
// m=max_m) must fit within FecCodec::MAX_SHARDS, otherwise packetize()
// would fail at FecCodec construction for frames that pass compute_k_m().
// FecPolicy::standard(): max_k=200, max_m=20
const _: () = {
    if 200 + 20 > crate::fec::MAX_SHARDS {
        panic!("FecPolicy::standard() max_k + max_m exceeds FecCodec::MAX_SHARDS");
    }
};

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
        if chunk_payload_len == 0 {
            return None;
        }
        debug_assert!(
            self.min_m <= self.max_m,
            "FecPolicy: min_m ({}) must be <= max_m ({})",
            self.min_m,
            self.max_m,
        );
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

/// Dynamic-k packetization. Replaces the static `&FecCodec`
/// argument with a `&FecPolicy` and constructs the codec per call.
///
/// The receiver-side `FrameAssembler` reads `source_chunks` and
/// `parity_chunks` from each packet header, so dynamic k/m is fully
/// wire-compatible.
///
/// Returns `FrameTooLarge` if the frame's chunk count exceeds the
/// policy's `max_k` or the global `MAX_SOURCE_CHUNKS` ceiling.
pub fn packetize(
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

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use prdt_protocol::frame::Codec;

    fn make_frame(bytes: &[u8]) -> EncodedFrame {
        EncodedFrame {
            seq: 1,
            timestamp_host_us: 42,
            is_keyframe: true,
            nal_units: Bytes::copy_from_slice(bytes),
            width: 3840,
            height: 2160,
            codec: Codec::H265,
        }
    }

    #[test]
    fn packetize_small_frame() {
        let policy = FecPolicy {
            max_k: 4,
            max_m: 2,
            parity_ratio_pct: 50,  // 4*50% = 2 → m=2 (matches old k=4, m=2)
            min_m: 2,
        };
        let payload = vec![0xAB; 10];
        let pkts = packetize(&make_frame(&payload), 100, &policy).unwrap();
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

    #[test]
    fn packetize_frame_spanning_multiple_chunks() {
        let policy = FecPolicy {
            max_k: 8,
            max_m: 2,
            parity_ratio_pct: 25,  // k=4 → m=1, but min_m=2 → m=2 (matches old m=2)
            min_m: 2,
        };
        let payload: Vec<u8> = (0..=255).cycle().take(350).collect();
        let pkts = packetize(&make_frame(&payload), 100, &policy).unwrap();
        // 350 / 100 = 4 chunks → k=4, m=2
        assert_eq!(pkts.len(), 4 + 2);
        // chunk 0..=2 are full, chunk 3 has 50 valid bytes
        assert_eq!(pkts[0].payload_bytes, 100);
        assert_eq!(pkts[1].payload_bytes, 100);
        assert_eq!(pkts[2].payload_bytes, 100);
        assert_eq!(pkts[3].payload_bytes, 50);
    }

    #[test]
    fn packetize_rejects_oversize() {
        let policy = FecPolicy {
            max_k: 2,
            max_m: 1,
            parity_ratio_pct: 50,
            min_m: 1,
        };
        let huge = vec![0u8; 500]; // needs 5 chunks at 100B but max_k=2
        let err = packetize(&make_frame(&huge), 100, &policy).unwrap_err();
        assert!(matches!(err, TransportError::FrameTooLarge { .. }));
    }

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
        let p = FecPolicy::standard();
        assert_eq!(p.compute_k_m(100, 1200), Some((1, 1)));
    }

    #[test]
    fn fec_policy_compute_k_m_medium_frame() {
        let p = FecPolicy::standard();
        assert_eq!(p.compute_k_m(5000, 1200), Some((5, 1)));
    }

    #[test]
    fn fec_policy_compute_k_m_idr_frame() {
        let p = FecPolicy::standard();
        assert_eq!(p.compute_k_m(168000, 1200), Some((140, 14)));
    }

    #[test]
    fn fec_policy_compute_k_m_oversize_rejects() {
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
        assert_eq!(p.compute_k_m(400, 1200), Some((1, 2)));
        assert!(p.compute_k_m(5000, 1200).is_none());
    }

    #[test]
    fn fec_policy_compute_k_m_zero_chunk_len_returns_none() {
        let p = FecPolicy::standard();
        assert!(p.compute_k_m(1000, 0).is_none());
    }

    #[test]
    #[should_panic(expected = "min_m")]
    #[cfg(debug_assertions)]
    fn fec_policy_compute_k_m_misconfigured_min_max_panics_in_debug() {
        let p = FecPolicy {
            max_k: 100,
            max_m: 2,
            parity_ratio_pct: 10,
            min_m: 5, // intentionally > max_m
        };
        let _ = p.compute_k_m(1000, 1200);
    }

    #[test]
    fn packetize_new_signature_tiny_frame() {
        let policy = FecPolicy::standard();
        let payload = vec![0xAB; 10];
        let pkts = packetize(&make_frame(&payload), 1200, &policy).unwrap();
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
        let pkts = packetize(&make_frame(&payload), 1200, &policy).unwrap();
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
        let err = packetize(&make_frame(&payload), 1200, &policy).unwrap_err();
        assert!(matches!(err, TransportError::FrameTooLarge { .. }));
    }
}
