use std::time::Duration;

use prdt_protocol::{control::ControlMessage, frame::Codec};

use crate::error::TransportError;
use crate::transport_trait::{ReceivedMessage, Transport};

pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(3);
pub const DEFAULT_HELLO_RETRIES: u8 = 3;

#[derive(Debug, Clone)]
pub struct HelloRequest {
    pub req_width: u32,
    pub req_height: u32,
    pub req_fps: u32,
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
}

/// Send Hello, await HelloAck. Retries on timeout, returns session info on success.
pub async fn viewer_handshake<T: Transport>(
    transport: &T,
    req: &HelloRequest,
    per_attempt_timeout: Duration,
    retries: u8,
) -> Result<SessionAck, TransportError> {
    for _ in 0..retries {
        let hello = ControlMessage::Hello {
            protocol_version: 1,
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
                    }) => {
                        return Ok::<SessionAck, TransportError>(SessionAck {
                            session_id,
                            host_monotonic_base_us,
                            neg_width,
                            neg_height,
                            neg_fps,
                            neg_bitrate_bps,
                        });
                    }
                    // ignore other messages during handshake
                    _ => continue,
                }
            }
        };
        match tokio::time::timeout(per_attempt_timeout, ack_fut).await {
            Ok(r) => return r,
            Err(_) => continue, // retry
        }
    }
    Err(TransportError::HandshakeTimeout)
}

/// Host-side: await Hello, respond with HelloAck.
pub async fn host_handshake<T: Transport>(
    transport: &T,
    session_id: u64,
    host_monotonic_base_us: u64,
    negotiated_bitrate_bps: u32,
    wait_timeout: Duration,
) -> Result<HelloRequest, TransportError> {
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
                    if protocol_version != 1 {
                        return Err(TransportError::Protocol(
                            prdt_protocol::ProtocolError::UnsupportedVersion(protocol_version),
                        ));
                    }
                    let ack = ControlMessage::HelloAck {
                        session_id,
                        host_monotonic_base_us,
                        neg_width: req_width,
                        neg_height: req_height,
                        neg_fps: req_fps,
                        neg_bitrate_bps: negotiated_bitrate_bps,
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
            host_handshake(&host, 0x1234, 42, 10_000_000, Duration::from_millis(500)).await
        });

        let (v, h) = tokio::join!(viewer_task, host_task);
        let ack = v.unwrap().unwrap();
        let req = h.unwrap().unwrap();
        assert_eq!(ack.session_id, 0x1234);
        assert_eq!(ack.neg_width, 1920);
        assert_eq!(req.req_fps, 60);
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
}
