# Phase 3a of Phase 3: Noise Protocol E2E Encryption

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** CustomUdpTransport の上に Noise_NK パターンの E2E 暗号化を被せる。host と viewer の間の全 UDP パケットを AEAD 暗号化し、host の公開鍵を viewer 側で pin することで MITM 攻撃から保護する。低遅延パイプライン(FEC、1:1 パケット構造)は維持。

**Architecture:**
- 新規クレート `prdt-crypto`: `snow` crate の薄いラッパ。`ServerHandshake`/`ClientHandshake` 型、完了後は `Session` で `encrypt`/`decrypt` を提供
- `prdt-protocol` 拡張: `PacketHeader.flags` に `ENCRYPTED` ビット、`ControlMessage` に `NoiseE1`/`NoiseE2` の 2 メッセージ(Noise_NK は 2 ラウンドトリップなので 3 メッセージだが、pattern が `NK(e, es, ss)` → `(e, ee)` で **2 メッセージ**で完了)
- `CustomUdpTransport` 統合: `EncryptedTransport` wrapper が Noise handshake を駆動、以降の `send_*`/`recv` は自動的に暗号化/復号
- host CLI: `--key-file` で ed25519/x25519 キーロード、初回は自動生成
- viewer CLI: `--host-pubkey BASE64` で host 公開鍵を pin

**Noise Protocol 選択:** `Noise_NK_25519_ChaChaPoly_BLAKE2s`
- NK: 一方向認証(viewer は anonymous、host は長期鍵)
- 25519: Curve25519 鍵交換
- ChaChaPoly: ChaCha20-Poly1305 AEAD(ハードウェア AES よりソフトウェアで速い、モバイル対応も見越し)
- BLAKE2s: handshake ハッシュ(SHA256 より速い)

**Tech Stack:** `snow = "0.9"`, `base64 = "0.22"`, `rand_core = "0.6"`(キー生成)

**Spec reference:** spec §5.10, §4.4(handshake)

---

## File Structure

```
crates/
├── crypto/                              [new crate]
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── keypair.rs                   KeyPair 型、ed25519/x25519 wrapping
│       └── session.rs                   Handshake + Session(snow wrapper)
├── protocol/
│   └── src/
│       ├── control.rs                   [modify] NoiseE1, NoiseE2 を ControlMessage に追加
│       └── wire.rs                      [modify] ENCRYPTED flag bit
├── transport/
│   └── src/
│       ├── lib.rs                       [modify] re-export
│       └── encrypted.rs                 [new] EncryptedTransport wrapper
├── host/
│   └── src/main.rs                      [modify] --key-file + keypair load
└── viewer/
    └── src/main.rs                      [modify] --host-pubkey + pin
```

---

## Task List(8 tasks)

- Task 1: `prdt-crypto` クレートと `KeyPair` / `Session` 型
- Task 2: `protocol` に Noise ControlMessage + ENCRYPTED flag 追加
- Task 3: `transport::encrypted::EncryptedTransport` 実装
- Task 4: host CLI で --key-file + pubkey 表示
- Task 5: viewer CLI で --host-pubkey + 検証
- Task 6: Integration test(暗号化 loopback round-trip)
- Task 7: E2E smoke 動作確認
- Task 8: README + `phase3a-complete` タグ

---

## Task 1: `prdt-crypto` クレート

### Cargo.toml

```toml
[package]
name = "prdt-crypto"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
snow = "0.9"
thiserror = { workspace = true }
base64 = "0.22"
rand_core = "0.6"
tracing = { workspace = true }
```

### `src/lib.rs`

```rust
//! Noise Protocol wrapper for power-remote-dt.
//! Pattern: Noise_NK_25519_ChaChaPoly_BLAKE2s.

pub mod keypair;
pub mod session;

pub use keypair::{KeyPair, PubKey, PrivKey};
pub use session::{ClientHandshake, ServerHandshake, Session, CryptoError};

pub const NOISE_PATTERN: &str = "Noise_NK_25519_ChaChaPoly_BLAKE2s";
```

### `src/keypair.rs`

```rust
//! Long-term host key pair (Curve25519 for the Noise static key) with
//! base64 encode/decode.

use base64::prelude::*;

#[derive(Debug, Clone)]
pub struct PrivKey(pub [u8; 32]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        // Curve25519 private key clamping (standard practice for X25519).
        priv_bytes[0] &= 248;
        priv_bytes[31] &= 127;
        priv_bytes[31] |= 64;

        // Derive public key via scalar multiplication on Curve25519.
        // snow's builder handles this, but we also need raw form for export.
        // Use x25519-dalek directly for derivation:
        let pub_key = x25519_dalek_pub_from_priv(&priv_bytes);
        Self {
            private: PrivKey(priv_bytes),
            public: PubKey(pub_key),
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

// NOTE: x25519_dalek_pub_from_priv should use x25519-dalek crate.
// Add x25519-dalek = "2" to Cargo.toml, or just use snow's builder
// internal to derive. For simplicity, we could store only private and
// let snow derive public at handshake time.
fn x25519_dalek_pub_from_priv(priv_bytes: &[u8; 32]) -> [u8; 32] {
    // Placeholder: implementer should use x25519-dalek crate:
    //   use x25519_dalek::{StaticSecret, PublicKey};
    //   let secret = StaticSecret::from(*priv_bytes);
    //   let pub_key = PublicKey::from(&secret);
    //   pub_key.to_bytes()
    //
    // Requires `x25519-dalek = "2"` in Cargo.toml.
    todo!("x25519 public key derivation — use x25519-dalek crate")
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
}
```

**Important**: Add `x25519-dalek = "2"` to Cargo.toml. Replace the `todo!` with the actual x25519 derivation.

### `src/session.rs`

```rust
//! Noise handshake + symmetric session wrapping.

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
    #[error("wrong state for operation: {0}")]
    WrongState(&'static str),
}

impl From<snow::Error> for CryptoError {
    fn from(e: snow::Error) -> Self {
        CryptoError::Snow(format!("{e:?}"))
    }
}

/// Server-side handshake state.
pub struct ServerHandshake {
    state: HandshakeState,
}

impl ServerHandshake {
    pub fn new(server_keypair: &KeyPair) -> Result<Self, CryptoError> {
        let params = crate::NOISE_PATTERN.parse()
            .map_err(|e: snow::Error| CryptoError::from(e))?;
        let state = Builder::new(params)
            .local_private_key(&server_keypair.private.0)
            .build_responder()?;
        Ok(Self { state })
    }

    /// Process the client's first message (e, es). Returns the server's
    /// response (e, ee), after which the handshake is complete.
    pub fn respond(mut self, client_msg: &[u8]) -> Result<(Vec<u8>, Session), CryptoError> {
        let mut buf = vec![0u8; 1024];
        let _read = self.state.read_message(client_msg, &mut buf)?;
        let mut response = vec![0u8; 1024];
        let written = self.state.write_message(&[], &mut response)?;
        response.truncate(written);
        let transport = self.state.into_transport_mode()?;
        Ok((response, Session { state: transport }))
    }
}

/// Client-side handshake state.
pub struct ClientHandshake {
    state: HandshakeState,
}

impl ClientHandshake {
    pub fn new(server_pubkey: &PubKey) -> Result<Self, CryptoError> {
        let params = crate::NOISE_PATTERN.parse()
            .map_err(|e: snow::Error| CryptoError::from(e))?;
        let state = Builder::new(params)
            .remote_public_key(&server_pubkey.0)
            .build_initiator()?;
        Ok(Self { state })
    }

    /// Produce the first handshake message (e, es).
    pub fn initiate(&mut self) -> Result<Vec<u8>, CryptoError> {
        let mut buf = vec![0u8; 1024];
        let written = self.state.write_message(&[], &mut buf)?;
        buf.truncate(written);
        Ok(buf)
    }

    /// Finalize by reading the server's response (e, ee).
    pub fn finalize(mut self, server_msg: &[u8]) -> Result<Session, CryptoError> {
        let mut buf = vec![0u8; 1024];
        self.state.read_message(server_msg, &mut buf)?;
        let transport = self.state.into_transport_mode()?;
        Ok(Session { state: transport })
    }
}

/// Active symmetric session. Each direction has its own key; within a
/// direction, snow auto-increments the nonce per message.
pub struct Session {
    state: TransportState,
}

impl Session {
    /// Encrypt plaintext in-place (returns ciphertext + 16 bytes AEAD tag).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = vec![0u8; plaintext.len() + 16];
        let written = self.state.write_message(plaintext, &mut out)?;
        out.truncate(written);
        Ok(out)
    }

    /// Decrypt ciphertext. Rejects replays (monotonic nonce).
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = vec![0u8; ciphertext.len()];
        let written = self.state.read_message(ciphertext, &mut out)?;
        out.truncate(written);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keypair::KeyPair;

    #[test]
    fn full_handshake_and_roundtrip() {
        let server_kp = KeyPair::generate();
        let server = ServerHandshake::new(&server_kp).unwrap();
        let mut client = ClientHandshake::new(&server_kp.public).unwrap();

        let msg1 = client.initiate().unwrap();
        let (msg2, mut server_session) = server.respond(&msg1).unwrap();
        let mut client_session = client.finalize(&msg2).unwrap();

        // Client → Server
        let plaintext = b"hello world";
        let ct = client_session.encrypt(plaintext).unwrap();
        let pt = server_session.decrypt(&ct).unwrap();
        assert_eq!(&pt, plaintext);

        // Server → Client
        let plaintext2 = b"reply";
        let ct2 = server_session.encrypt(plaintext2).unwrap();
        let pt2 = client_session.decrypt(&ct2).unwrap();
        assert_eq!(&pt2, plaintext2);
    }

    #[test]
    fn wrong_pubkey_fails() {
        let real = KeyPair::generate();
        let _fake = KeyPair::generate();
        let server = ServerHandshake::new(&real).unwrap();
        // Client connects with a DIFFERENT pubkey than server actually has.
        let mut client = ClientHandshake::new(&_fake.public).unwrap();

        let msg1 = client.initiate().unwrap();
        // Server will decrypt with its real static key; the ephemeral/static
        // combo won't match what client encrypted to (_fake). Handshake fails.
        let res = server.respond(&msg1);
        assert!(res.is_err(), "handshake with wrong pubkey should fail");
    }
}
```

### Update workspace Cargo.toml to include the new crate

Add `"crates/crypto"` to `workspace.members`.

### Steps
1. Create `crates/crypto/Cargo.toml`, `src/lib.rs`, `src/keypair.rs`, `src/session.rs`
2. Add `"crates/crypto"` to workspace members
3. Add `x25519-dalek = "2"` dep + implement `x25519_dalek_pub_from_priv`
4. Run `cargo test -p prdt-crypto` — expected: 3 tests pass
5. Clippy clean, fmt clean
6. Commit: `feat(crypto): add Noise_NK protocol wrapper (snow + x25519)`

---

## Task 2: Protocol extensions

### Modify `crates/protocol/src/control.rs`:

Add new ControlMessage variants:
```rust
/// Noise handshake stage 1 (initiator → responder).
NoiseE1 { payload: Vec<u8> },
/// Noise handshake stage 2 (responder → initiator).
NoiseE2 { payload: Vec<u8> },
```

Update `kind_u8()` to assign bytes 10/11:
```rust
Self::NoiseE1 { .. } => 10,
Self::NoiseE2 { .. } => 11,
```

### Modify `crates/protocol/src/wire.rs`:

Add `ENCRYPTED` flag bit to `PacketHeader`:
```rust
pub mod packet_flags {
    pub const ENCRYPTED: u8 = 0b0000_0001;
}
```

Update decode_control to handle kinds 10/11.

### Steps

1. Add variants, update kind_u8, update bincode-heuristic
2. Run tests: all 29 protocol tests still pass + control_all_kinds_round_trip updated for new variants
3. Commit: `feat(protocol): add NoiseE1/NoiseE2 control messages + ENCRYPTED flag`

---

## Task 3: `transport::encrypted::EncryptedTransport`

New module wrapping `CustomUdpTransport` with a Noise session:

```rust
// crates/transport/src/encrypted.rs

use std::sync::Arc;

use async_trait::async_trait;
use prdt_crypto::{ClientHandshake, PubKey, ServerHandshake, Session};
use prdt_protocol::{control::ControlMessage, input::InputEvent, EncodedFrame};
use tokio::sync::Mutex;

use crate::error::TransportError;
use crate::transport_trait::{ReceivedMessage, Transport};
use crate::CustomUdpTransport;

pub struct EncryptedTransport {
    inner: Arc<CustomUdpTransport>,
    tx_session: Mutex<Option<Session>>,
    rx_session: Mutex<Option<Session>>,
}

impl EncryptedTransport {
    pub fn new(inner: Arc<CustomUdpTransport>) -> Self {
        Self { inner, tx_session: Mutex::new(None), rx_session: Mutex::new(None) }
    }

    /// Viewer side: perform Noise handshake with the remote server.
    /// Blocks until complete. Must be called before any other send/recv.
    pub async fn client_handshake(
        &self,
        server_pubkey: &PubKey,
    ) -> Result<(), TransportError> {
        let mut hs = ClientHandshake::new(server_pubkey)
            .map_err(|e| TransportError::Io(std::io::Error::new(
                std::io::ErrorKind::Other, format!("crypto: {e}")
            )))?;
        let e1 = hs.initiate().map_err(|e| ...)?;
        self.inner.send_control(ControlMessage::NoiseE1 { payload: e1 }).await?;

        loop {
            match self.inner.recv().await? {
                ReceivedMessage::Control(ControlMessage::NoiseE2 { payload }) => {
                    let session = hs.finalize(&payload).map_err(|e| ...)?;
                    // Noise_NK: client uses one direction, server the other,
                    // snow handles this in Session internally.
                    *self.tx_session.lock().await = Some(session.clone_or_split_tx());
                    *self.rx_session.lock().await = Some(session.clone_or_split_rx());
                    return Ok(());
                }
                _ => continue,
            }
        }
    }

    pub async fn server_handshake(
        &self,
        server_keypair: &prdt_crypto::KeyPair,
    ) -> Result<(), TransportError> {
        let mut hs: Option<ServerHandshake> = Some(
            ServerHandshake::new(server_keypair).map_err(|e| ...)?
        );
        loop {
            match self.inner.recv().await? {
                ReceivedMessage::Control(ControlMessage::NoiseE1 { payload }) => {
                    let hs_taken = hs.take().unwrap();
                    let (e2, session) = hs_taken.respond(&payload).map_err(|e| ...)?;
                    self.inner.send_control(ControlMessage::NoiseE2 { payload: e2 }).await?;
                    *self.tx_session.lock().await = Some(...);
                    *self.rx_session.lock().await = Some(...);
                    return Ok(());
                }
                _ => continue,
            }
        }
    }
}

// NOTE: snow's TransportState is ONE session for both directions. snow
// automatically uses separate keys internally. We can just use one `Mutex<Session>`
// and call encrypt/decrypt on the same Session. Simpler than split.

#[async_trait]
impl Transport for EncryptedTransport {
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError> {
        // Serialize frame, encrypt, wrap in a Video packet with ENCRYPTED flag.
        // ... requires lower-level packet construction
        todo!()
    }
    // ... similar for send_input, send_control, recv
}
```

**IMPORTANT simplification**: rather than re-implementing the send/recv over a new wrapper, a cleaner approach may be to add encrypt/decrypt hooks INSIDE `CustomUdpTransport` itself. Add `encrypt_state: Option<Session>` field, and encrypt the `body` bytes at the outermost send_raw / recv level. Flag the packet header with ENCRYPTED bit. This way all existing FEC + reassembler logic works unchanged — encryption is just at the bytes layer.

**Recommended design**: Encrypt the UDP **datagram body** (bytes after PacketHeader), not individual control/video packets. So:
- Pre-handshake: NoiseE1 / NoiseE2 sent as regular ControlMessage (unencrypted)
- Post-handshake: set `ENCRYPTED` flag bit on header, and encrypt the entire body before sendto; decrypt on recv
- Assembler, FEC, VideoPacket/InputPacket/ControlPacket parsing all work on decrypted body unchanged

**However**: encrypting at the datagram level adds 16 bytes AEAD tag per datagram. For a 1200B chunk this is ~1.3% overhead. Fine.

Implementation plan for Task 3:
1. Add `encrypt_state: Mutex<Option<Session>>` to CustomUdpTransport (or a subclass/wrapper)
2. Add `handshake_as_client(&self, server_pubkey)` method that runs the Noise flow
3. Add `handshake_as_server(&self, server_keypair)` method
4. Modify `send_raw` to wrap body in `encrypt()` if state is set, set ENCRYPTED flag
5. Modify `recv` to detect ENCRYPTED flag, decrypt body before parsing

Simpler path. Let me go with this design.

### Steps
1. Add encryption fields + handshake methods to `CustomUdpTransport`
2. Modify `send_raw` and the internal recv loop to enc/dec
3. Integration test: 2 CustomUdpTransport instances, handshake, encrypted round-trip
4. Commit: `feat(transport): add Noise_NK encryption to CustomUdpTransport`

---

## Task 4-5: Host / Viewer CLI integration

### Host (`crates/host/src/main.rs`)

Add:
```rust
/// Path to host long-term key file (generated on first run).
#[arg(long, default_value = "host-key.bin")]
key_file: std::path::PathBuf,
```

Logic:
1. Try to read key from file
2. If file doesn't exist: generate KeyPair, save private key to file (restrictive permissions), print pubkey as base64 to stdout
3. After UDP bind but before host_handshake, call `transport.handshake_as_server(&keypair)`
4. Then existing host_handshake (over encrypted channel) completes

### Viewer (`crates/viewer/src/main.rs`)

Add:
```rust
/// Host's public key in base64. Required for Noise handshake.
#[arg(long)]
host_pubkey: String,
```

Logic:
1. Parse pubkey via `PubKey::from_base64`
2. After UDP socket setup, before `viewer_handshake`, call `transport.handshake_as_client(&pubkey)`

### Commits
- `feat(host): add --key-file and Noise server handshake`
- `feat(viewer): add --host-pubkey and Noise client handshake`

---

## Task 6: Integration test

`crates/transport/tests/encrypted_test.rs`: spin up 2 CustomUdpTransport instances on loopback, handshake (one as server, one as client), exchange Video + Input + Control messages, assert all round-trip correctly AND that on-wire bytes are encrypted (different from plaintext).

---

## Task 7: E2E smoke

User runs host + viewer with new args:
```powershell
# Terminal 1
.\target\release\prdt-host.exe --bind 127.0.0.1:9000 --monitor 0 --bitrate-mbps 20 --key-file host-key.bin
# First run prints: "Host public key: AAAA..."

# Terminal 2
.\target\release\prdt-viewer.exe --host 127.0.0.1:9000 --host-pubkey <paste>
```

Verify everything still works.

---

## Task 8: README + tag

Update README.md: document encryption, key management model (TOFU), security limitations (no forward secrecy rotation yet, no key pinning file, etc.).

Tag `phase3a-complete`.

---

## Exit Criteria

- [ ] `cargo test -p prdt-crypto -p prdt-transport` passes
- [ ] on-wire bytes are encrypted (integration test asserts)
- [ ] wrong pubkey → handshake fails (test case)
- [ ] E2E smoke: host prints pubkey, viewer connects with it, video flows
- [ ] Tag `phase3a-complete`

---

## Known Limitations after Phase 3a

1. **TOFU only** — viewer blindly trusts whatever pubkey it's told. A real MITM on first use can swap the key. Phase 3b could add known-hosts file, or Phase 5 adds a central directory/CA.
2. **No forward secrecy rotation** — session keys live for the whole session. Long sessions should re-key periodically (snow supports this via `rekey_*` methods); deferred.
3. **Host key stored in plain file** — no OS keyring integration. Acceptable for Phase 3a PoC; Phase 4 polish should use Windows DPAPI / macOS Keychain / Linux Secret Service.
4. **No replay protection on handshake** — the NoiseE1 message is accepted blindly by host. An attacker who snoops a prior handshake could replay E1 and get a fresh session token but wouldn't be able to decrypt subsequent packets (different ephemeral keys each handshake). Acceptable but note it.
5. **No downgrade protection** — if Phase 3b adds multiple encryption modes, need to bind protocol version into handshake hash. Deferred.

---

*End of Phase 3a.*
