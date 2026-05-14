use std::time::Duration;

use bytes::Bytes;
use prdt_protocol::{control::ControlMessage, frame::Codec, EncodedFrame, InputEvent, MouseButton};
use prdt_transport::{
    CustomUdpTransport, FecPolicy, ReceivedMessage, Transport, UdpTransportConfig,
};

#[tokio::test]
async fn udp_round_trip_control() {
    let cfg = UdpTransportConfig {
        session_id: 0xAA,
        ..Default::default()
    };
    let a = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
        .await
        .unwrap();
    let b = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
        .await
        .unwrap();

    let a_addr = a.local_addr().unwrap();
    let b_addr = b.local_addr().unwrap();
    a.configure_peer(b_addr).await;
    b.configure_peer(a_addr).await;

    a.send_control(ControlMessage::RequestIdr).await.unwrap();
    let m = tokio::time::timeout(Duration::from_millis(500), b.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        m,
        ReceivedMessage::Control(ControlMessage::RequestIdr)
    ));
}

#[tokio::test]
async fn udp_round_trip_input() {
    let cfg = UdpTransportConfig {
        session_id: 1,
        ..Default::default()
    };
    let a = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
        .await
        .unwrap();
    let b = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
        .await
        .unwrap();
    let b_addr = b.local_addr().unwrap();
    a.configure_peer(b_addr).await;
    b.configure_peer(a.local_addr().unwrap()).await;

    a.send_input(InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
    })
    .await
    .unwrap();
    let m = tokio::time::timeout(Duration::from_millis(500), b.recv())
        .await
        .unwrap()
        .unwrap();
    match m {
        ReceivedMessage::Input(InputEvent::MouseButton { button, pressed }) => {
            assert_eq!(button, MouseButton::Left);
            assert!(pressed);
        }
        other => panic!("unexpected {:?}", other),
    }
}

#[tokio::test]
async fn udp_round_trip_video_small_frame() {
    let cfg = UdpTransportConfig {
        session_id: 2,
        fec_policy: FecPolicy::strict_small(),
        ..Default::default()
    };
    let a = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
        .await
        .unwrap();
    let b = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
        .await
        .unwrap();
    let b_addr = b.local_addr().unwrap();
    a.configure_peer(b_addr).await;
    b.configure_peer(a.local_addr().unwrap()).await;

    let frame = EncodedFrame {
        seq: 1,
        timestamp_host_us: 42,
        is_keyframe: true,
        nal_units: Bytes::copy_from_slice(&[0xAA; 500]),
        width: 1920,
        height: 1080,
        codec: Codec::H265,
    };
    a.send_video(frame.clone()).await.unwrap();
    let m = tokio::time::timeout(Duration::from_secs(1), b.recv())
        .await
        .unwrap()
        .unwrap();
    match m {
        ReceivedMessage::Video(got) => {
            assert_eq!(got.seq, 1);
            assert!(got.is_keyframe);
            assert_eq!(&got.nal_units[..], &[0xAA; 500][..]);
        }
        other => panic!("unexpected {:?}", other),
    }
}
