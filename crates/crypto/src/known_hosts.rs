//! Parser for a known_hosts file.
//!
//! Format: one entry per line, `<host-key> <base64-pubkey>`.
//! Lines starting with `#` are comments. Blank lines ignored.

use std::collections::HashMap;
use std::path::Path;

use crate::keypair::PubKey;

#[derive(Debug, thiserror::Error)]
pub enum KnownHostsError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: {reason}")]
    Parse { line: usize, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TofuVerdict {
    FirstSeen,
    Matched,
    Mismatch { expected: PubKey, got: PubKey },
}

#[derive(Debug, Default, Clone)]
pub struct KnownHosts {
    entries: HashMap<String, PubKey>,
}

impl KnownHosts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from file. Missing file is an error. Empty/all-comment file is OK.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, KnownHostsError> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    pub fn parse(content: &str) -> Result<Self, KnownHostsError> {
        let mut entries = HashMap::new();
        for (i, raw_line) in content.lines().enumerate() {
            let line_no = i + 1;
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let host = parts.next().ok_or_else(|| KnownHostsError::Parse {
                line: line_no,
                reason: "missing host key".into(),
            })?;
            let pubkey_str = parts
                .next()
                .ok_or_else(|| KnownHostsError::Parse {
                    line: line_no,
                    reason: "missing pubkey".into(),
                })?
                .trim();
            let pubkey = PubKey::from_base64(pubkey_str).map_err(|e| KnownHostsError::Parse {
                line: line_no,
                reason: format!("bad pubkey: {e}"),
            })?;
            entries.insert(host.to_string(), pubkey);
        }
        Ok(Self { entries })
    }

    pub fn get(&self, host: &str) -> Option<&PubKey> {
        self.entries.get(host)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(&mut self, host_key: String, pubkey: PubKey) {
        self.entries.insert(host_key, pubkey);
    }

    /// Render to the plaintext format `parse` accepts.
    fn serialize(&self) -> String {
        let mut content = String::new();
        let mut keys: Vec<&String> = self.entries.keys().collect();
        keys.sort();
        for k in keys {
            let pk = &self.entries[k];
            content.push_str(k);
            content.push(' ');
            content.push_str(&pk.to_base64());
            content.push('\n');
        }
        content
    }

    /// Serialize to disk atomically (temp-file + `fsync` + `rename`), so a
    /// crash mid-write never truncates the file (AC-12).
    ///
    /// NOTE: `save` alone does not guard against a concurrent read-modify-write
    /// by another process; callers mutating shared state should go through
    /// [`KnownHosts::verify_or_record`], which holds an advisory lock across
    /// the whole cycle.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), KnownHostsError> {
        crate::atomic::atomic_write(path.as_ref(), self.serialize().as_bytes())?;
        Ok(())
    }

    /// TOFU: create-if-missing, verify-if-present. Records on first sight.
    ///
    /// The entire read-modify-write runs under an exclusive advisory lock so
    /// two processes recording *different* hosts concurrently cannot lose each
    /// other's entries (the pre-AC-12 unlocked path did: both read the old
    /// file, both wrote back only their own addition). The final write is
    /// atomic via [`KnownHosts::save`].
    pub fn verify_or_record<P: AsRef<Path>>(
        path: P,
        host_key: &str,
        pubkey: &PubKey,
    ) -> Result<TofuVerdict, KnownHostsError> {
        let path = path.as_ref();
        let _lock = crate::atomic::FileLock::acquire(path)?;
        let mut kh = if path.exists() {
            Self::load(path)?
        } else {
            Self::new()
        };
        let verdict = match kh.get(host_key) {
            None => {
                kh.insert(host_key.to_string(), *pubkey);
                kh.save(path)?;
                TofuVerdict::FirstSeen
            }
            Some(existing) if existing == pubkey => TofuVerdict::Matched,
            Some(existing) => TofuVerdict::Mismatch {
                expected: *existing,
                got: *pubkey,
            },
        };
        Ok(verdict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty() {
        let kh = KnownHosts::parse("").unwrap();
        assert!(kh.is_empty());
    }

    #[test]
    fn parse_comments_and_blanks() {
        let kh = KnownHosts::parse("# comment\n\n# another\n\t # indented comment\n").unwrap();
        assert!(kh.is_empty());
    }

    #[test]
    fn parse_valid_entries() {
        let content = "\
# header
192.168.1.5:9000 pBfwMy6qXBDbEyY0nwzoDyFOtJHbWtTNqZxdUjQD9C0
127.0.0.1:9000  AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
";
        let kh = KnownHosts::parse(content).unwrap();
        assert_eq!(kh.len(), 2);
        assert!(kh.get("192.168.1.5:9000").is_some());
        assert!(kh.get("127.0.0.1:9000").is_some());
        assert!(kh.get("unknown-host:9000").is_none());
    }

    #[test]
    fn parse_bad_pubkey_fails() {
        let content = "192.168.1.5:9000 not_base64!!!\n";
        let err = KnownHosts::parse(content).unwrap_err();
        match err {
            KnownHostsError::Parse { line: 1, .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_missing_pubkey_fails() {
        let content = "192.168.1.5:9000\n";
        let err = KnownHosts::parse(content).unwrap_err();
        match err {
            KnownHostsError::Parse { line: 1, .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn insert_and_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let mut kh = KnownHosts::new();
        let pk = PubKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        kh.insert("alice-desktop".into(), pk);
        kh.save(&path).unwrap();
        let reloaded = KnownHosts::load(&path).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded.get("alice-desktop").is_some());
    }

    #[test]
    fn verify_or_record_first_seen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let pk = PubKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let verdict = KnownHosts::verify_or_record(&path, "alice-desktop", &pk).unwrap();
        assert!(matches!(verdict, TofuVerdict::FirstSeen));
        let reloaded = KnownHosts::load(&path).unwrap();
        assert!(reloaded.get("alice-desktop").is_some());
    }

    #[test]
    fn concurrent_verify_or_record_keeps_all_entries() {
        // Regression for AC-12: N threads each record a *distinct* host into
        // the same file at once. The advisory lock + atomic write must ensure
        // every entry survives (the old unlocked read-modify-write dropped
        // all but one).
        let dir = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(dir.path().join("hosts"));
        let n = 24;
        let mut handles = vec![];
        for i in 0..n {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                let mut pk = [0u8; 32];
                pk[0] = i as u8;
                pk[1] = (i >> 8) as u8;
                let verdict =
                    KnownHosts::verify_or_record(&*path, &format!("host-{i:03}"), &PubKey(pk))
                        .unwrap();
                assert!(matches!(verdict, TofuVerdict::FirstSeen));
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let reloaded = KnownHosts::load(&*path).unwrap();
        assert_eq!(reloaded.len(), n, "concurrent writers lost entries");
        for i in 0..n {
            assert!(
                reloaded.get(&format!("host-{i:03}")).is_some(),
                "missing host-{i:03}"
            );
        }
    }

    #[test]
    fn verify_or_record_matched_and_mismatched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let pk1 = PubKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let pk2 = PubKey([1u8; 32]);
        let _ = KnownHosts::verify_or_record(&path, "alice-desktop", &pk1).unwrap();
        let v = KnownHosts::verify_or_record(&path, "alice-desktop", &pk1).unwrap();
        assert!(matches!(v, TofuVerdict::Matched));
        let v = KnownHosts::verify_or_record(&path, "alice-desktop", &pk2).unwrap();
        assert!(matches!(v, TofuVerdict::Mismatch { .. }));
    }
}
