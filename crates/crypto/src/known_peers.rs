//! Parser for a known-peer-ids file.
//!
//! Format: one entry per line, `<label> <base64-pubkey>`.
//! Lines starting with `#` are comments. Blank lines ignored.
//!
//! `<label>` is a free-form string with no whitespace. By default the host
//! uses the pubkey itself as the label when remembering a newly-accepted
//! peer; users can rename labels later by editing the file.

use std::collections::HashMap;
use std::path::Path;

use crate::keypair::PubKey;

#[derive(Debug, thiserror::Error)]
pub enum KnownPeersError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: {reason}")]
    Parse { line: usize, reason: String },
}

#[derive(Debug, Default, Clone)]
pub struct KnownPeers {
    /// pubkey → label
    entries: HashMap<PubKey, String>,
}

impl KnownPeers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from file. Missing file is an error. Empty/all-comment file is OK.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, KnownPeersError> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// Load from file, but a missing file yields an empty `KnownPeers` rather
    /// than an error. Used by the host's accept gate so first-run behavior
    /// (no file yet) is "no peers known" instead of a hard fail.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_and_comments() {
        let kp = KnownPeers::parse("").unwrap();
        assert!(kp.is_empty());
        let kp = KnownPeers::parse("# header\n\n# blah\n\t # indented\n").unwrap();
        assert!(kp.is_empty());
    }

    #[test]
    fn insert_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers");
        let mut kp = KnownPeers::new();
        let pk1 = PubKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let pk2 = PubKey([1u8; 32]);
        kp.insert(pk1, "alice".into());
        kp.insert(pk2, "bob".into());
        kp.save(&path).unwrap();

        let reloaded = KnownPeers::load(&path).unwrap();
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
        let err = KnownPeers::parse(content).unwrap_err();
        match err {
            KnownPeersError::Parse { line: 1, .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn load_or_default_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist");
        let kp = KnownPeers::load_or_default(&path).unwrap();
        assert!(kp.is_empty());
    }

    #[test]
    fn contains_after_insert() {
        let mut kp = KnownPeers::new();
        let pk = PubKey([7u8; 32]);
        assert!(!kp.contains(&pk));
        kp.insert(pk, pk.to_base64());
        assert!(kp.contains(&pk));
        assert_eq!(kp.label(&pk), Some(pk.to_base64()).as_deref());
    }

    #[test]
    fn parse_missing_pubkey_fails() {
        let content = "alice\n";
        let err = KnownPeers::parse(content).unwrap_err();
        match err {
            KnownPeersError::Parse { line: 1, .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
