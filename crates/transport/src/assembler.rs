use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use prdt_protocol::{frame::Codec, EncodedFrame, VideoPacket};

use crate::error::TransportError;
use crate::fec::FecCodec;

pub const DEFAULT_ASSEMBLY_TIMEOUT: Duration = Duration::from_millis(100);
pub const STALE_SEQ_WINDOW: u64 = 8;

/// Per-frame partial state.
#[derive(Debug)]
struct Partial {
    first_seen: Instant,
    source_chunks: u16,
    parity_chunks: u16,
    // chunk_idx → full-length (shard_len) shard payload
    chunks: HashMap<u16, Vec<u8>>,
    /// Total unpadded byte length of the whole frame. Carried identically
    /// on every packet (source and parity), so it is known even when the
    /// last source chunk was lost and must be FEC-reconstructed — that
    /// chunk is the only partial one, and its own packet was the only
    /// place its `payload_bytes` lived.
    frame_payload_bytes: u32,
    is_keyframe: bool,
}

/// Reassembles VideoPackets into EncodedFrames.
///
/// Internally tracks many in-flight frames. Call `try_pop_ready` to retrieve
/// newly-completed frames. Call `purge` periodically to drop timed-out frames.
pub struct FrameAssembler {
    partials: HashMap<u64, Partial>,
    /// Highest frame_seq we've ever completed or declined. Used for stale-drop.
    high_water_seq: u64,
    timeout: Duration,
    width: u32,
    height: u32,
    codec: Codec,
}

/// Outcome of feeding one VideoPacket.
#[derive(Debug)]
pub enum FeedResult {
    /// Still waiting for more chunks.
    Pending,
    /// This chunk was dropped (stale, or frame already completed).
    Stale,
    /// Frame is fully recovered (either all source chunks arrived, or FEC
    /// reconstructed the missing ones).
    Complete(EncodedFrame),
}

impl FrameAssembler {
    pub fn new(width: u32, height: u32, codec: Codec) -> Self {
        Self {
            partials: HashMap::new(),
            high_water_seq: 0,
            timeout: DEFAULT_ASSEMBLY_TIMEOUT,
            width,
            height,
            codec,
        }
    }

    pub fn set_timeout(&mut self, d: Duration) {
        self.timeout = d;
    }

    /// Feed one VideoPacket. `fec` is used for reconstruction if enough
    /// chunks have arrived but some are missing.
    pub fn feed(&mut self, pkt: VideoPacket, fec: &FecCodec) -> Result<FeedResult, TransportError> {
        // Drop stale frames (older than high_water - window).
        if pkt.frame_seq + STALE_SEQ_WINDOW < self.high_water_seq.saturating_add(1) {
            return Ok(FeedResult::Stale);
        }

        let total = pkt.source_chunks as usize + pkt.parity_chunks as usize;
        let shard_len = pkt.chunk_payload.len();
        let is_kf = pkt.is_keyframe();
        let chunk_idx = pkt.chunk_idx;
        let frame_seq = pkt.frame_seq;
        let ts = pkt.timestamp_host_us;
        let source_chunks = pkt.source_chunks;
        let parity_chunks = pkt.parity_chunks;
        let frame_payload_bytes = pkt.frame_payload_bytes;

        let entry = self.partials.entry(frame_seq).or_insert_with(|| Partial {
            first_seen: Instant::now(),
            source_chunks,
            parity_chunks,
            chunks: HashMap::new(),
            frame_payload_bytes,
            is_keyframe: is_kf,
        });

        // Paranoia: if a later packet disagrees on source/parity counts, trust the first.
        if entry.chunks.contains_key(&chunk_idx) {
            return Ok(FeedResult::Pending);
        }
        entry.chunks.insert(chunk_idx, pkt.chunk_payload);
        if is_kf {
            entry.is_keyframe = true;
        }

        let have = entry.chunks.len();
        let k = entry.source_chunks as usize;

        if have >= k {
            // Attempt reconstruction (possibly trivial if all source present).
            let seq = frame_seq;
            let frame_is_kf = entry.is_keyframe;
            let maybe_frame = self.try_complete(seq, total, shard_len, ts, frame_is_kf, fec);
            match maybe_frame {
                Ok(Some(frame)) => {
                    self.high_water_seq = self.high_water_seq.max(seq);
                    self.partials.remove(&seq);
                    return Ok(FeedResult::Complete(frame));
                }
                Ok(None) => return Ok(FeedResult::Pending),
                Err(e) => return Err(e),
            }
        }
        Ok(FeedResult::Pending)
    }

    fn try_complete(
        &mut self,
        seq: u64,
        total: usize,
        shard_len: usize,
        ts: u64,
        is_keyframe: bool,
        fec: &FecCodec,
    ) -> Result<Option<EncodedFrame>, TransportError> {
        let entry = match self.partials.get(&seq) {
            Some(e) => e,
            None => return Ok(None),
        };
        let k = entry.source_chunks as usize;
        if entry.chunks.len() < k {
            return Ok(None);
        }

        // Build k+m shard vector in index order with None for missing slots.
        let mut shards: Vec<Option<Vec<u8>>> = (0..total)
            .map(|i| entry.chunks.get(&(i as u16)).cloned())
            .collect();

        // If any source chunk missing, reconstruct.
        let missing_source = (0..k).any(|i| shards[i].is_none());
        let source: Vec<Vec<u8>> = if missing_source {
            fec.reconstruct(shards.clone()).map_err(|e| match e {
                TransportError::FecFailed { have, need, .. } => TransportError::FecFailed {
                    frame_seq: seq,
                    have,
                    need,
                },
                other => other,
            })?
        } else {
            // All source present; take them directly.
            shards.drain(..k).map(|s| s.unwrap()).collect()
        };

        // Stitch source shards back into a single EncodedFrame. The frame's
        // total unpadded length comes from `frame_payload_bytes`, which is
        // carried on every packet — so it is correct even when the last
        // source chunk (the only partial one) was FEC-reconstructed and
        // its own `payload_bytes` was never received. Each chunk's valid
        // span is `[i*shard_len, min((i+1)*shard_len, total))`.
        let total_bytes = entry.frame_payload_bytes as usize;
        let mut buf = Vec::with_capacity(total_bytes);
        for (i, shard) in source.iter().enumerate().take(k) {
            let chunk_start = i * shard_len;
            let valid = total_bytes.saturating_sub(chunk_start).min(shard_len);
            buf.extend_from_slice(&shard[..valid]);
        }

        let _ = entry.parity_chunks; // silence unused-field lint if ever triggered

        Ok(Some(EncodedFrame {
            seq,
            timestamp_host_us: ts,
            is_keyframe,
            nal_units: Bytes::from(buf),
            width: self.width,
            height: self.height,
            codec: self.codec,
        }))
    }

    /// Drop frames older than `self.timeout`. Returns Vec of frame_seqs
    /// that were purged; caller can use this to trigger IDR requests.
    pub fn purge(&mut self) -> Vec<u64> {
        let now = Instant::now();
        let stale: Vec<u64> = self
            .partials
            .iter()
            .filter(|(_, p)| now.duration_since(p.first_seen) > self.timeout)
            .map(|(seq, _)| *seq)
            .collect();
        for seq in &stale {
            self.partials.remove(seq);
            self.high_water_seq = self.high_water_seq.max(*seq);
        }
        stale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packetize::{packetize, FecPolicy};
    use bytes::Bytes;

    fn make_frame(seq: u64, bytes: &[u8]) -> EncodedFrame {
        EncodedFrame {
            seq,
            timestamp_host_us: seq * 1000,
            is_keyframe: true,
            nal_units: Bytes::copy_from_slice(bytes),
            width: 1920,
            height: 1080,
            codec: Codec::H265,
        }
    }

    #[test]
    fn assembler_trivial_all_chunks() {
        // 250 bytes at chunk_payload_len=100 → k=ceil(250/100)=3, m=2, total=5.
        // fec must match (k=3, m=2) in case reconstruction is needed.
        // Feed all 3 source chunks (indices 0,1,2); stop before parity to
        // avoid re-inserting a new partial entry for the completed frame.
        let fec = FecCodec::new(3, 2).unwrap();
        let policy = FecPolicy::strict_small();
        let frame = make_frame(1, &[0xAA; 250]);
        let pkts = packetize(&frame, 100, &policy).unwrap();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);

        // Feed source chunks only; skip parity.
        let mut last = FeedResult::Pending;
        for p in pkts.iter().take(3).cloned() {
            last = asm.feed(p, &fec).unwrap();
        }
        match last {
            FeedResult::Complete(f) => {
                assert_eq!(f.seq, 1);
                assert_eq!(&f.nal_units[..], &[0xAA; 250][..]);
                assert!(f.is_keyframe);
            }
            other => panic!("expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn assembler_reconstructs_missing_source() {
        // 200 bytes at chunk_payload_len=100 → k=2, parity_ratio_pct=50 →
        // raw_m=1, clamped to min_m=2 → m=2, total=4.
        // fec must match (k=2, m=2) for reconstruction to succeed.
        let fec = FecCodec::new(2, 2).unwrap();
        let policy = FecPolicy::strict_small();
        let frame = make_frame(1, &[0xCD; 200]);
        let mut pkts = packetize(&frame, 100, &policy).unwrap();
        // Drop source chunk idx 1.
        pkts.remove(1);
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);

        let mut final_result: Option<EncodedFrame> = None;
        for p in pkts {
            if let FeedResult::Complete(f) = asm.feed(p, &fec).unwrap() {
                final_result = Some(f);
                break;
            }
        }
        let f = final_result.expect("should complete via FEC");
        assert_eq!(&f.nal_units[..], &[0xCD; 200][..]);
    }

    #[test]
    fn assembler_drops_stale() {
        // [0; 10] at chunk_payload_len=100 → k=1, m=2, total=3.
        // Feed only the 1 source chunk (take(1)) so high_water advances to
        // 100 without re-inserting a new partial when parity arrives later.
        let fec = FecCodec::new(1, 2).unwrap();
        let policy = FecPolicy::strict_small();
        let f1 = make_frame(100, &[0; 10]);
        let pkts_f1 = packetize(&f1, 100, &policy).unwrap();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);
        for p in pkts_f1.into_iter().take(1) {
            asm.feed(p, &fec).unwrap();
        }
        // Now try a stale seq = 50; high_water_seq is now 100.
        let stale_frame = make_frame(50, &[0; 10]);
        let stale_pkts = packetize(&stale_frame, 100, &policy).unwrap();
        let r = asm.feed(stale_pkts[0].clone(), &fec).unwrap();
        assert!(matches!(r, FeedResult::Stale));
    }

    #[test]
    fn assembler_purges_timed_out() {
        let fec = FecCodec::new(2, 2).unwrap();
        let policy = FecPolicy::strict_small();
        // 150 bytes at chunk_payload_len=100 → k=ceil(150/100)=2, m=2, total=4.
        // Feed only the first chunk → have=1 < k=2 → stays Pending in partials
        // → times out → purge() fires.
        let frame = make_frame(1, &[0; 150]);
        let pkts = packetize(&frame, 100, &policy).unwrap();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);
        asm.set_timeout(Duration::from_millis(1));
        asm.feed(pkts[0].clone(), &fec).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let purged = asm.purge();
        assert_eq!(purged, vec![1]);
    }
}
