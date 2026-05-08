//! Long-term host key pair (Curve25519) with base64 encode/decode.

use base64::prelude::*;
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Debug, Clone)]
pub struct PrivKey(pub [u8; 32]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PubKey(pub [u8; 32]);

#[derive(Debug, Clone)]
pub struct KeyPair {
    pub private: PrivKey,
    pub public: PubKey,
}

impl KeyPair {
    /// Generate a fresh random key pair.
    pub fn generate() -> Self {
        use rand_core::{OsRng, RngCore};
        let mut priv_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut priv_bytes);
        let secret = StaticSecret::from(priv_bytes);
        let public = PublicKey::from(&secret);
        // Extract the clamped secret bytes from the StaticSecret.
        let clamped_priv = secret.to_bytes();
        Self {
            private: PrivKey(clamped_priv),
            public: PubKey(public.to_bytes()),
        }
    }

    /// Recompute the public key from a private key. Useful when loading
    /// a saved private key from disk.
    pub fn from_private(priv_bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(priv_bytes);
        let public = PublicKey::from(&secret);
        Self {
            private: PrivKey(secret.to_bytes()),
            public: PubKey(public.to_bytes()),
        }
    }
}

impl PubKey {
    pub fn to_base64(&self) -> String {
        BASE64_STANDARD_NO_PAD.encode(self.0)
    }
    pub fn from_base64(s: &str) -> Result<Self, String> {
        let bytes = BASE64_STANDARD_NO_PAD
            .decode(s.trim())
            .map_err(|e| format!("base64: {e}"))?;
        if bytes.len() != 32 {
            return Err(format!("expected 32 bytes, got {}", bytes.len()));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(PubKey(arr))
    }
}

impl PrivKey {
    pub fn to_base64(&self) -> String {
        BASE64_STANDARD_NO_PAD.encode(self.0)
    }
    pub fn from_base64(s: &str) -> Result<Self, String> {
        let bytes = BASE64_STANDARD_NO_PAD
            .decode(s.trim())
            .map_err(|e| format!("base64: {e}"))?;
        if bytes.len() != 32 {
            return Err(format!("expected 32 bytes, got {}", bytes.len()));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(PrivKey(arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_generate_and_serialize() {
        let kp = KeyPair::generate();
        let pub_b64 = kp.public.to_base64();
        let parsed = PubKey::from_base64(&pub_b64).unwrap();
        assert_eq!(parsed.0, kp.public.0);
    }

    #[test]
    fn keypair_from_private_matches() {
        let kp1 = KeyPair::generate();
        let kp2 = KeyPair::from_private(kp1.private.0);
        assert_eq!(kp1.public.0, kp2.public.0);
    }

    #[test]
    fn pubkey_malformed_base64_errors() {
        assert!(PubKey::from_base64("not-base64!@#$").is_err());
        assert!(PubKey::from_base64("c2hvcnQ=").is_err()); // too short
    }
}
