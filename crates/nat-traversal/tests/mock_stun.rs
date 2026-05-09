//! In-process STUN server for roundtrip tests. Implements just enough of
//! RFC 5389 to answer a Binding Request with an XOR-MAPPED-ADDRESS.

use bytecodec::{DecodeExt, EncodeExt};
use prdt_nat_traversal::{learn_public_addr, StunError};
use std::net::SocketAddr;
use std::time::Duration;
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::methods::BINDING;
use stun_codec::{
    define_attribute_enums, Message, MessageClass, MessageDecoder, MessageEncoder, TransactionId,
};
use tokio::net::UdpSocket;

define_attribute_enums!(
    Attribute,
    AttributeDecoder,
    AttributeEncoder,
    [XorMappedAddress]
);

async fn spawn_mock_stun_echoing_source_addr() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else {
                break;
            };
            let mut decoder = MessageDecoder::<Attribute>::new();
            let Ok(Ok(req)) = decoder.decode_from_bytes(&buf[..n]) else {
                continue;
            };
            if req.class() != MessageClass::Request || req.method() != BINDING {
                continue;
            }
            let mut resp = Message::<Attribute>::new(
                MessageClass::SuccessResponse,
                BINDING,
                req.transaction_id(),
            );
            resp.add_attribute(Attribute::from(XorMappedAddress::new(src)));
            let mut encoder = MessageEncoder::<Attribute>::new();
            let bytes = encoder.encode_into_bytes(resp).unwrap();
            let _ = socket.send_to(&bytes, src).await;
        }
    });
    addr
}

#[tokio::test]
async fn happy_path_learns_own_addr() {
    let server_addr = spawn_mock_stun_echoing_source_addr().await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let expected = client.local_addr().unwrap();

    let learned = learn_public_addr(&client, server_addr, Duration::from_secs(2))
        .await
        .unwrap();

    assert_eq!(learned.ip(), expected.ip());
    assert_eq!(learned.port(), expected.port());
}

#[tokio::test]
async fn timeout_when_server_silent() {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let silent_addr = silent.local_addr().unwrap();
    // Do NOT spawn a reader; packets sit in the kernel queue.
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let err = learn_public_addr(&client, silent_addr, Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(matches!(err, StunError::Timeout), "got: {err:?}");
}

#[tokio::test]
async fn ignores_wrong_transaction_id() {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else {
                break;
            };
            let mut decoder = MessageDecoder::<Attribute>::new();
            let Ok(Ok(_req)) = decoder.decode_from_bytes(&buf[..n]) else {
                continue;
            };
            let mut resp = Message::<Attribute>::new(
                MessageClass::SuccessResponse,
                BINDING,
                TransactionId::new([0xFF; 12]),
            );
            resp.add_attribute(Attribute::from(XorMappedAddress::new(src)));
            let mut encoder = MessageEncoder::<Attribute>::new();
            let bytes = encoder.encode_into_bytes(resp).unwrap();
            let _ = socket.send_to(&bytes, src).await;
        }
    });
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let err = learn_public_addr(&client, server_addr, Duration::from_millis(400))
        .await
        .unwrap_err();
    assert!(matches!(err, StunError::Timeout), "got: {err:?}");
}
