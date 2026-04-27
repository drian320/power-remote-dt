use std::time::Duration;

use prdt_protocol::{control::ControlMessage, frame::Codec, MonitorRect};

use crate::error::TransportError;
use crate::transport_trait::{ReceivedMessage, Transport};

pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(3);
pub const DEFAULT_HELLO_RETRIES: u8 = 3;

/// Wire-level protocol_version that this build of the codebase speaks.
/// Bumped to 2 in the software-codec / codec-negotiation phase.
pub const HELLO_PROTOCOL_VERSION: u8 = 2;

#[derive(Debug, Clone)]
pub struct HelloRequest {
    pub req_width: u32,
    pub req_height: u32,
    pub req_fps: u32,
    /// Post-Phase-0 semantics: this is the codec the host negotiated for
    /// the session (i.e. what the host will encode with). The field name
    /// is preserved from the pre-Phase-0 wire format where it carried the
    /// viewer's preferred codec; the host now accepts the viewer's request
    /// only if the codec is in its supported set, otherwise it replies
    /// HelloReject and the handshake fails.
    pub codec: Codec,
}

#[derive(Debug, Clone)]
pub struct SessionAck {
    pub session_id: u64,
    pub host_monotonic_base_us: u64,
    pub neg_width: u32,
    pub neg_height: u32,
    pub neg_fps: u32,
    pub neg_bitrate_bps: u32,
    pub host_monitor_rect: MonitorRect,
    pub host_virtual_desktop_rect: MonitorRect,
    pub negotiated_codec: Codec,
    pub host_supported_codecs: Vec<Codec>,
}

/// Send Hello, await HelloAck (or HelloReject). Retries on timeout, returns
/// session info on success. Returns `HelloRejected` immediately if the host
/// replies with HelloReject — there's no point retrying a rejection.
pub async fn viewer_handshake<T: Transport>(
    transport: &T,
    req: &HelloRequest,
    per_attempt_timeout: Duration,
    retries: u8,
) -> Result<SessionAck, TransportError> {
    for _ in 0..retries {
        let hello = ControlMessage::Hello {
            protocol_version: HELLO_PROTOCOL_VERSION,
            req_width: req.req_width,
            req_height: req.req_height,
            req_fps: req.req_fps,
            codec: req.codec,
        };
        transport.send_control(hello).await?;

        let ack_fut = async {
            loop {
                match transport.recv().await? {
                    ReceivedMessage::Control(ControlMessage::HelloAck {
                        session_id,
                        host_monotonic_base_us,
                        neg_width,
                        neg_height,
                        neg_fps,
                        neg_bitrate_bps,
                        host_monitor_rect,
                        host_virtual_desktop_rect,
                        negotiated_codec,
                        host_supported_codecs,
                    }) => {
                        return Ok::<SessionAck, TransportError>(SessionAck {
                            session_id,
                            host_monotonic_base_us,
                            neg_width,
                            neg_height,
                            neg_fps,
                            neg_bitrate_bps,
                            host_monitor_rect,
                            host_virtual_desktop_rect,
                            negotiated_codec,
                            host_supported_codecs,
                        });
                    }
                    ReceivedMessage::Control(ControlMessage::HelloReject { reason }) => {
                        return Err(TransportError::HelloRejected(reason));
                    }
                    // ignore other messages during handshake
                    _ => continue,
                }
            }
        };
        match tokio::time::timeout(per_attempt_timeout, ack_fut).await {
            Ok(r) => return r,
            Err(_) => continue, // retry on timeout only
        }
    }
    Err(TransportError::HandshakeTimeout)
}

/// Host-side: await Hello, respond with HelloAck or HelloReject.
///
/// `host_supported_codecs` is the full set of codecs this host can drive.
/// If `Hello.codec` is in the set, the handshake succeeds and the negotiated
/// codec is the viewer's request. Otherwise the host sends a HelloReject and
/// returns `Err(TransportError::HelloRejected(_))`.
#[allow(clippy::too_many_arguments)]
pub async fn host_handshake<T: Transport>(
    transport: &T,
    session_id: u64,
    host_monotonic_base_us: u64,
    negotiated_bitrate_bps: u32,
    host_monitor_rect: MonitorRect,
    host_virtual_desktop_rect: MonitorRect,
    host_supported_codecs: &[Codec],
    wait_timeout: Duration,
) -> Result<HelloRequest, TransportError> {
    let supported = host_supported_codecs.to_vec();
    let fut = async {
        loop {
            match transport.recv().await? {
                ReceivedMessage::Control(ControlMessage::Hello {
                    protocol_version,
                    req_width,
                    req_height,
                    req_fps,
                    codec,
                }) => {
                    if protocol_version != HELLO_PROTOCOL_VERSION {
                        // Tell the viewer why and surface UnsupportedVersion.
                        let reason = format!(
                            "host speaks protocol_version {}, viewer sent {}",
                            HELLO_PROTOCOL_VERSION, protocol_version
                        );
                        let _ = transport
                            .send_control(ControlMessage::HelloReject { reason })
                            .await;
                        return Err(TransportError::Protocol(
                            prdt_protocol::ProtocolError::UnsupportedVersion(protocol_version),
                        ));
                    }
                    if !supported.contains(&codec) {
                        let reason =
                            format!("host does not support {}", codec.name());
                        transport
                            .send_control(ControlMessage::HelloReject {
                                reason: reason.clone(),
                            })
                            .await?;
                        return Err(TransportError::HelloRejected(reason));
                    }
                    let ack = ControlMessage::HelloAck {
                        session_id,
                        host_monotonic_base_us,
                        neg_width: req_width,
                        neg_height: req_height,
                        neg_fps: req_fps,
                        neg_bitrate_bps: negotiated_bitrate_bps,
                        host_monitor_rect,
                        host_virtual_desktop_rect,
                        negotiated_codec: codec,
                        host_supported_codecs: supported.clone(),
                    };
                    transport.send_control(ack).await?;
                    return Ok(HelloRequest {
                        req_width,
                        req_height,
                        req_fps,
                        codec,
                    });
                }
                _ => continue,
            }
        }
    };
    match tokio::time::timeout(wait_timeout, fut).await {
        Ok(r) => r,
        Err(_) => Err(TransportError::HandshakeTimeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopback::{InProcTransport, LoopbackOptions};
    use prdt_protocol::frame::Codec;

    #[tokio::test]
    async fn handshake_happy_path() {
        let (viewer, host) = InProcTransport::pair(LoopbackOptions::default());

        let viewer_task = tokio::spawn(async move {
            viewer_handshake(
                &viewer,
                &HelloRequest {
                    req_width: 1920,
                    req_height: 1080,
                    req_fps: 60,
                    codec: Codec::H265,
                },
                Duration::from_millis(500),
                3,
            )
            .await
        });
        let host_task = tokio::spawn(async move {
            host_handshake(
                &host,
                0x1234,
                42,
                10_000_000,
                MonitorRect::new(0, 0, 1920, 1080),
                MonitorRect::new(0, 0, 3840, 1080),
                &[Codec::H265],
                Duration::from_millis(500),
            )
            .await
        });

        let (v, h) = tokio::join!(viewer_task, host_task);
        let ack = v.unwrap().unwrap();
        let req = h.unwrap().unwrap();
        assert_eq!(ack.session_id, 0x1234);
        assert_eq!(ack.neg_width, 1920);
        assert_eq!(ack.host_monitor_rect.width(), 1920);
        assert_eq!(ack.host_virtual_desktop_rect.width(), 3840);
        assert_eq!(ack.negotiated_codec, Codec::H265);
        assert_eq!(ack.host_supported_codecs, vec![Codec::H265]);
        assert_eq!(req.req_fps, 60);
        assert_eq!(req.codec, Codec::H265);
    }

    #[tokio::test]
    async fn handshake_timeout_when_no_ack() {
        // drop every control packet
        let (viewer, _host) = InProcTransport::pair(LoopbackOptions {
            drop_ppm: 1_000_000,
            latency: None,
        });

        let err = viewer_handshake(
            &viewer,
            &HelloRequest {
                req_width: 1920,
                req_height: 1080,
                req_fps: 60,
                codec: Codec::H265,
            },
            Duration::from_millis(50),
            2,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransportError::HandshakeTimeout));
    }

    #[tokio::test]
    async fn host_handshake_picks_h264_when_viewer_asks_for_h264() {
        let (viewer, host) = InProcTransport::pair(LoopbackOptions::default());

        let viewer_task = tokio::spawn(async move {
            viewer_handshake(
                &viewer,
                &HelloRequest {
                    req_width: 1920,
                    req_height: 1080,
                    req_fps: 60,
                    codec: Codec::H264,
                },
                Duration::from_millis(500),
                3,
            )
            .await
        });
        let host_task = tokio::spawn(async move {
            host_handshake(
                &host,
                0xAA,
                0,
                10_000_000,
                MonitorRect::new(0, 0, 1920, 1080),
                MonitorRect::new(0, 0, 1920, 1080),
                &[Codec::H265, Codec::H264],
                Duration::from_millis(500),
            )
            .await
        });

        let (v, h) = tokio::join!(viewer_task, host_task);
        let ack = v.unwrap().unwrap();
        let req = h.unwrap().unwrap();
        assert_eq!(ack.negotiated_codec, Codec::H264);
        assert_eq!(req.codec, Codec::H264);
        assert!(ack.host_supported_codecs.contains(&Codec::H265));
        assert!(ack.host_supported_codecs.contains(&Codec::H264));
    }

    #[tokio::test]
    async fn host_rejects_unsupported_codec() {
        let (viewer, host) = InProcTransport::pair(LoopbackOptions::default());

        let viewer_task = tokio::spawn(async move {
            viewer_handshake(
                &viewer,
                &HelloRequest {
                    req_width: 1920,
                    req_height: 1080,
                    req_fps: 60,
                    codec: Codec::Av1,
                },
                Duration::from_millis(500),
                3,
            )
            .await
        });
        let host_task = tokio::spawn(async move {
            host_handshake(
                &host,
                0xBB,
                0,
                10_000_000,
                MonitorRect::new(0, 0, 1920, 1080),
                MonitorRect::new(0, 0, 1920, 1080),
                &[Codec::H265, Codec::H264], // no AV1
                Duration::from_millis(500),
            )
            .await
        });

        // The viewer must observe a HelloRejected error within 100ms once
        // the host sends HelloReject — i.e. no waiting for the retry budget.
        let v_outcome =
            tokio::time::timeout(Duration::from_millis(100), viewer_task)
                .await
                .expect("viewer must observe rejection within 100ms");
        let v_err = v_outcome.unwrap().unwrap_err();
        match v_err {
            TransportError::HelloRejected(reason) => {
                assert!(
                    reason.contains("av1") || reason.contains("AV1"),
                    "reason should mention the codec: {reason}",
                );
            }
            other => panic!("expected HelloRejected, got {other:?}"),
        }

        let h_err = host_task.await.unwrap().unwrap_err();
        assert!(matches!(h_err, TransportError::HelloRejected(_)));
    }

    #[tokio::test]
    async fn host_rejects_protocol_version_1_hello() {
        let (viewer, host) = InProcTransport::pair(LoopbackOptions::default());

        // Viewer sends a v1 Hello directly (bypassing viewer_handshake which
        // always sends HELLO_PROTOCOL_VERSION).
        let viewer_task = tokio::spawn(async move {
            let hello = ControlMessage::Hello {
                protocol_version: 1,
                req_width: 1920,
                req_height: 1080,
                req_fps: 60,
                codec: Codec::H265,
            };
            viewer.send_control(hello).await.unwrap();
            // Drain one inbound control to absorb the HelloReject.
            let _ = transport_trait_recv_one(&viewer).await;
        });
        let host_task = tokio::spawn(async move {
            host_handshake(
                &host,
                0xCC,
                0,
                10_000_000,
                MonitorRect::new(0, 0, 1920, 1080),
                MonitorRect::new(0, 0, 1920, 1080),
                &[Codec::H265],
                Duration::from_millis(500),
            )
            .await
        });

        let _ = viewer_task.await;
        let h_err = host_task.await.unwrap().unwrap_err();
        match h_err {
            TransportError::Protocol(prdt_protocol::ProtocolError::UnsupportedVersion(v)) => {
                assert_eq!(v, 1);
            }
            other => panic!("expected UnsupportedVersion(1), got {other:?}"),
        }
    }

    async fn transport_trait_recv_one<T: Transport>(t: &T) -> Option<ReceivedMessage> {
        tokio::time::timeout(Duration::from_millis(200), t.recv())
            .await
            .ok()
            .and_then(|r| r.ok())
    }
}
