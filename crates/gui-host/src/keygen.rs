//! Host key generation flow for the GUI's first-run experience.

use std::path::Path;

use prdt_crypto::KeyPair;

/// Result of `try_load_or_generate`: either the existing key was loaded
/// or a fresh one was generated and persisted.
///
/// `keypair` and `generated` are exposed so future GUI flows can show
/// "newly generated, copy this!" prompts and pass the keypair to a Noise
/// handshake without re-reading the file. They're not consumed in G1
/// (the host server loads its own copy from `key_file`); the
/// `#[allow(dead_code)]` suppresses the warning until G2+ uses them.
#[allow(dead_code)]
pub struct KeyOutcome {
    pub keypair: KeyPair,
    pub pubkey_b64: String,
    pub generated: bool,
}

/// Try to load `path`; if missing, generate a new keypair and write it.
/// Returns the keypair plus a base64-encoded pubkey for display.
pub fn try_load_or_generate(path: &Path) -> anyhow::Result<KeyOutcome> {
    if path.exists() {
        let bytes = std::fs::read(path)?;
        if bytes.len() != 32 {
            anyhow::bail!("key file {} is not 32 bytes", path.display());
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        let kp = KeyPair::from_private(arr);
        let pubkey_b64 = kp.public.to_base64();
        return Ok(KeyOutcome {
            keypair: kp,
            pubkey_b64,
            generated: false,
        });
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let kp = KeyPair::generate();
    std::fs::write(path, kp.private.0)?;
    let pubkey_b64 = kp.public.to_base64();
    Ok(KeyOutcome {
        keypair: kp,
        pubkey_b64,
        generated: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-key.bin");
        let out = try_load_or_generate(&path).unwrap();
        assert!(out.generated);
        assert!(path.exists());
        assert!(!out.pubkey_b64.is_empty());
    }

    #[test]
    fn loads_existing_without_regenerating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-key.bin");
        let first = try_load_or_generate(&path).unwrap();
        let second = try_load_or_generate(&path).unwrap();
        assert!(!second.generated);
        assert_eq!(first.pubkey_b64, second.pubkey_b64);
    }
}
