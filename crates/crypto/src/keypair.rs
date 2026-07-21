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

    /// Full key fingerprint: the SHA-256 of the 32 raw public-key bytes,
    /// rendered as uppercase hex in colon-separated byte pairs (SSH style,
    /// e.g. `A1:B2:C3:...`). Stable for a given key; used for out-of-band
    /// verification (AC-14) and to key the persisted identity record.
    pub fn fingerprint_full(&self) -> String {
        let digest = Self::sha256(&self.0);
        let mut out = String::with_capacity(digest.len() * 3);
        for (i, b) in digest.iter().enumerate() {
            if i > 0 {
                out.push(':');
            }
            out.push_str(&format!("{b:02X}"));
        }
        out
    }

    /// Short fingerprint: the first 4 SHA-256 bytes as `AABB:CCDD`. Compact
    /// enough to read aloud while still distinguishing keys in practice.
    pub fn fingerprint_short(&self) -> String {
        let d = Self::sha256(&self.0);
        format!("{:02X}{:02X}:{:02X}{:02X}", d[0], d[1], d[2], d[3])
    }

    fn sha256(bytes: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().into()
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

    #[test]
    fn fingerprint_is_stable_and_distinguishes_keys() {
        let a = PubKey([0u8; 32]);
        let b = PubKey([1u8; 32]);
        // Deterministic for a given key.
        assert_eq!(a.fingerprint_full(), a.fingerprint_full());
        assert_eq!(a.fingerprint_short(), a.fingerprint_short());
        // Different keys → different fingerprints.
        assert_ne!(a.fingerprint_full(), b.fingerprint_full());
        assert_ne!(a.fingerprint_short(), b.fingerprint_short());
        // Full is 32 uppercase-hex byte pairs joined by ':' (95 chars).
        let full = a.fingerprint_full();
        assert_eq!(full.len(), 32 * 2 + 31);
        assert!(full.split(':').all(|p| p.len() == 2));
        // Short is `AABB:CCDD`.
        assert_eq!(a.fingerprint_short().len(), 9);
    }
}
