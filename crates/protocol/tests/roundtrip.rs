//! End-to-end tests treating the protocol as an opaque byte-stream.

use prdt_protocol::{
    control::{ControlMessage, PermissionSet},
    decode_control, encode_control,
    input::MouseButton,
    wire::{self, video_flags, InputPacket, PacketHeader, PacketType, VideoPacket, HEADER_LEN},
    InputEvent, MonitorRect, ProtocolError,
};

fn build_video_datagram(session_id: u64, pkt: &VideoPacket) -> Vec<u8> {
    let body = pkt.encode();
    let hdr = PacketHeader {
        packet_type: PacketType::Video,
        flags: 0,
        session_id,
        payload_len: body.len() as u32,
    };
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&hdr.encode());
    out.extend_from_slice(&body);
    out
}

#[test]
fn full_video_datagram_round_trip() {
    let chunk = VideoPacket {
        frame_seq: 100,
        timestamp_host_us: 9_999_999,
        chunk_idx: 0,
        source_chunks: 8,
        parity_chunks: 2,
        video_flags: video_flags::IS_KEYFRAME,
        payload_bytes: 4,
        chunk_payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
    };
    let datagram = build_video_datagram(0xDEAD_BEEF_CAFE_BABE, &chunk);
    assert_eq!(datagram.len(), HEADER_LEN + wire::VIDEO_PAYLOAD_HDR_LEN + 4);

    let hdr = PacketHeader::decode(&datagram).unwrap();
    assert_eq!(hdr.packet_type, PacketType::Video);
    assert_eq!(hdr.session_id, 0xDEAD_BEEF_CAFE_BABE);
    assert_eq!(hdr.payload_len as usize, wire::VIDEO_PAYLOAD_HDR_LEN + 4);

    let body = &datagram[HEADER_LEN..HEADER_LEN + hdr.payload_len as usize];
    let back = VideoPacket::decode(body).unwrap();
    assert_eq!(back, chunk);
}

#[test]
fn full_input_datagram_round_trip() {
    let pkt = InputPacket {
        input_seq: 7,
        timestamp_viewer_us: 1_000,
        event: InputEvent::MouseButton {
            button: MouseButton::Right,
            pressed: true,
        },
    };
    let body = pkt.encode();
    let hdr = PacketHeader {
        packet_type: PacketType::Input,
        flags: 0,
        session_id: 42,
        payload_len: body.len() as u32,
    };
    let mut datagram = Vec::from(hdr.encode());
    datagram.extend_from_slice(&body);

    let parsed_hdr = PacketHeader::decode(&datagram).unwrap();
    assert_eq!(parsed_hdr.packet_type, PacketType::Input);
    let back =
        InputPacket::decode(&datagram[HEADER_LEN..HEADER_LEN + parsed_hdr.payload_len as usize])
            .unwrap();
    assert_eq!(back, pkt);
}

#[test]
fn full_control_datagram_round_trip() {
    let msg = ControlMessage::HelloAck {
        session_id: 1,
        host_monotonic_base_us: 2,
        neg_width: 3840,
        neg_height: 2160,
        neg_fps: 60,
        neg_bitrate_bps: 50_000_000,
        host_monitor_rect: MonitorRect::new(0, 0, 3840, 2160),
        host_virtual_desktop_rect: MonitorRect::new(0, 0, 5760, 2160),
        negotiated_codec: prdt_protocol::frame::Codec::H265,
        host_supported_codecs: vec![prdt_protocol::frame::Codec::H265],
        granted_permissions: PermissionSet::all(),
    };
    let body = encode_control(&msg).unwrap();
    let hdr = PacketHeader {
        packet_type: PacketType::Control,
        flags: 0,
        session_id: 1,
        payload_len: body.len() as u32,
    };
    let mut datagram = Vec::from(hdr.encode());
    datagram.extend_from_slice(&body);

    let parsed_hdr = PacketHeader::decode(&datagram).unwrap();
    assert_eq!(parsed_hdr.packet_type, PacketType::Control);
    let back = decode_control(&datagram[HEADER_LEN..HEADER_LEN + parsed_hdr.payload_len as usize])
        .unwrap();
    assert_eq!(back, msg);
}

#[test]
fn datagram_rejects_corruption() {
    let mut buf = Vec::from(
        PacketHeader {
            packet_type: PacketType::Control,
            flags: 0,
            session_id: 1,
            payload_len: 0,
        }
        .encode(),
    );
    buf[0] = 0xFF;
    let err = PacketHeader::decode(&buf).unwrap_err();
    assert!(matches!(err, ProtocolError::BadMagic { .. }));
}

use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_video_packet_round_trip(
        frame_seq in 0u64..u64::MAX,
        ts in 0u64..u64::MAX,
        chunk_idx in 0u16..1024,
        source_chunks in 1u16..32,
        parity_chunks in 0u16..8,
        is_kf in any::<bool>(),
        payload in prop::collection::vec(any::<u8>(), 0..=1200),
    ) {
        let flags = if is_kf { video_flags::IS_KEYFRAME } else { 0 };
        let pkt = VideoPacket {
            frame_seq,
            timestamp_host_us: ts,
            chunk_idx,
            source_chunks,
            parity_chunks,
            video_flags: flags,
            payload_bytes: payload.len() as u16,
            chunk_payload: payload.clone(),
        };
        let buf = pkt.encode();
        let back = VideoPacket::decode(&buf).unwrap();
        prop_assert_eq!(back.frame_seq, frame_seq);
        prop_assert_eq!(back.timestamp_host_us, ts);
        prop_assert_eq!(back.chunk_idx, chunk_idx);
        prop_assert_eq!(back.source_chunks, source_chunks);
        prop_assert_eq!(back.parity_chunks, parity_chunks);
        prop_assert_eq!(back.video_flags, flags);
        prop_assert_eq!(back.chunk_payload, payload);
    }
}
