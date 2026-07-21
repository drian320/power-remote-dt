//! Durable load-or-create for the host's long-term private key file.
//!
//! The key file is exactly 32 raw bytes (the Curve25519 static secret). AC-9
//! requires the key to be generated and `fsync`'d to disk *before* any network
//! provisioning happens, so a crash immediately after first launch can never
//! leave the device without a stable identity. Creation is serialized with an
//! advisory lock so two launchers racing on first run agree on a single key
//! instead of clobbering each other.

use std::io;
use std::path::Path;

use crate::atomic::{atomic_write, FileLock};
use crate::keypair::KeyPair;

/// Load the keypair at `path`, or generate + persist a fresh one if the file
/// does not yet exist.
///
/// On creation the private key is written atomically (temp + `fsync` +
/// `rename`) and, on Unix, chmod'd to `0600`. Idempotent: concurrent callers
/// observe the same key because creation happens under [`FileLock`].
pub fn load_or_create_keypair(path: &Path) -> io::Result<KeyPair> {
    if let Some(kp) = try_load(path)? {
        return Ok(kp);
    }

    // Serialize first-run creation so two processes don't each generate a key
    // and race their writes.
    let _lock = FileLock::acquire(path)?;
    // Re-check under the lock: another process may have created it while we
    // were blocked acquiring.
    if let Some(kp) = try_load(path)? {
        return Ok(kp);
    }

    let kp = KeyPair::generate();
    atomic_write(path, &kp.private.0)?;
    set_owner_only_permissions(path)?;
    Ok(kp)
}

/// Load an existing 32-byte key file. Returns `Ok(None)` if the file is
/// absent, `Err` if present but malformed (wrong length).
fn try_load(path: &Path) -> io::Result<Option<KeyPair>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.len() != 32 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("key file must be exactly 32 bytes, got {}", bytes.len()),
                ));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Ok(Some(KeyPair::from_private(arr)))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) -> io::Result<()> {
    // On Windows the key file inherits the user-profile ACL; there is no
    // portable chmod equivalent worth applying here.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_reloads_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-key.bin");
        assert!(!path.exists());
        let kp1 = load_or_create_keypair(&path).unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap().len(), 32);
        // Second call must return the identical key, not generate a new one.
        let kp2 = load_or_create_keypair(&path).unwrap();
        assert_eq!(kp1.public.0, kp2.public.0);
        assert_eq!(kp1.private.0, kp2.private.0);
    }

    #[test]
    fn malformed_key_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-key.bin");
        std::fs::write(&path, b"too short").unwrap();
        assert!(load_or_create_keypair(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn created_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-key.bin");
        load_or_create_keypair(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "key file should be chmod 600");
    }
}
