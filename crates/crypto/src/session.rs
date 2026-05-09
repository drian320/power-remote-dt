//! Noise handshake and transport session wrapper around snow.

use snow::{Builder, HandshakeState, TransportState};

use crate::keypair::{KeyPair, PubKey};

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("snow: {0}")]
    Snow(String),
    #[error("handshake not complete")]
    HandshakeIncomplete,
    #[error("handshake already complete")]
    HandshakeAlreadyComplete,
    #[error("wrong state: {0}")]
    WrongState(&'static str),
}

impl From<snow::Error> for CryptoError {
    fn from(e: snow::Error) -> Self {
        CryptoError::Snow(format!("{e:?}"))
    }
}

/// Server-side (responder) handshake.
pub struct ServerHandshake {
    state: HandshakeState,
}

impl ServerHandshake {
    pub fn new(server_keypair: &KeyPair) -> Result<Self, CryptoError> {
        let params = crate::NOISE_PATTERN
            .parse::<snow::params::NoiseParams>()
            .map_err(|e| CryptoError::Snow(format!("parse params: {e:?}")))?;
        let state = Builder::new(params)
            .local_private_key(&server_keypair.private.0)
            .build_responder()?;
        Ok(Self { state })
    }

    /// Consume the initiator's first message (e, es, s, ss) and produce the
    /// responder's reply (e, ee, se). After this call the handshake is
    /// complete; a `Session` plus the initiator's static public key are
    /// returned so the host can identify the peer cryptographically.
    pub fn respond(mut self, client_msg: &[u8]) -> Result<(Vec<u8>, Session, PubKey), CryptoError> {
        let mut read_buf = vec![0u8; 1024];
        let _ = self.state.read_message(client_msg, &mut read_buf)?;

        let mut response = vec![0u8; 1024];
        let written = self.state.write_message(&[], &mut response)?;
        response.truncate(written);

        // IK pattern: by the time we've read the initiator message, snow has
        // decrypted and stored the initiator's static pubkey. Capture it
        // BEFORE into_transport_mode() consumes the handshake state.
        let remote_static = self
            .state
            .get_remote_static()
            .ok_or(CryptoError::WrongState("missing remote static"))?;
        if remote_static.len() != 32 {
            return Err(CryptoError::WrongState("missing remote static"));
        }
        let mut pk_bytes = [0u8; 32];
        pk_bytes.copy_from_slice(remote_static);
        let peer_pubkey = PubKey(pk_bytes);

        let transport = self.state.into_transport_mode()?;
        Ok((response, Session { state: transport }, peer_pubkey))
    }
}

/// Client-side (initiator) handshake.
pub struct ClientHandshake {
    state: HandshakeState,
}

impl ClientHandshake {
    pub fn new(server_pubkey: &PubKey, client_keypair: &KeyPair) -> Result<Self, CryptoError> {
        let params = crate::NOISE_PATTERN
            .parse::<snow::params::NoiseParams>()
            .map_err(|e| CryptoError::Snow(format!("parse params: {e:?}")))?;
        let state = Builder::new(params)
            .local_private_key(&client_keypair.private.0)
            .remote_public_key(&server_pubkey.0)
            .build_initiator()?;
        Ok(Self { state })
    }

    pub fn initiate(&mut self) -> Result<Vec<u8>, CryptoError> {
        let mut buf = vec![0u8; 1024];
        let written = self.state.write_message(&[], &mut buf)?;
        buf.truncate(written);
        Ok(buf)
    }

    pub fn finalize(mut self, server_msg: &[u8]) -> Result<Session, CryptoError> {
        let mut buf = vec![0u8; 1024];
        self.state.read_message(server_msg, &mut buf)?;
        let transport = self.state.into_transport_mode()?;
        Ok(Session { state: transport })
    }
}

/// Active symmetric session. Each direction uses its own key internally,
/// with monotonic per-direction nonce managed by snow.
pub struct Session {
    state: TransportState,
}

impl Session {
    /// Encrypt plaintext. Returns ciphertext = plaintext_len + 16 (AEAD tag).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = vec![0u8; plaintext.len() + 16];
        let written = self.state.write_message(plaintext, &mut out)?;
        out.truncate(written);
        Ok(out)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if ciphertext.len() < 16 {
            return Err(CryptoError::Snow(
                "ciphertext too short for AEAD tag".into(),
            ));
        }
        let mut out = vec![0u8; ciphertext.len()];
        let written = self.state.read_message(ciphertext, &mut out)?;
        out.truncate(written);
        Ok(out)
    }

    /// Override the nonce used for the NEXT `decrypt()` call. Needed when
    /// the underlying transport reorders or drops packets so the receiver
    /// must tell snow which nonce the ciphertext was produced with.
    ///
    /// snow 0.9.6's `TransportState::set_receiving_nonce` returns unit.
    pub fn set_receiving_nonce(&mut self, nonce: u64) {
        self.state.set_receiving_nonce(nonce);
    }

    /// Rotate the outgoing cipher key. Should be matched by the peer calling
    /// `rekey_incoming` on their side; typically done as a periodic background
    /// operation (e.g. every 10 minutes or 100k messages) to limit the window
    /// of key compromise impact.
    pub fn rekey_outgoing(&mut self) {
        self.state.rekey_outgoing();
    }

    /// Rotate the incoming cipher key.
    pub fn rekey_incoming(&mut self) {
        self.state.rekey_incoming();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keypair::KeyPair;

    #[test]
    fn full_handshake_and_roundtrip() {
        let server_kp = KeyPair::generate();
        let client_kp = KeyPair::generate();
        let server = ServerHandshake::new(&server_kp).unwrap();
        let mut client = ClientHandshake::new(&server_kp.public, &client_kp).unwrap();

        let msg1 = client.initiate().unwrap();
        let (msg2, mut server_session, _peer_pk) = server.respond(&msg1).unwrap();
        let mut client_session = client.finalize(&msg2).unwrap();

        // Client → Server
        let plaintext = b"hello world";
        let ct = client_session.encrypt(plaintext).unwrap();
        assert_ne!(
            &ct[..],
            plaintext,
            "ciphertext should differ from plaintext"
        );
        let pt = server_session.decrypt(&ct).unwrap();
        assert_eq!(&pt, plaintext);

        // Server → Client
        let plaintext2 = b"reply from server";
        let ct2 = server_session.encrypt(plaintext2).unwrap();
        let pt2 = client_session.decrypt(&ct2).unwrap();
        assert_eq!(&pt2, plaintext2);
    }

    #[test]
    fn wrong_pubkey_fails() {
        let real = KeyPair::generate();
        let fake = KeyPair::generate();
        let client_kp = KeyPair::generate();
        let server = ServerHandshake::new(&real).unwrap();
        let mut client = ClientHandshake::new(&fake.public, &client_kp).unwrap();

        let msg1 = client.initiate().unwrap();
        let res = server.respond(&msg1);
        assert!(res.is_err(), "handshake with wrong pubkey should fail");
    }

    #[test]
    fn server_recovers_initiator_static_pubkey() {
        let server_kp = KeyPair::generate();
        let client_kp = KeyPair::generate();
        let server = ServerHandshake::new(&server_kp).unwrap();
        let mut client = ClientHandshake::new(&server_kp.public, &client_kp).unwrap();

        let msg1 = client.initiate().unwrap();
        let (_msg2, _server_session, peer_pk) = server.respond(&msg1).unwrap();
        assert_eq!(
            peer_pk, client_kp.public,
            "server-side recovered pubkey must equal client's static pubkey"
        );
    }

    #[test]
    fn out_of_order_decrypt_with_set_receiving_nonce() {
        let server_kp = KeyPair::generate();
        let client_kp = KeyPair::generate();
        let server = ServerHandshake::new(&server_kp).unwrap();
        let mut client = ClientHandshake::new(&server_kp.public, &client_kp).unwrap();
        let (msg2, mut server_session, _peer_pk) =
            server.respond(&client.initiate().unwrap()).unwrap();
        let mut client_session = client.finalize(&msg2).unwrap();

        // Client encrypts 5 messages — snow assigns nonces 0..4 internally.
        let pt0 = b"msg zero";
        let pt1 = b"msg one";
        let pt2 = b"msg two";
        let pt3 = b"msg three";
        let pt4 = b"msg four";
        let ct0 = client_session.encrypt(pt0).unwrap();
        let ct1 = client_session.encrypt(pt1).unwrap();
        let ct2 = client_session.encrypt(pt2).unwrap();
        let ct3 = client_session.encrypt(pt3).unwrap();
        let ct4 = client_session.encrypt(pt4).unwrap();

        // Server receives them OUT OF ORDER: 3, 1, 0, 4, 2
        server_session.set_receiving_nonce(3);
        assert_eq!(server_session.decrypt(&ct3).unwrap(), pt3);
        server_session.set_receiving_nonce(1);
        assert_eq!(server_session.decrypt(&ct1).unwrap(), pt1);
        server_session.set_receiving_nonce(0);
        assert_eq!(server_session.decrypt(&ct0).unwrap(), pt0);
        server_session.set_receiving_nonce(4);
        assert_eq!(server_session.decrypt(&ct4).unwrap(), pt4);
        server_session.set_receiving_nonce(2);
        assert_eq!(server_session.decrypt(&ct2).unwrap(), pt2);
    }

    #[test]
    fn rekey_preserves_message_flow() {
        let server_kp = KeyPair::generate();
        let client_kp = KeyPair::generate();
        let server = ServerHandshake::new(&server_kp).unwrap();
        let mut client = ClientHandshake::new(&server_kp.public, &client_kp).unwrap();
        let (msg2, mut server_session, _peer_pk) =
            server.respond(&client.initiate().unwrap()).unwrap();
        let mut client_session = client.finalize(&msg2).unwrap();

        // Rekey both sides in lockstep.
        client_session.rekey_outgoing();
        server_session.rekey_incoming();
        client_session.rekey_incoming();
        server_session.rekey_outgoing();

        // Verify bidirectional flow still works.
        let pt = b"post-rekey";
        let ct = client_session.encrypt(pt).unwrap();
        assert_eq!(server_session.decrypt(&ct).unwrap(), pt);

        let ct = server_session.encrypt(pt).unwrap();
        assert_eq!(client_session.decrypt(&ct).unwrap(), pt);
    }

    #[test]
    fn multiple_messages_in_sequence() {
        let server_kp = KeyPair::generate();
        let client_kp = KeyPair::generate();
        let server = ServerHandshake::new(&server_kp).unwrap();
        let mut client = ClientHandshake::new(&server_kp.public, &client_kp).unwrap();
        let (msg2, mut server_session, _peer_pk) =
            server.respond(&client.initiate().unwrap()).unwrap();
        let mut client_session = client.finalize(&msg2).unwrap();

        // Send 10 messages each way and verify they all decrypt correctly.
        for i in 0..10 {
            let cs = format!("client msg {i}");
            let ct = client_session.encrypt(cs.as_bytes()).unwrap();
            let pt = server_session.decrypt(&ct).unwrap();
            assert_eq!(pt, cs.as_bytes());

            let ss = format!("server msg {i}");
            let ct = server_session.encrypt(ss.as_bytes()).unwrap();
            let pt = client_session.decrypt(&ct).unwrap();
            assert_eq!(pt, ss.as_bytes());
        }
    }
}
