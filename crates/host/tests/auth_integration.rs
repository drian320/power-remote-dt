//! P6 auth integration tests: drive AuthValidator through realistic Hello
//! payloads and assert the AuthVerdict that comes back.
//!
//! These tests bypass the transport layer entirely — they construct an
//! AuthValidator and call `validate()` directly, which is intentional: the
//! 13 tests here exercise the state machine in isolation without needing a
//! running host or network.

use prdt_crypto::known_peers::{KnownPeer, KnownPeers};
use prdt_host::auth::{AuthValidator, AuthVerdict};
use prdt_host::auth_config::{AuthMode, HostAuthConfig};
use prdt_protocol::{AuthMethod, Codec, ControlMessage, HelloRejectCode, PermissionSet};
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn make_hello(auth_method: AuthMethod, payload: &[u8], protocol_version: u8) -> ControlMessage {
    ControlMessage::Hello {
        protocol_version,
        req_width: 1920,
        req_height: 1080,
        req_fps: 60,
        codec: Codec::H264,
        auth_method,
        auth_payload: payload.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// PIN tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pin_auth_success() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Pin,
        pin_hash: Some(HostAuthConfig::hash_pin("hunter2").unwrap()),
        ..Default::default()
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    let hello = make_hello(AuthMethod::Pin, b"hunter2", 3);
    let verdict = v.validate(&hello, "peerA").await;

    match verdict {
        AuthVerdict::Granted {
            permissions,
            remember,
        } => {
            assert_eq!(permissions, PermissionSet::all());
            assert!(!remember); // PIN mode never auto-remembers
        }
        other => panic!("expected Granted, got {other:?}"),
    }
}

#[tokio::test]
async fn pin_auth_wrong_then_correct() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Pin,
        pin_hash: Some(HostAuthConfig::hash_pin("hunter2").unwrap()),
        max_pin_attempts: 5,
        pin_lockout_seconds: 300,
        ..Default::default()
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    // Two wrong attempts.
    for _ in 0..2 {
        let hello = make_hello(AuthMethod::Pin, b"wrong", 3);
        let verdict = v.validate(&hello, "peerA").await;
        assert!(
            matches!(
                verdict,
                AuthVerdict::Rejected {
                    code: HelloRejectCode::AuthFailed,
                    ..
                }
            ),
            "expected AuthFailed"
        );
    }

    // Correct PIN succeeds and resets the counter.
    let hello = make_hello(AuthMethod::Pin, b"hunter2", 3);
    assert!(
        matches!(
            v.validate(&hello, "peerA").await,
            AuthVerdict::Granted { .. }
        ),
        "expected Granted after correct PIN"
    );
}

#[tokio::test]
async fn pin_auth_lockout_after_max_attempts() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Pin,
        pin_hash: Some(HostAuthConfig::hash_pin("hunter2").unwrap()),
        max_pin_attempts: 3,
        pin_lockout_seconds: 300,
        ..Default::default()
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    // Exhaust all attempts.
    for _ in 0..3 {
        let hello = make_hello(AuthMethod::Pin, b"wrong", 3);
        let _ = v.validate(&hello, "peerA").await;
    }

    // Even the correct PIN is rejected while locked out.
    let hello = make_hello(AuthMethod::Pin, b"hunter2", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(
            verdict,
            AuthVerdict::Rejected {
                code: HelloRejectCode::AuthLockout,
                ..
            }
        ),
        "expected AuthLockout, got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// Ephemeral tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ephemeral_auth_success() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Ephemeral,
        ..Default::default()
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let eph = v.rotate_ephemeral().await;

    let hello = make_hello(AuthMethod::Ephemeral, eph.as_bytes(), 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(verdict, AuthVerdict::Granted { .. }),
        "expected Granted, got {verdict:?}"
    );
}

#[tokio::test]
async fn ephemeral_auth_wrong_rejected() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Ephemeral,
        ..Default::default()
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let _real = v.rotate_ephemeral().await;

    let hello = make_hello(AuthMethod::Ephemeral, b"WRONG123", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(
            verdict,
            AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                ..
            }
        ),
        "expected AuthFailed, got {verdict:?}"
    );
}

#[tokio::test]
async fn ephemeral_expired_rejected() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Ephemeral,
        ephemeral_lifetime_seconds: 1, // expires after 1 s
        ..Default::default()
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let eph = v.rotate_ephemeral().await;

    // Wait for the ephemeral to expire (1.5 s > 1 s lifetime).
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let hello = make_hello(AuthMethod::Ephemeral, eph.as_bytes(), 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(
            verdict,
            AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                ..
            }
        ),
        "expected AuthFailed (expired), got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// Mode-mismatch tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pin_required_when_viewer_sends_tofu_to_pin_host() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Pin,
        pin_hash: Some(HostAuthConfig::hash_pin("hunter2").unwrap()),
        ..Default::default()
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    let hello = make_hello(AuthMethod::Tofu, b"", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(
            verdict,
            AuthVerdict::Rejected {
                code: HelloRejectCode::PinRequired,
                ..
            }
        ),
        "expected PinRequired, got {verdict:?}"
    );
}

#[tokio::test]
async fn ephemeral_required_when_viewer_sends_tofu_to_ephemeral_host() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Ephemeral,
        ..Default::default()
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let _ = v.rotate_ephemeral().await;

    let hello = make_hello(AuthMethod::Tofu, b"", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(
            verdict,
            AuthVerdict::Rejected {
                code: HelloRejectCode::EphemeralRequired,
                ..
            }
        ),
        "expected EphemeralRequired, got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// Protocol version gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn protocol_version_mismatch_rejected() {
    let cfg = HostAuthConfig::default();
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    let hello = make_hello(AuthMethod::Tofu, b"", 2); // pre-P6
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(
            verdict,
            AuthVerdict::Rejected {
                code: HelloRejectCode::ProtocolVersionMismatch,
                ..
            }
        ),
        "expected ProtocolVersionMismatch, got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// TOFU tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tofu_known_peer_grants_without_prompt() {
    let cfg = HostAuthConfig::default(); // mode = Tofu
    let custom_perms = PermissionSet {
        input: true,
        clipboard: false,
        file_transfer: false,
        audio: true,
    };
    let peer = KnownPeer {
        pubkey_b64: "peerA".into(),
        label: "work".into(),
        permissions: custom_perms,
        first_seen_at: std::time::UNIX_EPOCH,
        last_seen_at: std::time::SystemTime::now(),
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![peer] }));
    let v = AuthValidator::new(cfg, known);

    let hello = make_hello(AuthMethod::Tofu, b"", 3);
    let verdict = v.validate(&hello, "peerA").await;
    match verdict {
        AuthVerdict::Granted { permissions, .. } => assert_eq!(permissions, custom_perms),
        other => panic!("expected Granted, got {other:?}"),
    }
}

#[tokio::test]
async fn tofu_unknown_peer_needs_consent() {
    let cfg = HostAuthConfig::default();
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    let hello = make_hello(AuthMethod::Tofu, b"", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(verdict, AuthVerdict::NeedsConsent { .. }),
        "expected NeedsConsent, got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// PIN + known peer: still requires PIN
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pin_known_peer_still_requires_pin() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Pin,
        pin_hash: Some(HostAuthConfig::hash_pin("hunter2").unwrap()),
        ..Default::default()
    };
    let peer = KnownPeer {
        pubkey_b64: "peerA".into(),
        label: "work".into(),
        permissions: PermissionSet {
            input: true,
            clipboard: false,
            file_transfer: false,
            audio: true,
        },
        first_seen_at: std::time::UNIX_EPOCH,
        last_seen_at: std::time::SystemTime::now(),
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![peer] }));
    let v = AuthValidator::new(cfg, known);

    // Wrong PIN doesn't auto-pass even for a known peer.
    let hello = make_hello(AuthMethod::Pin, b"wrong", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(
            verdict,
            AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                ..
            }
        ),
        "expected AuthFailed for wrong PIN even for known peer, got {verdict:?}"
    );

    // Correct PIN grants with the *saved* peer permissions (not the default).
    let hello = make_hello(AuthMethod::Pin, b"hunter2", 3);
    let verdict = v.validate(&hello, "peerA").await;
    match verdict {
        AuthVerdict::Granted { permissions, .. } => {
            assert_eq!(
                permissions,
                PermissionSet {
                    input: true,
                    clipboard: false,
                    file_transfer: false,
                    audio: true
                },
                "expected saved peer permissions, not defaults"
            );
        }
        other => panic!("expected Granted, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Payload size cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_payload_oversize_rejected() {
    let cfg = HostAuthConfig {
        mode: AuthMode::Pin,
        pin_hash: Some(HostAuthConfig::hash_pin("hunter2").unwrap()),
        ..Default::default()
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    let huge = vec![b'A'; 65]; // > 64-byte cap (MAX_AUTH_PAYLOAD_BYTES)
    let hello = make_hello(AuthMethod::Pin, &huge, 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(
        matches!(
            verdict,
            AuthVerdict::Rejected {
                code: HelloRejectCode::Unspecified,
                ..
            }
        ),
        "expected Unspecified (oversize payload), got {verdict:?}"
    );
}
