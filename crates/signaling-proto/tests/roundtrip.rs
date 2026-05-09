use prdt_signaling_proto::{Candidate, CandidateType};
use proptest::prelude::*;

fn arb_candidate_type() -> impl Strategy<Value = CandidateType> {
    prop_oneof![
        Just(CandidateType::Host),
        Just(CandidateType::Srflx),
        Just(CandidateType::Relay),
    ]
}

fn arb_candidate() -> impl Strategy<Value = Candidate> {
    (
        arb_candidate_type(),
        "[0-9]{1,3}\\.[0-9]{1,3}\\.[0-9]{1,3}\\.[0-9]{1,3}".prop_map(String::from),
        any::<u16>(),
        any::<u32>(),
    )
        .prop_map(|(typ, ip, port, priority)| Candidate {
            typ,
            ip,
            port,
            priority,
        })
}

proptest! {
    #[test]
    fn candidate_json_roundtrip(c in arb_candidate()) {
        let s = serde_json::to_string(&c).unwrap();
        let back: Candidate = serde_json::from_str(&s).unwrap();
        prop_assert_eq!(c.typ, back.typ);
        prop_assert_eq!(c.ip, back.ip);
        prop_assert_eq!(c.port, back.port);
        prop_assert_eq!(c.priority, back.priority);
    }
}

#[test]
fn candidate_type_snake_case() {
    assert_eq!(
        serde_json::to_string(&CandidateType::Host).unwrap(),
        "\"host\""
    );
    assert_eq!(
        serde_json::to_string(&CandidateType::Srflx).unwrap(),
        "\"srflx\""
    );
    assert_eq!(
        serde_json::to_string(&CandidateType::Relay).unwrap(),
        "\"relay\""
    );
}

fn arb_client_message() -> impl Strategy<Value = prdt_signaling_proto::ClientMessage> {
    use prdt_signaling_proto::*;
    prop_oneof![
        ("[a-z]{1,8}", "[A-Za-z0-9+/=]{4,40}").prop_map(|(host_id, pubkey_b64)| {
            ClientMessage::Register {
                host_id,
                pubkey_b64,
            }
        }),
        "[a-z]{1,8}".prop_map(|host_id| ClientMessage::Connect { host_id }),
        ("[a-z0-9]{4,12}", arb_candidate()).prop_map(|(session_id, candidate)| {
            ClientMessage::Candidate {
                session_id,
                candidate,
            }
        }),
        "[a-z0-9]{4,12}".prop_map(|session_id| ClientMessage::Done {
            session_id,
            outcome: DoneOutcome::Connected
        }),
    ]
}

proptest! {
    #[test]
    fn client_message_roundtrip(m in arb_client_message()) {
        let s = serde_json::to_string(&m).unwrap();
        let back: prdt_signaling_proto::ClientMessage = serde_json::from_str(&s).unwrap();
        prop_assert_eq!(m, back);
    }
}
