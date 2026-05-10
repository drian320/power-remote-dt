use prdt_signaling_proto::*;

/// The JSON literals here MUST match the wire format promised in the spec.
/// If any assertion changes, the wire format has broken — review downstream consumers.

#[test]
fn parse_register() {
    let json = r#"{"t":"register","host_id":"alice-desktop","pubkey_b64":"ZXhhbXBsZQ=="}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Register {
            host_id,
            pubkey_b64,
        } => {
            assert_eq!(host_id, "alice-desktop");
            assert_eq!(pubkey_b64, "ZXhhbXBsZQ==");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_connect() {
    let json = r#"{"t":"connect","host_id":"alice-desktop"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    assert!(matches!(msg, ClientMessage::Connect { host_id } if host_id == "alice-desktop"));
}

#[test]
fn parse_candidate() {
    let json = r#"{"t":"candidate","session_id":"s1","candidate":{"typ":"host","ip":"127.0.0.1","port":55000,"priority":100}}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Candidate {
            session_id,
            candidate,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(candidate.typ, CandidateType::Host);
            assert_eq!(candidate.ip, "127.0.0.1");
            assert_eq!(candidate.port, 55000);
            assert_eq!(candidate.priority, 100);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_done_connected() {
    let json = r#"{"t":"done","session_id":"s1","outcome":{"t":"connected"}}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    assert!(matches!(
        msg,
        ClientMessage::Done { ref session_id, outcome: DoneOutcome::Connected } if session_id == "s1"
    ));
}

#[test]
fn parse_done_failed() {
    let json = r#"{"t":"done","session_id":"s1","outcome":{"t":"failed","reason":"x"}}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Done {
            outcome: DoneOutcome::Failed { reason },
            ..
        } => {
            assert_eq!(reason, "x");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_session_start_host() {
    let json = r#"{"t":"session_start","session_id":"s1","role":"host","peer_pubkey_b64":null}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::SessionStart {
            session_id,
            role,
            peer_pubkey_b64,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(role, Role::Host);
            assert_eq!(peer_pubkey_b64, None);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_session_start_viewer() {
    let json =
        r#"{"t":"session_start","session_id":"s1","role":"viewer","peer_pubkey_b64":"Pa=="}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::SessionStart {
            role,
            peer_pubkey_b64,
            ..
        } => {
            assert_eq!(role, Role::Viewer);
            assert_eq!(peer_pubkey_b64.as_deref(), Some("Pa=="));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_server_error() {
    let json = r#"{"t":"error","code":"host_not_found","message":"no such host"}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Error { code, message } => {
            assert_eq!(code, ErrorCode::HostNotFound);
            assert_eq!(message, "no such host");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn unknown_variant_rejected() {
    let json = r#"{"t":"invented","foo":1}"#;
    let err = serde_json::from_str::<ClientMessage>(json).unwrap_err();
    assert!(err.to_string().contains("invented") || err.is_data());
}

#[test]
fn parse_host_id_pubkey_mismatch() {
    let json = r#"{"t":"error","code":"host_id_pubkey_mismatch","message":"x"}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Error { code, .. } => {
            assert_eq!(code, ErrorCode::HostIdPubkeyMismatch);
        }
        other => panic!("unexpected: {other:?}"),
    }
}
