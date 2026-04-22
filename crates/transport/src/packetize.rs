use prdt_protocol::{wire::video_flags, EncodedFrame, VideoPacket, DEFAULT_CHUNK_PAYLOAD_LEN};

use crate::error::TransportError;
use crate::fec::FecCodec;

/// Max source chunks per frame. Raised from spec §5.3's 32 to 128 to
/// accommodate 4K60 at high bitrates (Plan 3 smoke feedback: 20Mbps
/// H.265 IDRs hit 40KB+). The hard limit comes from reed-solomon GF(8)
/// which supports k+m <= 255, so 128 leaves headroom. Exceeding this
/// should trigger IDR + bitrate drop at a higher layer (deferred to
/// Plan 4's dynamic FEC sizing).
pub const MAX_SOURCE_CHUNKS: usize = 128;

/// Split an EncodedFrame into k source chunks, then apply FEC to produce
/// m parity chunks. Returns exactly k + m VideoPackets.
///
/// All chunks use the SAME `chunk_payload` byte length (padded with zeros
/// on the last source chunk). The original frame byte length is preserved
/// indirectly through `payload_bytes` which records the true valid bytes
/// per chunk.
pub fn packetize(
    frame: &EncodedFrame,
    fec: &FecCodec,
    chunk_payload_len: usize,
) -> Result<Vec<VideoPacket>, TransportError> {
    let k = fec.k();
    let m = fec.m();

    // How many source chunks are needed?
    let bytes = frame.nal_units.len();
    let chunks_needed = bytes.div_ceil(chunk_payload_len);
    if chunks_needed > k {
        return Err(TransportError::FrameTooLarge {
            bytes,
            max_bytes: k * chunk_payload_len,
        });
    }
    if chunks_needed > MAX_SOURCE_CHUNKS {
        return Err(TransportError::FrameTooLarge {
            bytes,
            max_bytes: MAX_SOURCE_CHUNKS * chunk_payload_len,
        });
    }

    // Build k source shards, each of fixed length chunk_payload_len.
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

    // Emit k + m VideoPackets.
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
    // Silence unused-const warning if DEFAULT_CHUNK_PAYLOAD_LEN is only referenced
    // via the protocol re-export.
    let _ = DEFAULT_CHUNK_PAYLOAD_LEN;
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
        let fec = FecCodec::new(4, 2).unwrap();
        let payload = vec![0xAB; 10];
        let pkts = packetize(&make_frame(&payload), &fec, 100).unwrap();
        assert_eq!(pkts.len(), 6); // k + m
        assert_eq!(pkts[0].chunk_idx, 0);
        assert_eq!(pkts[0].source_chunks, 4);
        assert_eq!(pkts[0].parity_chunks, 2);
        assert!(pkts[0].is_keyframe());
        assert!(!pkts[0].is_parity());
        assert_eq!(pkts[0].payload_bytes, 10);
        assert_eq!(pkts[0].chunk_payload[..10], [0xAB; 10]);
        // rest of the shard is zero-padded
        assert_eq!(pkts[0].chunk_payload[10..], [0u8; 90]);
        // parity flag
        assert!(pkts[4].is_parity());
        assert!(pkts[5].is_parity());
    }

    #[test]
    fn packetize_frame_spanning_multiple_chunks() {
        let fec = FecCodec::new(4, 2).unwrap();
        let payload: Vec<u8> = (0..=255).cycle().take(350).collect();
        let pkts = packetize(&make_frame(&payload), &fec, 100).unwrap();
        assert_eq!(pkts.len(), 6);
        // chunk 0..=2 are full, chunk 3 has 50 valid bytes
        assert_eq!(pkts[0].payload_bytes, 100);
        assert_eq!(pkts[1].payload_bytes, 100);
        assert_eq!(pkts[2].payload_bytes, 100);
        assert_eq!(pkts[3].payload_bytes, 50);
    }

    #[test]
    fn packetize_rejects_oversize() {
        let fec = FecCodec::new(2, 1).unwrap();
        let huge = vec![0u8; 500]; // needs 5 chunks at 100B but k=2
        let err = packetize(&make_frame(&huge), &fec, 100).unwrap_err();
        assert!(matches!(err, TransportError::FrameTooLarge { .. }));
    }
}
