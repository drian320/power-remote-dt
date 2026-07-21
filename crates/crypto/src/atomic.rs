//! Crash-safe file persistence primitives shared by the identity, known_hosts
//! and key-file writers.
//!
//! Two building blocks:
//!
//! * [`atomic_write`] — write bytes to a temp file, `fsync` it, then `rename`
//!   over the destination. A crash or power loss can leave the temp file
//!   behind but never a half-written destination. The parent directory is
//!   `fsync`'d after the rename so the new dirent is durable too.
//! * [`FileLock`] — a cross-platform *advisory* exclusive lock (via `fs2`)
//!   taken on a sidecar `<path>.lock` file. Held across a read-modify-write so
//!   two processes cannot interleave and lose each other's entries. The lock
//!   lives on a sidecar rather than the data file itself so that
//!   [`atomic_write`]'s `rename` (which swaps the data file's inode) does not
//!   invalidate a held lock.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;

/// Monotonic per-process counter so two temp files created in the same
/// millisecond by the same process still get distinct names.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_tmp_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("prdt-tmp");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".{stem}.tmp.{}.{nanos}.{n}", std::process::id()))
}

/// Atomically replace `path`'s contents with `bytes`.
///
/// Creates the parent directory on demand. Durability order: write temp →
/// `fsync` temp → `rename` over dest → best-effort `fsync` of the parent dir.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let tmp = unique_tmp_path(path);
    // Scope the file handle so it is closed before the rename (required on
    // Windows, where an open handle blocks ReplaceFile/MoveFileEx).
    let write_result = (|| -> io::Result<()> {
        // Create the temp with owner-only perms from the start (Unix): callers
        // include secret material (private keys, the plaintext PIN), and a
        // `File::create` would briefly expose the temp at the default umask
        // (0644) before any later chmod — a TOCTOU window on shared machines.
        // Windows keeps its default create behavior (inherits the profile ACL).
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // Best-effort directory fsync so the rename survives a crash. Not all
    // platforms allow opening a directory as a File (notably Windows), so a
    // failure here is non-fatal.
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// An exclusive advisory lock guarding read-modify-write sequences on `path`.
///
/// Acquired by [`FileLock::acquire`]; released when the guard is dropped.
/// Cross-process and cross-thread (fs2 maps to `flock`/`LockFileEx`).
#[must_use = "the lock is released as soon as the guard is dropped"]
pub struct FileLock {
    file: File,
}

impl FileLock {
    /// Take the exclusive lock for `path`, blocking until it is available.
    ///
    /// The lock is keyed on a sidecar `<path>.lock` file created on demand.
    pub fn acquire(path: &Path) -> io::Result<FileLock> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let lock_path = lock_path_for(path);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        file.lock_exclusive()?;
        Ok(FileLock { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Explicit unlock; the handle also unlocks on close, but being
        // explicit keeps the intent obvious and releases promptly.
        let _ = FileExt::unlock(&self.file);
    }
}

fn lock_path_for(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("prdt");
    parent.join(format!("{stem}.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn atomic_write_replaces_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        atomic_write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");
        atomic_write(&path, b"second-longer").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second-longer");
    }

    #[test]
    fn atomic_write_leaves_no_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        atomic_write(&path, b"x").unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_result_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        // The temp is created with mode 0600 and rename preserves it, so the
        // destination is owner-only from the first byte on disk (no default-
        // umask 0644 TOCTOU window for secret material).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.bin");
        atomic_write(&path, b"top-secret").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "atomic_write output should be chmod 600"
        );
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deep/data.bin");
        atomic_write(&path, b"hi").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hi");
    }

    #[test]
    fn file_lock_serializes_writers() {
        // Two threads each append their id to a shared counter file under the
        // lock. With the lock held across read-modify-write, no update is lost.
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("counter"));
        atomic_write(&path, b"0").unwrap();

        let mut handles = vec![];
        for _ in 0..8 {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    let _guard = FileLock::acquire(&path).unwrap();
                    let cur: u64 = fs::read_to_string(&*path).unwrap().trim().parse().unwrap();
                    atomic_write(&path, (cur + 1).to_string().as_bytes()).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let final_count: u64 = fs::read_to_string(&*path).unwrap().trim().parse().unwrap();
        assert_eq!(final_count, 8 * 25, "lock lost an update");
    }
}
