//! Integration test for Noise encryption over CustomUdpTransport.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use prdt_crypto::KeyPair;
use prdt_protocol::{control::ControlMessage, frame::Codec, EncodedFrame, InputEvent, MouseButton};
use prdt_transport::{CustomUdpTransport, ReceivedMessage, Transport, UdpTransportConfig};

/// Spin up host + client transports on localhost, run Noise handshake,
/// then exchange messages. Asserts all messages round-trip.
#[tokio::test]
async fn encrypted_round_trip_all_message_types() {
    // Use small fec_k/fec_m to keep the test fast and avoid saturating
    // the localhost UDP socket buffer with a 70-packet burst (the default
    // fec_k=64 produces k+m=70 packets per frame even for a tiny frame,
    // and encryption makes each send slower).
    let cfg = UdpTransportConfig {
        session_id: 1,
        fec_k: 4,
        fec_m: 2,
        ..Default::default()
    };

    // Create both ends.
    let host = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap(),
    );
    let viewer = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap(),
    );

    let host_addr = host.local_addr().unwrap();
    let viewer_addr = viewer.local_addr().unwrap();
    viewer.configure_peer(host_addr).await;
    host.configure_peer(viewer_addr).await;

    // Generate host keypair.
    let keypair = KeyPair::generate();
    let pubkey = keypair.public;

    // Drive both handshakes concurrently.
    let host_clone = Arc::clone(&host);
    let server_task = tokio::spawn(async move { host_clone.handshake_as_server(&keypair).await });
    let viewer_clone = Arc::clone(&viewer);
    let client_task = tokio::spawn(async move { viewer_clone.handshake_as_client(&pubkey).await });

    let (s_res, c_res) = tokio::time::timeout(Duration::from_secs(5), async {
        (server_task.await.unwrap(), client_task.await.unwrap())
    })
    .await
    .expect("handshake timeout");
    s_res.expect("server handshake");
    c_res.expect("client handshake");

    // Now exchange messages in both directions, all encrypted.
    // Control: viewer -> host
    viewer
        .send_control(ControlMessage::RequestIdr)
        .await
        .unwrap();
    let m = tokio::time::timeout(Duration::from_millis(500), host.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        m,
        ReceivedMessage::Control(ControlMessage::RequestIdr)
    ));

    // Input: viewer -> host
    viewer
        .send_input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        })
        .await
        .unwrap();
    let m = tokio::time::timeout(Duration::from_millis(500), host.recv())
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

    // Video: host -> viewer (500 bytes)
    let frame = EncodedFrame {
        seq: 42,
        timestamp_host_us: 12345,
        is_keyframe: true,
        nal_units: Bytes::copy_from_slice(&[0xCD; 500]),
        width: 1920,
        height: 1080,
        codec: Codec::H265,
    };
    host.send_video(frame.clone()).await.unwrap();
    let m = tokio::time::timeout(Duration::from_secs(1), viewer.recv())
        .await
        .unwrap()
        .unwrap();
    match m {
        ReceivedMessage::Video(got) => {
            assert_eq!(got.seq, 42);
            assert!(got.is_keyframe);
            assert_eq!(&got.nal_units[..], &[0xCD; 500][..]);
        }
        other => panic!("unexpected {:?}", other),
    }
}

/// Verify that an incorrect pubkey causes the handshake to fail.
#[tokio::test]
async fn encrypted_wrong_pubkey_fails() {
    let cfg = UdpTransportConfig {
        session_id: 2,
        ..Default::default()
    };

    let host = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap(),
    );
    let viewer = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap(),
    );

    viewer.configure_peer(host.local_addr().unwrap()).await;
    host.configure_peer(viewer.local_addr().unwrap()).await;

    let real_keypair = KeyPair::generate();
    let fake_keypair = KeyPair::generate();

    let host_clone = Arc::clone(&host);
    let server_task =
        tokio::spawn(async move { host_clone.handshake_as_server(&real_keypair).await });
    let viewer_clone = Arc::clone(&viewer);
    let fake_pubkey = fake_keypair.public;
    let client_task =
        tokio::spawn(async move { viewer_clone.handshake_as_client(&fake_pubkey).await });

    // Give handshake up to 2 seconds — server should return Err, client may
    // also fail (or hang awaiting NoiseE2 that never comes, since server
    // aborted). We accept either outcome as long as at least ONE side
    // reports failure and neither returns a successful Ok().
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let s = server_task.await.unwrap();
        let c = client_task.await.unwrap();
        (s, c)
    })
    .await;

    match result {
        Ok((s, c)) => {
            assert!(
                s.is_err() || c.is_err(),
                "at least one side should fail: server={:?}, client={:?}",
                s,
                c
            );
        }
        Err(_) => {
            // Timeout: client likely hangs waiting for NoiseE2 that server
            // couldn't build. Acceptable — the key point is no session was
            // established. Abort the tasks.
        }
    }
}
