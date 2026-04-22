//! Noise Protocol wrapper for power-remote-dt.
//! Pattern: Noise_NK_25519_ChaChaPoly_BLAKE2s.

pub mod keypair;
pub mod session;

pub use keypair::{KeyPair, PrivKey, PubKey};
pub use session::{ClientHandshake, CryptoError, ServerHandshake, Session};

pub const NOISE_PATTERN: &str = "Noise_NK_25519_ChaChaPoly_BLAKE2s";
