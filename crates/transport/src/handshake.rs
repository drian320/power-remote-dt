use std::time::Duration;

use prdt_protocol::{
    control::{AuthMethod, ControlMessage, HelloRejectCode, PermissionSet},
    frame::Codec,
    MonitorRect,
};

use crate::error::TransportError;
use crate::transport_trait::{ReceivedMessage, Transport};

/// The decision returned by [`AuthHook::evaluate`] during Hello processing.
///
/// Transport calls the hook after validating protocol_version and codec
/// (which are wire-level concerns) and before constructing HelloAck.
#[derive(Debug)]
pub enum AuthDecision {
    /// Grant access with the specified permissions. Transport sends HelloAck.
    Grant(PermissionSet),
    /// Reject the connection. Transport sends HelloReject and returns an error.
    Reject {
        code: HelloRejectCode,
        reason: String,
    },
}

/// Hook that transport calls during [`host_handshake`] to delegate the
/// auth decision to the host layer.
///
/// Implement this on a struct that owns an `AuthValidator` (and optionally
/// a consent channel). For T4 / headless hosts the impl auto-rejects unknown
/// peers (`NeedsConsent` → `Reject(ConsentDenied, ...)`). T7 will plug in
/// the real GUI prompt.
///
/// # Why not a closure?
/// A `Box<dyn Fn>` closure cannot be `async` ergonomically in stable Rust
/// without boxing the future, making testing awkward. A trait is cleaner and
/// more future-proof (T7 will implement this on the GUI host state machine).
#[async_trait::async_trait]
pub trait AuthHook: Send + Sync {
    /// Evaluate the incoming Hello and return an `AuthDecision`.
    ///
    /// Called after protocol_version and codec checks pass. The hook receives
    /// the raw Hello message so it can inspect `auth_method` / `auth_payload`,
    /// and the peer's Noise public key in base-64 so it can look up
    /// per-peer permissions.
    ///
    /// The hook is responsible for:
    /// - AuthValidator dispatch (PIN / Ephemeral / TOFU)
    /// - Consent prompt handling (internal; transport never blocks on consent)
    /// - Mapping `AuthVerdict::NeedsConsent` to either `Grant` or `Reject`
    ///   (the host decides — headless auto-rejects; GUI pops a dialog)
    async fn evaluate(&self, hello: &ControlMessage, peer_pubkey_b64: &str) -> AuthDecision;
}

pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(3);
/// Number of Hello (re)sends the viewer attempts before giving up.
///
/// Sized so the total patience budget — `DEFAULT_HELLO_TIMEOUT *
/// DEFAULT_HELLO_RETRIES` = 3s × 22 = 66s — comfortably exceeds the host's
/// default consent-dialog window (`consent_timeout_seconds`, 60s). For an
/// unknown peer the host displays a consent prompt for up to 60s BEFORE it
/// ever reads a Hello; a viewer that gave up sooner would time out while the
/// host operator is still deciding, even though the connection is healthy.
pub const DEFAULT_HELLO_RETRIES: u8 = 22;

/// Wire-level protocol_version that this build of the codebase speaks.
/// Bumped to 4 in P5B-2b for the CursorUpdate variant + cursor_mode=Metadata
/// path. v3 viewers and v4 hosts are mutually incompatible (strict-match
/// rejection in handshake.rs); operators upgrade both sides simultaneously.
pub const HELLO_PROTOCOL_VERSION: u8 = 4;

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
    /// Permissions granted by the host for this session (P6 T5).
    /// Populated from the HelloAck wire field.
    pub granted_permissions: PermissionSet,
}

/// Send Hello, await HelloAck (or HelloReject). Retries on timeout, returns
/// session info on success. Returns `HelloRejectedWithCode` immediately if the
/// host replies with HelloReject — there's no point retrying a rejection.
///
/// # Deprecation
///
/// Production viewer code should use `run_viewer_auth_loop` (in `prdt-viewer`)
/// which handles PIN/Ephemeral re-prompts and maps all `HelloRejectCode` variants
/// to user-visible errors. This function is retained only for transport-layer
/// integration tests (signaling smoke tests) that need a minimal one-shot handshake.
#[deprecated(note = "Use `run_viewer_auth_loop` for production viewer code; \
            `viewer_handshake` is kept for transport-layer integration tests only.")]
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
            auth_method: AuthMethod::Tofu,
            auth_payload: vec![],
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
                        granted_permissions,
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
                            granted_permissions,
                        });
                    }
                    ReceivedMessage::Control(ControlMessage::HelloReject { reason, code }) => {
                        return Err(TransportError::HelloRejectedWithCode { code, reason });
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

/// Result of a successful [`host_handshake`]: the viewer's request parameters
/// plus the permissions granted by the [`AuthHook`].
#[derive(Debug, Clone)]
pub struct HostHandshakeResult {
    pub req: HelloRequest,
    /// Permissions granted to this session by the [`AuthHook`].
    /// Immutable for the session lifetime — enforcement is at the call site.
    pub granted_permissions: PermissionSet,
}

/// Host-side: await Hello, respond with HelloAck or HelloReject.
///
/// `host_supported_codecs` is the full set of codecs this host can drive;
/// used for the codec-negotiation check (`Hello.codec` must be in this set).
///
/// `ack_codecs_for` is called with the viewer's `Hello.codec` to produce the
/// `host_supported_codecs` list placed in the HelloAck. This allows callers to
/// filter the advertised set based on the inbound codec (R15 mitigation: hosts
/// must not advertise `Codec::H265Main10` to pre-PR1 clients).  Pass
/// `|_| host_supported_codecs.to_vec()` to reproduce the previous behaviour.
///
/// Auth is delegated to `hook` — after codec/version checks pass, the hook
/// receives the raw Hello and the peer's Noise pubkey and returns either
/// `AuthDecision::Grant(perms)` or `AuthDecision::Reject { .. }`.
///
/// # Multi-round auth contract
///
/// A single call may span several Hello → HelloReject rounds over the SAME
/// crypto session. The viewer auth flow is multi-round: the first Hello may
/// carry no credential, the host answers `HelloReject(PinRequired)` (or
/// `EphemeralRequired`), and the viewer resends a Hello carrying the PIN/token.
/// When the hook returns `AuthDecision::Reject` with one of these
/// *continuation* codes — `PinRequired`, `EphemeralRequired`, or `AuthFailed`
/// — this function sends the HelloReject and loops back to await the viewer's
/// next Hello **without** returning; the caller's session stays alive. Only
/// *fatal* codes (`AuthLockout`, `ConsentDenied`, and anything else) end the
/// call with `Err(TransportError::HelloRejected)`. The whole exchange, across
/// all rounds, is bounded by `wait_timeout` — the timeout is not reset per
/// round.
#[allow(clippy::too_many_arguments)]
pub async fn host_handshake<T: Transport, A: AuthHook, F>(
    transport: &T,
    hook: &A,
    peer_pubkey_b64: &str,
    session_id: u64,
    host_monotonic_base_us: u64,
    negotiated_bitrate_bps: u32,
    host_monitor_rect: MonitorRect,
    host_virtual_desktop_rect: MonitorRect,
    host_supported_codecs: &[Codec],
    ack_codecs_for: F,
    wait_timeout: Duration,
) -> Result<HostHandshakeResult, TransportError>
where
    F: Fn(Codec) -> Vec<Codec>,
{
    let supported = host_supported_codecs.to_vec();
    let fut = async {
        loop {
            let hello = match transport.recv().await? {
                ReceivedMessage::Control(msg @ ControlMessage::Hello { .. }) => msg,
                _ => continue,
            };
            let (protocol_version, req_width, req_height, req_fps, codec) = match &hello {
                ControlMessage::Hello {
                    protocol_version,
                    req_width,
                    req_height,
                    req_fps,
                    codec,
                    ..
                } => (*protocol_version, *req_width, *req_height, *req_fps, *codec),
                _ => unreachable!(),
            };

            if protocol_version != HELLO_PROTOCOL_VERSION {
                // Tell the viewer why and surface UnsupportedVersion.
                let reason = format!(
                    "host speaks protocol_version {}, viewer sent {}",
                    HELLO_PROTOCOL_VERSION, protocol_version
                );
                let _ = transport
                    .send_control(ControlMessage::HelloReject {
                        reason,
                        code: HelloRejectCode::ProtocolVersionMismatch,
                    })
                    .await;
                return Err(TransportError::Protocol(
                    prdt_protocol::ProtocolError::UnsupportedVersion(protocol_version),
                ));
            }
            if !supported.contains(&codec) {
                let reason = format!("host does not support {}", codec.name());
                transport
                    .send_control(ControlMessage::HelloReject {
                        reason: reason.clone(),
                        code: HelloRejectCode::UnsupportedCodec,
                    })
                    .await?;
                return Err(TransportError::HelloRejected(reason));
            }

            // Delegate auth decision to the hook (after wire-level checks pass).
            let granted_permissions = match hook.evaluate(&hello, peer_pubkey_b64).await {
                AuthDecision::Grant(perms) => perms,
                // Continuation rejects: the viewer is expected to resend a Hello
                // carrying the missing/corrected credential over the SAME crypto
                // session. Send the HelloReject and loop back to await that next
                // Hello without tearing the session down. A send failure here is
                // fatal (the channel is gone), so propagate it as Err instead of
                // swallowing it with `let _ =`.
                AuthDecision::Reject {
                    code:
                        code @ (HelloRejectCode::PinRequired
                        | HelloRejectCode::EphemeralRequired
                        | HelloRejectCode::AuthFailed),
                    reason,
                } => {
                    transport
                        .send_control(ControlMessage::HelloReject { reason, code })
                        .await?;
                    continue;
                }
                // Fatal rejects (AuthLockout, ConsentDenied, and anything else):
                // no viewer action recovers this session, so send the reject
                // best-effort and tear the session down with an error.
                AuthDecision::Reject { code, reason } => {
                    let _ = transport
                        .send_control(ControlMessage::HelloReject {
                            reason: reason.clone(),
                            code,
                        })
                        .await;
                    return Err(TransportError::HelloRejected(reason));
                }
            };

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
                host_supported_codecs: ack_codecs_for(codec),
                granted_permissions,
            };
            transport.send_control(ack).await?;
            return Ok(HostHandshakeResult {
                req: HelloRequest {
                    req_width,
                    req_height,
                    req_fps,
                    codec,
                },
                granted_permissions,
            });
        }
    };
    match tokio::time::timeout(wait_timeout, fut).await {
        Ok(r) => r,
        Err(_) => Err(TransportError::HandshakeTimeout),
    }
}

#[cfg(test)]
#[allow(deprecated)] // viewer_handshake is tested here as a transport primitive
mod tests {
    use super::*;
    use crate::loopback::{InProcTransport, LoopbackOptions};
    use prdt_protocol::frame::Codec;

    /// Minimal AuthHook for tests: always grants PermissionSet::all().
    struct GrantAllHook;

    #[async_trait::async_trait]
    impl AuthHook for GrantAllHook {
        async fn evaluate(&self, _hello: &ControlMessage, _peer: &str) -> AuthDecision {
            AuthDecision::Grant(PermissionSet::all())
        }
    }

    /// Helper: run host_handshake with GrantAllHook and no peer pubkey.
    #[allow(clippy::too_many_arguments)]
    async fn host_hs<T: Transport>(
        transport: &T,
        session_id: u64,
        host_monotonic_base_us: u64,
        negotiated_bitrate_bps: u32,
        host_monitor_rect: MonitorRect,
        host_virtual_desktop_rect: MonitorRect,
        host_supported_codecs: &[Codec],
        wait_timeout: Duration,
    ) -> Result<HostHandshakeResult, TransportError> {
        let supported = host_supported_codecs.to_vec();
        host_handshake(
            transport,
            &GrantAllHook,
            "test-peer",
            session_id,
            host_monotonic_base_us,
            negotiated_bitrate_bps,
            host_monitor_rect,
            host_virtual_desktop_rect,
            host_supported_codecs,
            |_| supported.clone(),
            wait_timeout,
        )
        .await
    }

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
            host_hs(
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
        let result = h.unwrap().unwrap();
        assert_eq!(ack.session_id, 0x1234);
        assert_eq!(ack.neg_width, 1920);
        assert_eq!(ack.host_monitor_rect.width(), 1920);
        assert_eq!(ack.host_virtual_desktop_rect.width(), 3840);
        assert_eq!(ack.negotiated_codec, Codec::H265);
        assert_eq!(ack.host_supported_codecs, vec![Codec::H265]);
        assert_eq!(result.req.req_fps, 60);
        assert_eq!(result.req.codec, Codec::H265);
        assert_eq!(result.granted_permissions, PermissionSet::all());
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
            host_hs(
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
        let result = h.unwrap().unwrap();
        assert_eq!(ack.negotiated_codec, Codec::H264);
        assert_eq!(result.req.codec, Codec::H264);
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
            host_hs(
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
        let v_outcome = tokio::time::timeout(Duration::from_millis(100), viewer_task)
            .await
            .expect("viewer must observe rejection within 100ms");
        let v_err = v_outcome.unwrap().unwrap_err();
        match v_err {
            TransportError::HelloRejectedWithCode { reason, .. } => {
                assert!(
                    reason.contains("av1") || reason.contains("AV1"),
                    "reason should mention the codec: {reason}",
                );
            }
            other => panic!("expected HelloRejectedWithCode, got {other:?}"),
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
                auth_method: AuthMethod::Tofu,
                auth_payload: vec![],
            };
            viewer.send_control(hello).await.unwrap();
            // Drain one inbound control to absorb the HelloReject.
            let _ = transport_trait_recv_one(&viewer).await;
        });
        let host_task = tokio::spawn(async move {
            host_hs(
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

    #[tokio::test]
    async fn auth_hook_reject_is_surfaced_as_hello_rejected() {
        /// Hook that always rejects with a *fatal* code (ConsentDenied). Note:
        /// AuthFailed / PinRequired / EphemeralRequired are now *continuation*
        /// codes that keep the session alive for a retry Hello, so a fatal code
        /// is required to exercise the "reject surfaces as HelloRejected" path.
        struct RejectAllHook;
        #[async_trait::async_trait]
        impl AuthHook for RejectAllHook {
            async fn evaluate(&self, _hello: &ControlMessage, _peer: &str) -> AuthDecision {
                AuthDecision::Reject {
                    code: HelloRejectCode::ConsentDenied,
                    reason: "consent denied in test".into(),
                }
            }
        }

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
                1,
            )
            .await
        });
        let host_task = tokio::spawn(async move {
            host_handshake(
                &host,
                &RejectAllHook,
                "peer-x",
                0xDD,
                0,
                10_000_000,
                MonitorRect::new(0, 0, 1920, 1080),
                MonitorRect::new(0, 0, 1920, 1080),
                &[Codec::H265],
                |_| vec![Codec::H265],
                Duration::from_millis(500),
            )
            .await
        });

        let (v, h) = tokio::join!(viewer_task, host_task);
        assert!(matches!(
            v.unwrap().unwrap_err(),
            TransportError::HelloRejectedWithCode { .. }
        ));
        assert!(matches!(
            h.unwrap().unwrap_err(),
            TransportError::HelloRejected(_)
        ));
    }

    async fn transport_trait_recv_one<T: Transport>(t: &T) -> Option<ReceivedMessage> {
        tokio::time::timeout(Duration::from_millis(200), t.recv())
            .await
            .ok()
            .and_then(|r| r.ok())
    }

    /// Receive the next `ControlMessage`, skipping any non-control frames.
    /// Returns `None` if nothing arrives within the recv timeout.
    async fn recv_control<T: Transport>(t: &T) -> Option<ControlMessage> {
        loop {
            match transport_trait_recv_one(t).await {
                Some(ReceivedMessage::Control(m)) => return Some(m),
                Some(_) => continue,
                None => return None,
            }
        }
    }

    /// Regression test for the multi-round PIN contract: a viewer sends a Hello
    /// with no PIN, the host replies HelloReject(PinRequired), and the viewer
    /// resends a Hello carrying the PIN over the SAME session. A SINGLE
    /// `host_handshake` call must survive the continuation reject and return Ok
    /// — proving the session is not torn down between rounds.
    #[tokio::test]
    async fn pin_required_then_with_pin_succeeds() {
        /// Grants only Hellos whose `auth_payload` equals the expected PIN;
        /// otherwise rejects with the continuation code `PinRequired`.
        struct PinGateHook {
            expected_pin: Vec<u8>,
        }
        #[async_trait::async_trait]
        impl AuthHook for PinGateHook {
            async fn evaluate(&self, hello: &ControlMessage, _peer: &str) -> AuthDecision {
                let payload = match hello {
                    ControlMessage::Hello { auth_payload, .. } => auth_payload.as_slice(),
                    _ => &[],
                };
                if payload == self.expected_pin.as_slice() {
                    AuthDecision::Grant(PermissionSet::all())
                } else {
                    AuthDecision::Reject {
                        code: HelloRejectCode::PinRequired,
                        reason: "host is in PIN mode; viewer must set auth_method=Pin".into(),
                    }
                }
            }
        }

        let (viewer, host) = InProcTransport::pair(LoopbackOptions::default());
        let hook = PinGateHook {
            expected_pin: b"1234".to_vec(),
        };

        let host_task = tokio::spawn(async move {
            host_handshake(
                &host,
                &hook,
                "peer-pin",
                0x5151,
                0,
                10_000_000,
                MonitorRect::new(0, 0, 1920, 1080),
                MonitorRect::new(0, 0, 1920, 1080),
                &[Codec::H265],
                |_| vec![Codec::H265],
                Duration::from_secs(2),
            )
            .await
        });

        let viewer_task = tokio::spawn(async move {
            // Round 1: Hello with no PIN → expect HelloReject(PinRequired).
            let no_pin = ControlMessage::Hello {
                protocol_version: HELLO_PROTOCOL_VERSION,
                req_width: 1920,
                req_height: 1080,
                req_fps: 60,
                codec: Codec::H265,
                auth_method: AuthMethod::Tofu,
                auth_payload: vec![],
            };
            viewer.send_control(no_pin).await.unwrap();
            let reject = recv_control(&viewer).await;
            assert!(
                matches!(
                    reject,
                    Some(ControlMessage::HelloReject {
                        code: HelloRejectCode::PinRequired,
                        ..
                    })
                ),
                "expected HelloReject(PinRequired), got {reject:?}"
            );

            // Round 2: resend Hello carrying the PIN over the SAME session.
            let with_pin = ControlMessage::Hello {
                protocol_version: HELLO_PROTOCOL_VERSION,
                req_width: 1920,
                req_height: 1080,
                req_fps: 60,
                codec: Codec::H265,
                auth_method: AuthMethod::Pin,
                auth_payload: b"1234".to_vec(),
            };
            viewer.send_control(with_pin).await.unwrap();
            let ack = recv_control(&viewer).await;
            assert!(
                matches!(ack, Some(ControlMessage::HelloAck { .. })),
                "expected HelloAck after PIN, got {ack:?}"
            );
        });

        let (h, v) = tokio::join!(host_task, viewer_task);
        v.unwrap();
        // The single host_handshake call returned Ok despite the earlier reject.
        let result = h.unwrap().unwrap();
        assert_eq!(result.granted_permissions, PermissionSet::all());
        assert_eq!(result.req.codec, Codec::H265);
    }

    /// A fatal reject (`AuthLockout`) must end the single `host_handshake` call
    /// with Err, and the viewer must receive a HelloReject carrying that code.
    #[tokio::test]
    async fn lockout_reject_is_fatal() {
        struct LockoutHook;
        #[async_trait::async_trait]
        impl AuthHook for LockoutHook {
            async fn evaluate(&self, _hello: &ControlMessage, _peer: &str) -> AuthDecision {
                AuthDecision::Reject {
                    code: HelloRejectCode::AuthLockout,
                    reason: "too many attempts; locked out".into(),
                }
            }
        }

        let (viewer, host) = InProcTransport::pair(LoopbackOptions::default());

        let host_task = tokio::spawn(async move {
            host_handshake(
                &host,
                &LockoutHook,
                "peer-lock",
                0x6262,
                0,
                10_000_000,
                MonitorRect::new(0, 0, 1920, 1080),
                MonitorRect::new(0, 0, 1920, 1080),
                &[Codec::H265],
                |_| vec![Codec::H265],
                Duration::from_secs(2),
            )
            .await
        });

        let viewer_task = tokio::spawn(async move {
            let hello = ControlMessage::Hello {
                protocol_version: HELLO_PROTOCOL_VERSION,
                req_width: 1920,
                req_height: 1080,
                req_fps: 60,
                codec: Codec::H265,
                auth_method: AuthMethod::Pin,
                auth_payload: b"9999".to_vec(),
            };
            viewer.send_control(hello).await.unwrap();
            recv_control(&viewer).await
        });

        let (h, v) = tokio::join!(host_task, viewer_task);
        let h_err = h.unwrap().unwrap_err();
        assert!(
            matches!(h_err, TransportError::HelloRejected(_)),
            "expected HelloRejected, got {h_err:?}"
        );
        let reject = v.unwrap();
        assert!(
            matches!(
                reject,
                Some(ControlMessage::HelloReject {
                    code: HelloRejectCode::AuthLockout,
                    ..
                })
            ),
            "expected HelloReject(AuthLockout), got {reject:?}"
        );
    }
}
