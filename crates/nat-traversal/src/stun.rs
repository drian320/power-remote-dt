//! STUN Binding Request client (RFC 5389).
//!
//! `learn_public_addr` sends a single Binding Request on the provided UDP
//! socket, waits up to `timeout`, and returns the XOR-MAPPED-ADDRESS attribute
//! from the success response.

use crate::error::StunError;
use bytecodec::{DecodeExt, EncodeExt};
use std::net::SocketAddr;
use std::time::Duration;
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::methods::BINDING;
use stun_codec::{
    define_attribute_enums, Message, MessageClass, MessageDecoder, MessageEncoder, TransactionId,
};
use tokio::net::UdpSocket;
use tokio::time::timeout;
use tracing::{debug, instrument};

define_attribute_enums!(
    Attribute,
    AttributeDecoder,
    AttributeEncoder,
    [XorMappedAddress]
);

fn random_transaction_id() -> TransactionId {
    use rand_core::{OsRng, RngCore};
    let mut id = [0u8; 12];
    OsRng.fill_bytes(&mut id);
    TransactionId::new(id)
}

#[instrument(skip(socket), fields(%server_addr))]
pub async fn learn_public_addr(
    socket: &UdpSocket,
    server_addr: SocketAddr,
    timeout_duration: Duration,
) -> Result<SocketAddr, StunError> {
    let txn_id = random_transaction_id();
    let request = Message::<Attribute>::new(MessageClass::Request, BINDING, txn_id);

    let mut encoder = MessageEncoder::<Attribute>::new();
    let req_bytes = encoder
        .encode_into_bytes(request)
        .map_err(|e| StunError::Encode(e.to_string()))?;

    socket.send_to(&req_bytes, server_addr).await?;
    debug!(len = req_bytes.len(), "stun binding request sent");

    let mut buf = [0u8; 1500];
    let deadline = tokio::time::Instant::now() + timeout_duration;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(StunError::Timeout);
        }
        let (n, _from) = match timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(StunError::Io(e)),
            Err(_) => return Err(StunError::Timeout),
        };

        let mut decoder = MessageDecoder::<Attribute>::new();
        let msg = match decoder.decode_from_bytes(&buf[..n]) {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                debug!(error = ?e, "stun decode error; ignoring packet");
                continue;
            }
            Err(e) => {
                debug!(error = %e, "bytecodec error; ignoring packet");
                continue;
            }
        };
        if msg.transaction_id() != txn_id {
            debug!("transaction id mismatch; ignoring packet");
            continue;
        }
        if msg.class() != MessageClass::SuccessResponse {
            return Err(StunError::Decode(format!(
                "unexpected message class: {:?}",
                msg.class()
            )));
        }
        if let Some(xma) = msg.get_attribute::<XorMappedAddress>() {
            return Ok(xma.address());
        }
        return Err(StunError::NoMappedAddress);
    }
}
