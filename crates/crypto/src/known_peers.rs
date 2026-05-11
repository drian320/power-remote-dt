//! Parser for a known-peer-ids file.
//!
//! Two representations are provided:
//!
//! - [`KnownPeersFile`]: legacy text-file format (`<label> <base64-pubkey>` per
//!   line). Used by the host accept gate (pre-P6 TOFU path).
//!
//! - [`KnownPeers`] / [`KnownPeer`]: P6 TOML-based store. Persisted to
//!   `host-peers.toml`. Each peer carries a [`prdt_protocol::PermissionSet`]
//!   and timestamps.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use prdt_protocol::PermissionSet;
use serde::{Deserialize, Serialize};

use crate::keypair::PubKey;

// ---------------------------------------------------------------------------
// Legacy text-file KnownPeers (pre-P6)
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum KnownPeersError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: {reason}")]
    Parse { line: usize, reason: String },
}

/// Text-file known-peers store: `<label> <base64-pubkey>` per line.
///
/// Kept for the pre-P6 TOFU accept gate. P6 code uses [`KnownPeers`] instead.
#[derive(Debug, Default, Clone)]
pub struct KnownPeersFile {
    /// pubkey → label
    entries: HashMap<PubKey, String>,
}

impl KnownPeersFile {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from file. Missing file is an error. Empty/all-comment file is OK.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, KnownPeersError> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// Load from file, but a missing file yields an empty `KnownPeersFile`
    /// rather than an error. Used by the host's accept gate so first-run
    /// behavior (no file yet) is "no peers known" instead of a hard fail.
    pub fn load_or_default<P: AsRef<Path>>(path: P) -> Result<Self, KnownPeersError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::new());
        }
        Self::load(path)
    }

    pub fn parse(content: &str) -> Result<Self, KnownPeersError> {
        let mut entries = HashMap::new();
        for (i, raw_line) in content.lines().enumerate() {
            let line_no = i + 1;
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let label = parts.next().ok_or_else(|| KnownPeersError::Parse {
                line: line_no,
                reason: "missing label".into(),
            })?;
            let pubkey_str = parts
                .next()
                .ok_or_else(|| KnownPeersError::Parse {
                    line: line_no,
                    reason: "missing pubkey".into(),
                })?
                .trim();
            let pubkey = PubKey::from_base64(pubkey_str).map_err(|e| KnownPeersError::Parse {
                line: line_no,
                reason: format!("bad pubkey: {e}"),
            })?;
            entries.insert(pubkey, label.to_string());
        }
        Ok(Self { entries })
    }

    pub fn contains(&self, pk: &PubKey) -> bool {
        self.entries.contains_key(pk)
    }

    pub fn label(&self, pk: &PubKey) -> Option<&str> {
        self.entries.get(pk).map(|s| s.as_str())
    }

    /// Iterate over all `(PubKey, label)` pairs. Used for migration to the
    /// P6 TOML `KnownPeers` format.
    pub fn entries_iter(&self) -> impl Iterator<Item = (&PubKey, &str)> {
        self.entries.iter().map(|(pk, label)| (pk, label.as_str()))
    }

    pub fn insert(&mut self, pk: PubKey, label: String) {
        self.entries.insert(pk, label);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Serialize to the same plaintext format `parse` accepts. Entries are
    /// sorted by base64-pubkey so the file is stable under git.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), KnownPeersError> {
        let mut rows: Vec<(String, &String)> = self
            .entries
            .iter()
            .map(|(pk, label)| (pk.to_base64(), label))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let mut content = String::new();
        for (pk_b64, label) in rows {
            content.push_str(label);
            content.push(' ');
            content.push_str(&pk_b64);
            content.push('\n');
        }
        std::fs::write(path, content)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// P6 TOML-based KnownPeer / KnownPeers
// ---------------------------------------------------------------------------

/// A single remembered peer entry (P6 schema).
///
/// `permissions` and the timestamp fields use `#[serde(default)]` so that
/// legacy TOML rows that pre-date P6 deserialize safely:
/// - `permissions` → `PermissionSet::deny_all()` (secure default)
/// - `first_seen_at` / `last_seen_at` → `UNIX_EPOCH`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownPeer {
    pub pubkey_b64: String,
    pub label: String,
    #[serde(default)]
    pub permissions: PermissionSet,
    #[serde(default = "epoch")]
    pub first_seen_at: SystemTime,
    #[serde(default = "epoch")]
    pub last_seen_at: SystemTime,
}

fn epoch() -> SystemTime {
    UNIX_EPOCH
}

/// TOML-based store of remembered peers (P6).
///
/// Serializes to `[[peers]]` array-of-tables format, e.g.:
/// ```toml
/// [[peers]]
/// pubkey_b64 = "..."
/// label = "laptop"
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct KnownPeers {
    #[serde(default)]
    pub peers: Vec<KnownPeer>,
}

impl KnownPeers {
    /// Load from `path`. A missing file returns an empty `KnownPeers`
    /// (first-run: no peers yet). Malformed TOML returns an error.
    pub fn load_or_default(path: &Path) -> Result<Self, KnownPeersError> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let store: KnownPeers = toml::from_str(&s).map_err(|e| KnownPeersError::Parse {
                    line: 0,
                    reason: e.to_string(),
                })?;
                Ok(store)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(KnownPeersError::Io(e)),
        }
    }

    /// Atomically write to `path` using a PID-suffixed temp file so concurrent
    /// processes do not clobber each other's in-progress writes.
    ///
    /// Peers are sorted by `pubkey_b64` before serialising so repeated save
    /// operations on logically identical stores produce byte-identical output
    /// (deterministic, git-stable, no spurious diffs).
    pub fn save(&self, path: &Path) -> Result<(), KnownPeersError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
        // Clone + sort so insertion order in self.peers is preserved for callers.
        let mut sorted = self.clone();
        sorted.peers.sort_by(|a, b| a.pubkey_b64.cmp(&b.pubkey_b64));
        let s = toml::to_string_pretty(&sorted).map_err(|e| KnownPeersError::Parse {
            line: 0,
            reason: e.to_string(),
        })?;
        std::fs::write(&tmp, s)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Remove the peer identified by `pubkey_b64`. Returns `true` if a peer
    /// was found and removed, `false` if no match.
    pub fn remove_by_pubkey(&mut self, pubkey_b64: &str) -> bool {
        let before = self.peers.len();
        self.peers.retain(|p| p.pubkey_b64 != pubkey_b64);
        self.peers.len() < before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Legacy KnownPeersFile tests (unchanged)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_empty_and_comments() {
        let kp = KnownPeersFile::parse("").unwrap();
        assert!(kp.is_empty());
        let kp = KnownPeersFile::parse("# header\n\n# blah\n\t # indented\n").unwrap();
        assert!(kp.is_empty());
    }

    #[test]
    fn insert_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers");
        let mut kp = KnownPeersFile::new();
        let pk1 = PubKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let pk2 = PubKey([1u8; 32]);
        kp.insert(pk1, "alice".into());
        kp.insert(pk2, "bob".into());
        kp.save(&path).unwrap();

        let reloaded = KnownPeersFile::load(&path).unwrap();
        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.contains(&pk1));
        assert!(reloaded.contains(&pk2));
        assert_eq!(reloaded.label(&pk1), Some("alice"));
        assert_eq!(reloaded.label(&pk2), Some("bob"));

        // Determinism: saving the reloaded set produces byte-identical output.
        let path2 = dir.path().join("peers2");
        reloaded.save(&path2).unwrap();
        let a = std::fs::read_to_string(&path).unwrap();
        let b = std::fs::read_to_string(&path2).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parse_rejects_bad_pubkey() {
        let content = "alice not_base64!!!\n";
        let err = KnownPeersFile::parse(content).unwrap_err();
        match err {
            KnownPeersError::Parse { line: 1, .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn load_or_default_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist");
        let kp = KnownPeersFile::load_or_default(&path).unwrap();
        assert!(kp.is_empty());
    }

    #[test]
    fn contains_after_insert() {
        let mut kp = KnownPeersFile::new();
        let pk = PubKey([7u8; 32]);
        assert!(!kp.contains(&pk));
        kp.insert(pk, pk.to_base64());
        assert!(kp.contains(&pk));
        assert_eq!(kp.label(&pk), Some(pk.to_base64()).as_deref());
    }

    #[test]
    fn parse_missing_pubkey_fails() {
        let content = "alice\n";
        let err = KnownPeersFile::parse(content).unwrap_err();
        match err {
            KnownPeersError::Parse { line: 1, .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // P6 KnownPeer / KnownPeers TOML tests
    // -----------------------------------------------------------------------

    #[test]
    fn legacy_known_peer_loads_with_default_permissions() {
        // Pre-P6 format: only pubkey_b64 + label, no permissions/timestamps.
        let toml_str = r#"
[[peers]]
pubkey_b64 = "abc"
label = "old-laptop"
"#;
        let store: KnownPeers = toml::from_str(toml_str).expect("legacy format should parse");
        assert_eq!(store.peers.len(), 1);
        let p = &store.peers[0];
        assert_eq!(p.label, "old-laptop");
        // Defaulted serde-default fields.
        assert_eq!(p.permissions, prdt_protocol::PermissionSet::default()); // = deny_all
    }

    #[test]
    fn known_peer_round_trip_with_permissions() {
        use std::time::UNIX_EPOCH;
        let now = SystemTime::now();
        let p = KnownPeer {
            pubkey_b64: "xyz".into(),
            label: "work".into(),
            permissions: prdt_protocol::PermissionSet::all(),
            first_seen_at: UNIX_EPOCH,
            last_seen_at: now,
        };
        let s = toml::to_string(&KnownPeers {
            peers: vec![p.clone()],
        })
        .unwrap();
        let back: KnownPeers = toml::from_str(&s).unwrap();
        assert_eq!(back.peers[0].pubkey_b64, p.pubkey_b64);
        assert_eq!(back.peers[0].permissions, p.permissions);
    }

    // -----------------------------------------------------------------------
    // KnownPeers load_or_default / save / remove_by_pubkey tests
    // -----------------------------------------------------------------------

    fn make_peer(pubkey: &str, label: &str) -> KnownPeer {
        KnownPeer {
            pubkey_b64: pubkey.into(),
            label: label.into(),
            permissions: prdt_protocol::PermissionSet::all(),
            first_seen_at: std::time::UNIX_EPOCH,
            last_seen_at: std::time::UNIX_EPOCH,
        }
    }

    #[test]
    fn known_peers_load_or_default_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-peers.toml");
        let store = KnownPeers::load_or_default(&path).unwrap();
        assert!(store.peers.is_empty());
    }

    #[test]
    fn known_peers_save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-peers.toml");
        let mut store = KnownPeers::default();
        store.peers.push(make_peer("AAAA", "alice"));
        store.peers.push(make_peer("BBBB", "bob"));
        store.save(&path).unwrap();

        let loaded = KnownPeers::load_or_default(&path).unwrap();
        assert_eq!(loaded.peers.len(), 2);
        assert_eq!(loaded.peers[0].pubkey_b64, "AAAA");
        assert_eq!(loaded.peers[1].label, "bob");
    }

    #[test]
    fn known_peers_remove_by_pubkey_removes_and_reports() {
        let mut store = KnownPeers::default();
        store.peers.push(make_peer("AAAA", "alice"));
        store.peers.push(make_peer("BBBB", "bob"));

        assert!(
            store.remove_by_pubkey("AAAA"),
            "should return true when peer found"
        );
        assert_eq!(store.peers.len(), 1);
        assert_eq!(store.peers[0].pubkey_b64, "BBBB");

        assert!(
            !store.remove_by_pubkey("ZZZZ"),
            "should return false for missing peer"
        );
        assert_eq!(store.peers.len(), 1);
    }

    #[test]
    fn known_peers_delete_removes_peer_and_save_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-peers.toml");
        let mut store = KnownPeers::default();
        store.peers.push(make_peer("AAAA", "alice"));
        store.peers.push(make_peer("BBBB", "bob"));
        store.save(&path).unwrap();

        let mut loaded = KnownPeers::load_or_default(&path).unwrap();
        loaded.remove_by_pubkey("AAAA");
        loaded.save(&path).unwrap();

        let final_store = KnownPeers::load_or_default(&path).unwrap();
        assert_eq!(final_store.peers.len(), 1);
        assert_eq!(final_store.peers[0].pubkey_b64, "BBBB");
    }

    #[test]
    fn save_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let path1 = dir.path().join("peers-a.toml");
        let path2 = dir.path().join("peers-b.toml");

        // Insert in reverse order — save should sort both times and produce identical bytes.
        let mut store = KnownPeers::default();
        store.peers.push(make_peer("ZZZZ", "zara"));
        store.peers.push(make_peer("AAAA", "alice"));
        store.peers.push(make_peer("MMMM", "mallory"));
        store.save(&path1).unwrap();
        store.save(&path2).unwrap();

        let bytes1 = std::fs::read(&path1).unwrap();
        let bytes2 = std::fs::read(&path2).unwrap();
        assert_eq!(
            bytes1, bytes2,
            "two saves of identical store must be byte-equal"
        );

        // Reload and save again — still deterministic.
        let reloaded = KnownPeers::load_or_default(&path1).unwrap();
        let path3 = dir.path().join("peers-c.toml");
        reloaded.save(&path3).unwrap();
        let bytes3 = std::fs::read(&path3).unwrap();
        assert_eq!(
            bytes1, bytes3,
            "reload-then-save must be byte-equal to original"
        );
    }
}
