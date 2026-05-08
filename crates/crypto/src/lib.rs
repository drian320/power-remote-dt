//! Noise Protocol wrapper for power-remote-dt.
//!
//! Pattern: Noise_IK_25519_ChaChaPoly_BLAKE2s — both sides have static keys.
//! The viewer (initiator) transmits its static pubkey to the host inside the
//! first encrypted handshake message; the host can therefore identify the
//! viewer cryptographically and gate `accept` on a known-peer-ids list.
//!
//! NOTE: PR5b is a wire-incompatible change from the previous NK pattern.
//! Pre-PR5b releases were not tagged for compatibility, so no migration is
//! provided.

pub mod keypair;
pub mod known_hosts;
pub mod session;

pub use keypair::{KeyPair, PrivKey, PubKey};
pub use known_hosts::{KnownHosts, KnownHostsError, TofuVerdict};
pub use session::{ClientHandshake, CryptoError, ServerHandshake, Session};

pub const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
