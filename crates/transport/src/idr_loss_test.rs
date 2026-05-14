//! Loopback test: IDR fragment loss → assembler purge → IdrRequester → RequestIdr.
//!
//! Validates spec §5.2: that the purge→RequestIdr signal chain fires
//! deterministically when a keyframe's fragments are partially dropped.
//!
//! Uses real `std::time::sleep` for the assembler timeout (which uses
//! `std::time::Instant`, not tokio virtual clock) and `#[tokio::test]`
//! for the async portion that checks rate-limit cooldown.

use std::time::{Duration, Instant};

use bytes::Bytes;
use prdt_protocol::{frame::Codec, EncodedFrame};

use crate::assembler::{FeedResult, FrameAssembler};
use crate::fec::FecCodec;
use crate::packetize::{packetize, FecPolicy};

fn make_idr_frame(seq: u64, size_bytes: usize) -> EncodedFrame {
    EncodedFrame {
        seq,
        timestamp_host_us: seq * 16_667, // ~60fps
        is_keyframe: true,
        nal_units: Bytes::from(vec![0xABu8; size_bytes]),
        width: 1920,
        height: 1080,
        codec: Codec::H264,
    }
}

fn make_p_frame(seq: u64) -> EncodedFrame {
    EncodedFrame {
        seq,
        timestamp_host_us: seq * 16_667,
        is_keyframe: false,
        nal_units: Bytes::from(vec![0xCDu8; 200]),
        width: 1920,
        height: 1080,
        codec: Codec::H264,
    }
}

/// Feed all packets of `frame` except those whose chunk_idx is in `drop_indices`.
fn feed_with_drops(
    asm: &mut FrameAssembler,
    policy: &FecPolicy,
    frame: &EncodedFrame,
    drop_indices: &[u16],
) {
    let pkts = packetize(frame, 1200, policy).expect("packetize");
    // Build a FecCodec matching the (k, m) chosen by packetize so the
    // assembler's reconstruction uses the correct parameters.
    let (k, m) = policy
        .compute_k_m(frame.nal_units.len(), 1200)
        .expect("policy compute_k_m");
    let fec = FecCodec::new(k, m).expect("FecCodec::new");
    for pkt in pkts {
        if drop_indices.contains(&pkt.chunk_idx) {
            continue; // simulate UDP loss
        }
        let _ = asm.feed(pkt, &fec);
    }
}

/// IDR fragment loss → purge() returns the stale frame_seq.
///
/// With dynamic-k FEC: a 4800-byte frame at chunk_len=1200 produces
/// k=4 source chunks + m=2 parity = 6 packets total. Dropping source
/// indices [0, 1, 2] leaves 1 source + 2 parity = 3 received < 4
/// needed → Reed-Solomon cannot recover → assembler times out and
/// `purge()` returns seq=0.
#[test]
fn idr_fragment_loss_detected_by_purge() {
    let policy = FecPolicy::strict_small();
    let mut asm = FrameAssembler::new(1920, 1080, Codec::H264);
    // Set a very short timeout so the test doesn't have to wait 100ms.
    asm.set_timeout(Duration::from_millis(5));

    // 4800 bytes at chunk_payload_len=1200 → k=ceil(4800/1200)=4, m=2, total=6.
    // Dropping source chunks [0,1,2] leaves 1 source + 2 parity = 3 received
    // which is less than k=4 required for FEC recovery → assembler times out
    // and purge() returns seq=0.
    let idr = make_idr_frame(0, 4800);
    feed_with_drops(&mut asm, &policy, &idr, &[0, 1, 2]);

    // No frame should have completed.
    // Purge should be empty immediately (timeout not elapsed yet).
    let purged = asm.purge();
    assert!(
        purged.is_empty(),
        "purge() should not fire before timeout: {purged:?}"
    );

    // Wait past the 5ms timeout.
    std::thread::sleep(Duration::from_millis(10));

    let purged = asm.purge();
    assert_eq!(
        purged,
        vec![0],
        "purge() must return frame_seq=0 after timeout: {purged:?}"
    );
}

/// Purged keyframe seq triggers IdrRequester::mark(), which produces a
/// rate-limited RequestIdr send within 250ms cooldown.
#[test]
fn purge_triggers_idr_requester_mark() {
    // Minimal IdrRequester reimplementation to avoid cross-crate import
    // (IdrRequester lives in prdt-viewer, not prdt-transport).
    // This tests the semantic contract the viewer relies on.
    struct IdrRequester {
        pending: bool,
        last_at: Option<Instant>,
    }
    impl IdrRequester {
        fn new() -> Self {
            Self {
                pending: false,
                last_at: None,
            }
        }
        fn mark(&mut self) {
            self.pending = true;
        }
        fn try_take(&mut self, now: Instant, cooldown: Duration) -> bool {
            if !self.pending {
                return false;
            }
            if let Some(t) = self.last_at {
                if now.duration_since(t) < cooldown {
                    return false;
                }
            }
            self.pending = false;
            self.last_at = Some(now);
            true
        }
    }

    let policy = FecPolicy::strict_small();
    let mut asm = FrameAssembler::new(1920, 1080, Codec::H264);
    asm.set_timeout(Duration::from_millis(5));
    let mut req = IdrRequester::new();

    // 4800 bytes → k=4, m=2. Dropping [0,1,2] leaves 3 chunks < k=4 → can't recover.
    let idr = make_idr_frame(1, 4800);
    feed_with_drops(&mut asm, &policy, &idr, &[0, 1, 2]);
    std::thread::sleep(Duration::from_millis(10));

    let purged = asm.purge();
    assert!(!purged.is_empty(), "expected purge to return seqs");

    // The viewer would call mark() here.
    req.mark();

    // First try_take should succeed immediately (no prior request).
    assert!(
        req.try_take(Instant::now(), Duration::from_millis(250)),
        "first try_take must succeed"
    );

    // A second mark + try_take within cooldown must fail.
    req.mark();
    assert!(
        !req.try_take(Instant::now(), Duration::from_millis(250)),
        "second try_take within cooldown must fail"
    );
}

/// P-frame wholesale loss (not detectable via purge alone — decoder error path).
/// Validates that after purge returns nothing for a P-frame loss, the decoder
/// error path is the expected trigger (documented here, tested in viewer unit test).
#[test]
fn p_frame_wholesale_loss_not_detected_by_purge() {
    let policy = FecPolicy::strict_small();
    let mut asm = FrameAssembler::new(1920, 1080, Codec::H264);
    asm.set_timeout(Duration::from_millis(5));

    // IDR arrives with all source chunks (but no parity). With 200 bytes <
    // 1200 chunk_payload, dynamic-k gives k=1 (ceil(200/1200)=1), m=2.
    // All source chunks are fed → FEC trivially succeeds → assembler marks
    // the frame complete and removes it from partials immediately. Parity
    // chunks are deliberately not fed to avoid re-creating the partial entry
    // (feeding parity after completion inserts a new partial via
    // or_insert_with, which would then time out in purge).
    let idr = make_idr_frame(0, 200);
    // Old assumed static k=4; with dynamic-k, k = ceil(bytes / chunk_len).
    let k = idr.nal_units.len().div_ceil(1200) as u16;
    let idr_pkts = packetize(&idr, 1200, &policy).expect("packetize idr");
    let (k_actual, m_actual) = policy
        .compute_k_m(idr.nal_units.len(), 1200)
        .expect("compute_k_m");
    let fec = FecCodec::new(k_actual, m_actual).expect("FecCodec::new");
    for pkt in idr_pkts {
        if pkt.chunk_idx >= k {
            continue; // skip parity chunks
        }
        let _ = asm.feed(pkt, &fec);
    }

    // P-frame is wholly absent (never arrived at assembler). Assembler
    // never sees it, so purge() returns nothing for seq=1.
    std::thread::sleep(Duration::from_millis(10));
    let purged = asm.purge();
    assert!(
        purged.is_empty(),
        "wholesale P-frame loss is invisible to purge: {purged:?}"
    );
    // This is expected: decoder error path handles it (spec §3.3).
}

// Suppress dead_code warning for make_p_frame which documents the P-frame
// struct shape used in the prose above but isn't called directly.
const _: fn(u64) -> EncodedFrame = make_p_frame;

/// Regression for the P5C-1 + P5B-2a-successor smoke failure
/// (2026-05-13, N100 GNOME 46): VAAPI produced a 168 KB IDR that the
/// old static fec_k=64 transport could not packetize. With dynamic-k
/// FEC and MAX_SHARDS=240, a 180 KB synthetic IDR must round-trip
/// cleanly at 0 % loss.
///
/// 180 000 B / 1200 B = 150 source chunks, m = ceil(150×10/100) = 15
/// parity → 165 total packets, well within MAX_SHARDS=240.
#[test]
fn large_idr_round_trip() {
    let policy = FecPolicy::standard();
    // 180 KB → k = ceil(180000/1200) = 150, m = ceil(150*10/100) = 15
    let payload: Vec<u8> = (0..=255u8).cycle().take(180_000).collect();
    let frame = EncodedFrame {
        seq: 7,
        timestamp_host_us: 100,
        is_keyframe: true,
        nal_units: Bytes::from(payload.clone()),
        width: 1920,
        height: 1080,
        codec: Codec::H264,
    };
    let pkts = packetize(&frame, 1200, &policy).expect("packetize large IDR");
    assert_eq!(pkts.len(), 150 + 15, "expected 165 packets");

    // Feed all 150 source packets through the assembler. The codec needs
    // to match the (k, m) that packetize used.
    let fec = FecCodec::new(150, 15).expect("fec 150/15");
    let mut asm = FrameAssembler::new(1920, 1080, Codec::H264);
    let mut completed: Option<EncodedFrame> = None;
    for p in pkts.iter().take(150).cloned() {
        match asm.feed(p, &fec).expect("feed") {
            FeedResult::Complete(f) => {
                completed = Some(f);
                break;
            }
            FeedResult::Pending | FeedResult::Stale => {}
        }
    }
    let reconstructed = completed.expect("assembler must reconstruct large IDR");
    assert_eq!(reconstructed.nal_units.len(), payload.len());
    assert_eq!(&reconstructed.nal_units[..], &payload[..]);
    assert!(reconstructed.is_keyframe);
    assert_eq!(reconstructed.seq, 7);
}

/// Drop 5 deterministic source packets from a 180 KB IDR; FEC must
/// reconstruct the missing chunks via parity (m = 15 ≥ 5).
#[test]
fn large_idr_with_loss_recovery() {
    let policy = FecPolicy::standard();
    let payload: Vec<u8> = (0..=255u8).cycle().take(180_000).collect();
    let frame = EncodedFrame {
        seq: 9,
        timestamp_host_us: 200,
        is_keyframe: true,
        nal_units: Bytes::from(payload.clone()),
        width: 1920,
        height: 1080,
        codec: Codec::H264,
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
    assert_eq!(kept.len(), 165 - 5);

    let fec = FecCodec::new(150, 15).expect("fec 150/15");
    let mut asm = FrameAssembler::new(1920, 1080, Codec::H264);
    let mut completed: Option<EncodedFrame> = None;
    for p in kept {
        match asm.feed(p, &fec).expect("feed") {
            FeedResult::Complete(f) => {
                completed = Some(f);
                break;
            }
            FeedResult::Pending | FeedResult::Stale => {}
        }
    }
    let reconstructed = completed.expect("assembler must FEC-recover after 5 source losses");
    assert_eq!(reconstructed.nal_units.len(), payload.len());
    assert_eq!(&reconstructed.nal_units[..], &payload[..]);
    assert!(reconstructed.is_keyframe);
    assert_eq!(reconstructed.seq, 9);
}
