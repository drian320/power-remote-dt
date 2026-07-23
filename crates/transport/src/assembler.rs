use std::collections::{BTreeSet, HashMap};
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
    /// frame_seqs whose frame has already been completed and emitted.
    /// A late-arriving parity chunk (or a duplicate source chunk) for one
    /// of these seqs must be dropped rather than re-completing the frame,
    /// which would otherwise feed the decoder the same frame twice.
    /// Pruned on every completion to entries within `STALE_SEQ_WINDOW` of
    /// `high_water_seq`, so it stays bounded.
    completed: BTreeSet<u64>,
    /// Cumulative count of frames that were *entirely* lost — every chunk of
    /// the frame was dropped, so no `Partial` was ever created and the purge
    /// path never saw them. Such a frame is invisible to purge-based loss
    /// detection; it shows up only as a hole in the completed-seq sequence.
    /// Drained (and reset) by `take_wholesale_gaps`. See the completion path
    /// in `feed` for the counting invariant that avoids double-counting
    /// against the purge and stale-drop flows.
    wholesale_gaps: u64,
    /// Whether at least one frame has completed. The first completion only
    /// anchors the wholesale-gap baseline: a viewer joining an in-progress
    /// stream must not retro-count every seq that preceded it as a gap.
    saw_first_completion: bool,
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
            completed: BTreeSet::new(),
            wholesale_gaps: 0,
            saw_first_completion: false,
            timeout: DEFAULT_ASSEMBLY_TIMEOUT,
            width,
            height,
            codec,
        }
    }

    /// Number of tracked completed-frame entries. Test-only introspection
    /// for verifying the completed-set stays bounded (see `prune_completed`).
    #[cfg(test)]
    pub(crate) fn completed_len(&self) -> usize {
        self.completed.len()
    }

    /// Drop `completed` entries older than `STALE_SEQ_WINDOW` behind
    /// `high_water_seq`, mirroring the existing stale-drop window so the
    /// set can't grow unboundedly over a long session.
    fn prune_completed(&mut self) {
        let threshold = self.high_water_seq.saturating_sub(STALE_SEQ_WINDOW);
        self.completed = self.completed.split_off(&threshold);
    }

    pub fn set_timeout(&mut self, d: Duration) {
        self.timeout = d;
    }

    /// Return and reset the count of wholesale-lost frames observed since the
    /// last call — frames that were entirely lost (no chunk arrived, so no
    /// partial ever purged). The viewer's adaptive-bitrate tick folds this
    /// into its per-window loss so that wholesale loss, not just partial-frame
    /// purges, drives the controller.
    pub fn take_wholesale_gaps(&mut self) -> u64 {
        std::mem::take(&mut self.wholesale_gaps)
    }

    /// Feed one VideoPacket. `fec` is used for reconstruction if enough
    /// chunks have arrived but some are missing.
    pub fn feed(&mut self, pkt: VideoPacket, fec: &FecCodec) -> Result<FeedResult, TransportError> {
        // Drop stale frames (older than high_water - window).
        if pkt.frame_seq + STALE_SEQ_WINDOW < self.high_water_seq.saturating_add(1) {
            return Ok(FeedResult::Stale);
        }

        // Drop chunks for a frame that has already been completed and
        // emitted. Without this, a late parity chunk (or a duplicate
        // source chunk) arriving after emission would re-create the
        // `partials` entry via `entry().or_insert_with()` below and, for
        // k=1 frames, complete and emit the same frame a second time —
        // corrupting the decoder with a duplicate POC.
        if self.completed.contains(&pkt.frame_seq) {
            tracing::trace!(
                target: "frame.trace",
                "asm seq={} chunk_idx={} dropped: frame already completed",
                pkt.frame_seq,
                pkt.chunk_idx,
            );
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
                    // Wholesale-gap detection. As the high-water mark advances
                    // from its previous value to `seq`, every intermediate seq
                    // is a frame we should have seen. Classify each one:
                    //   * still pending as a `Partial` → skip; it will time out
                    //     and be reported by `purge`, which owns that loss (so
                    //     counting it here would double-count).
                    //   * already in `completed` → skip; it arrived out of order.
                    //   * otherwise → no chunk of it ever arrived: a wholesale
                    //     gap.
                    // Seqs an earlier `purge` skipped already sit at/below the
                    // high-water mark, so they fall outside this exclusive
                    // (prev_high_water, seq) range and are never revisited —
                    // no double count with purge. The high-water mark only
                    // moves forward, so a given seq falls in exactly one such
                    // range and is counted at most once. The first completion
                    // merely anchors the baseline (mid-stream join guard).
                    let prev_high_water = self.high_water_seq;
                    if self.saw_first_completion && seq > prev_high_water.saturating_add(1) {
                        for gap_seq in (prev_high_water + 1)..seq {
                            if !self.partials.contains_key(&gap_seq)
                                && !self.completed.contains(&gap_seq)
                            {
                                self.wholesale_gaps = self.wholesale_gaps.saturating_add(1);
                            }
                        }
                    }
                    self.saw_first_completion = true;
                    self.high_water_seq = self.high_water_seq.max(seq);
                    self.partials.remove(&seq);
                    self.completed.insert(seq);
                    self.prune_completed();
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

        if prdt_protocol::frame_trace::enabled() {
            let m = entry.parity_chunks as usize;
            let fnv = prdt_protocol::frame_trace::fnv1a64(&buf);
            tracing::info!(
                target: "frame.trace",
                "asm seq={seq} k={k} m={m} reconstructed={missing_source} len={total_bytes} fnv={fnv:016x}"
            );
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

    #[test]
    fn late_parity_after_completion_is_not_reemitted() {
        // 50 bytes at chunk_payload_len=1200 → k=1, m=2 (min_m floor),
        // total=3. Matches the field-observed shape (asm seq=6 k=1) that
        // produced a duplicate POC: source chunk completes the frame, then a
        // parity chunk for the same seq arrives late and must not re-emit it.
        let fec = FecCodec::new(1, 2).unwrap();
        let policy = FecPolicy::standard();
        let frame = make_frame(6, &[0xEE; 50]);
        let pkts = packetize(&frame, 1200, &policy).unwrap();
        assert_eq!(pkts.len(), 3);
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);

        let r1 = asm.feed(pkts[0].clone(), &fec).unwrap();
        assert!(
            matches!(r1, FeedResult::Complete(_)),
            "source chunk should complete the frame, got {:?}",
            r1
        );

        // Late parity chunk for the already-completed seq must be dropped,
        // not re-complete/re-emit the frame.
        let r2 = asm.feed(pkts[1].clone(), &fec).unwrap();
        assert!(
            !matches!(r2, FeedResult::Complete(_)),
            "late parity after completion must not re-emit the frame, got {:?}",
            r2
        );
    }

    #[test]
    fn late_duplicate_source_after_completion_is_dropped() {
        // Same k=1, m=1 shape, but the duplicate is a re-delivered copy of
        // the source chunk itself rather than parity (e.g. a network-level
        // retransmit), which must be dropped identically.
        let fec = FecCodec::new(1, 1).unwrap();
        let policy = FecPolicy::standard();
        let frame = make_frame(6, &[0xEE; 50]);
        let pkts = packetize(&frame, 1200, &policy).unwrap();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);

        let r1 = asm.feed(pkts[0].clone(), &fec).unwrap();
        assert!(matches!(r1, FeedResult::Complete(_)));

        let r2 = asm.feed(pkts[0].clone(), &fec).unwrap();
        assert!(
            !matches!(r2, FeedResult::Complete(_)),
            "duplicate source chunk after completion must not re-emit the frame, got {:?}",
            r2
        );
    }

    #[test]
    fn wholesale_gap_counts_never_seen_seqs() {
        // Complete seq 0 (baseline), then seq 3 while seqs 1 and 2 were never
        // seen at all → 2 wholesale gaps. take_wholesale_gaps resets the count.
        let fec = FecCodec::new(1, 2).unwrap();
        let policy = FecPolicy::standard();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);

        let f0 = make_frame(0, &[0xAA; 50]);
        let p0 = packetize(&f0, 1200, &policy).unwrap();
        assert!(matches!(
            asm.feed(p0[0].clone(), &fec).unwrap(),
            FeedResult::Complete(_)
        ));
        // Baseline established; no gaps counted for the first completion.
        assert_eq!(asm.take_wholesale_gaps(), 0);

        let f3 = make_frame(3, &[0xBB; 50]);
        let p3 = packetize(&f3, 1200, &policy).unwrap();
        assert!(matches!(
            asm.feed(p3[0].clone(), &fec).unwrap(),
            FeedResult::Complete(_)
        ));
        // seqs 1 and 2 never arrived → 2 wholesale gaps.
        assert_eq!(asm.take_wholesale_gaps(), 2);
        // Draining resets the counter.
        assert_eq!(asm.take_wholesale_gaps(), 0);
    }

    #[test]
    fn partial_then_purged_seq_is_not_counted_as_wholesale_gap() {
        // seq 1 arrives as an incomplete partial (1 of 2 source chunks), so it
        // never completes and will purge. When seq 2 completes and high-water
        // passes seq 1, seq 1 must NOT be counted as a wholesale gap — the
        // purge path owns that loss and reports it separately.
        let fec = FecCodec::new(2, 2).unwrap();
        let policy = FecPolicy::strict_small();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);
        asm.set_timeout(Duration::from_millis(1));

        // seq 0: complete (feed both source chunks) → anchors the baseline.
        let f0 = make_frame(0, &[0x11; 200]);
        let p0 = packetize(&f0, 100, &policy).unwrap();
        let mut done0 = false;
        for p in p0.into_iter().take(2) {
            if matches!(asm.feed(p, &fec).unwrap(), FeedResult::Complete(_)) {
                done0 = true;
            }
        }
        assert!(done0, "seq 0 should complete");

        // seq 1: only one of two source chunks → stays a pending partial.
        let f1 = make_frame(1, &[0x22; 200]);
        let p1 = packetize(&f1, 100, &policy).unwrap();
        assert!(matches!(
            asm.feed(p1[0].clone(), &fec).unwrap(),
            FeedResult::Pending
        ));

        // seq 2: completes while seq 1 is still a pending partial.
        let f2 = make_frame(2, &[0x33; 200]);
        let p2 = packetize(&f2, 100, &policy).unwrap();
        let mut done2 = false;
        for p in p2.into_iter().take(2) {
            if matches!(asm.feed(p, &fec).unwrap(), FeedResult::Complete(_)) {
                done2 = true;
            }
        }
        assert!(done2, "seq 2 should complete");
        assert_eq!(
            asm.take_wholesale_gaps(),
            0,
            "pending partial owned by purge, not a wholesale gap"
        );

        // seq 1 now times out and is purged; purge reports the loss.
        std::thread::sleep(Duration::from_millis(5));
        let purged = asm.purge();
        assert_eq!(purged, vec![1]);
        assert_eq!(
            asm.take_wholesale_gaps(),
            0,
            "purge must not add a wholesale gap"
        );
    }

    #[test]
    fn completed_set_is_pruned() {
        // Complete many far-apart seqs and confirm the completed-set
        // doesn't grow unboundedly: it should stay within one
        // STALE_SEQ_WINDOW's worth of entries of the current high-water mark.
        let fec = FecCodec::new(1, 1).unwrap();
        let policy = FecPolicy::standard();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);

        for seq in 0..200u64 {
            let frame = make_frame(seq, &[0xAB; 50]);
            let pkts = packetize(&frame, 1200, &policy).unwrap();
            let r = asm.feed(pkts[0].clone(), &fec).unwrap();
            assert!(
                matches!(r, FeedResult::Complete(_)),
                "seq {seq} should complete, got {:?}",
                r
            );
        }

        assert!(
            asm.completed_len() <= STALE_SEQ_WINDOW as usize + 1,
            "completed set should stay bounded to ~STALE_SEQ_WINDOW entries, got {}",
            asm.completed_len()
        );
    }
}
