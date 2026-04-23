//! Shared chunked file-transfer helpers for host and viewer.
//!
//! The protocol is the existing `ControlMessage::FileTransferBegin` /
//! `FileChunk` / `FileTransferEnd` trio. Both directions use the same wire
//! format; who-sends-to-whom is just a question of which binary owns which
//! helper. This crate lets both host and viewer send and receive without
//! duplicating the state machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use prdt_protocol::ControlMessage;
use prdt_transport::Transport;
use tokio::io::AsyncReadExt;
use tracing::{info, warn};

/// Default hard cap on a single transfer (64 MiB). Prevents a malicious or
/// buggy peer from asking the receiver to allocate unbounded disk space.
pub const DEFAULT_MAX_TRANSFER_BYTES: u64 = 64 * 1024 * 1024;

/// Chunk size for `FileChunk`. 8 KB keeps us below typical Ethernet MTU
/// even with encryption + framing overhead, so IP fragmentation pressure
/// stays small.
pub const CHUNK_BYTES: usize = 8 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("file too large: {size} > max {max}")]
    TooLarge { size: u64, max: u64 },
    #[error("bad filename")]
    BadFilename,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("transport: {0}")]
    Transport(#[from] prdt_transport::TransportError),
}

/// Send `path` as a chunked file transfer over any `Transport`. Uses a
/// fresh monotonic-us `transfer_id`. Returns after `FileTransferEnd` is
/// sent. Generic over transport so host/viewer binaries use `CustomUdpTransport`
/// while tests use `InProcTransport`.
pub async fn send_file<T: Transport>(
    transport: &T,
    path: &Path,
    max_bytes: u64,
) -> Result<(), SendError> {
    let metadata = tokio::fs::metadata(path).await?;
    let total = metadata.len();
    if total > max_bytes {
        return Err(SendError::TooLarge {
            size: total,
            max: max_bytes,
        });
    }
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(SendError::BadFilename)?
        .to_string();

    let transfer_id = prdt_transport::now_monotonic_us();
    transport
        .send_control(ControlMessage::FileTransferBegin {
            transfer_id,
            filename,
            total_bytes: total,
        })
        .await?;

    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut seq: u32 = 0;
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let bytes = buf[..n].to_vec();
        transport
            .send_control(ControlMessage::FileChunk {
                transfer_id,
                chunk_seq: seq,
                bytes,
            })
            .await?;
        seq += 1;
    }
    transport
        .send_control(ControlMessage::FileTransferEnd {
            transfer_id,
            success: true,
        })
        .await?;
    Ok(())
}

/// Per-transfer receive state kept by `TransferReceiver`.
pub struct InProgressTransfer {
    pub dest: PathBuf,
    pub file: std::fs::File,
    pub total_bytes: u64,
    pub written_bytes: u64,
    pub next_chunk_seq: u32,
}

/// Mirror of the host's file-receive state machine, extracted so the viewer
/// can reuse it for host→viewer transfers. Owns the in-progress transfer map
/// and the destination directory.
pub struct TransferReceiver {
    recv_dir: PathBuf,
    max_bytes: u64,
    transfers: HashMap<u64, InProgressTransfer>,
}

/// Outcome of one control message fed to `TransferReceiver::handle`.
#[derive(Debug)]
pub enum ReceiveOutcome {
    /// Message wasn't a file-transfer message; pass it to the next handler.
    NotForUs,
    /// Transfer progressed (begin/chunk accepted, no end yet).
    Progress,
    /// Transfer ended. `dest` is the final file path; `success` reflects
    /// whether bytes + end-flag agree.
    Completed { dest: PathBuf, success: bool },
    /// Something went wrong but we swallowed it with a log; caller keeps going.
    Dropped,
}

impl TransferReceiver {
    pub fn new(recv_dir: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            recv_dir: recv_dir.into(),
            max_bytes,
            transfers: HashMap::new(),
        }
    }

    /// Feed one control message. Returns `NotForUs` if the variant is
    /// unrelated to file transfer so the caller can dispatch further.
    pub fn handle(&mut self, msg: ControlMessage) -> ReceiveOutcome {
        use std::io::Write;
        match msg {
            ControlMessage::FileTransferBegin {
                transfer_id,
                filename,
                total_bytes,
            } => {
                if total_bytes > self.max_bytes {
                    warn!(%filename, total_bytes, "rejecting oversized transfer");
                    return ReceiveOutcome::Dropped;
                }
                let base = Path::new(&filename)
                    .file_name()
                    .and_then(|os| os.to_str())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty() && s != ".." && !s.contains('\0'));
                let base = match base {
                    Some(b) => b,
                    None => {
                        warn!(%filename, "rejecting unsafe filename");
                        return ReceiveOutcome::Dropped;
                    }
                };
                if let Err(e) = std::fs::create_dir_all(&self.recv_dir) {
                    warn!(?e, path = %self.recv_dir.display(), "create_dir_all failed");
                    return ReceiveOutcome::Dropped;
                }
                let mut dest = self.recv_dir.clone();
                dest.push(&base);
                let dest = unique_path(&dest);
                let file = match std::fs::File::create(&dest) {
                    Ok(f) => f,
                    Err(e) => {
                        warn!(?e, path = %dest.display(), "file create failed");
                        return ReceiveOutcome::Dropped;
                    }
                };
                info!(
                    transfer_id,
                    filename = %base,
                    %total_bytes,
                    path = %dest.display(),
                    "file transfer start",
                );
                self.transfers.insert(
                    transfer_id,
                    InProgressTransfer {
                        dest,
                        file,
                        total_bytes,
                        written_bytes: 0,
                        next_chunk_seq: 0,
                    },
                );
                ReceiveOutcome::Progress
            }
            ControlMessage::FileChunk {
                transfer_id,
                chunk_seq,
                bytes,
            } => {
                let Some(t) = self.transfers.get_mut(&transfer_id) else {
                    warn!(transfer_id, "unknown transfer_id");
                    return ReceiveOutcome::Dropped;
                };
                if chunk_seq != t.next_chunk_seq {
                    warn!(
                        transfer_id,
                        expected = t.next_chunk_seq,
                        got = chunk_seq,
                        "out-of-order chunk",
                    );
                    self.transfers.remove(&transfer_id);
                    return ReceiveOutcome::Dropped;
                }
                if t.written_bytes + bytes.len() as u64 > t.total_bytes {
                    warn!(transfer_id, "chunk overflows declared total_bytes");
                    self.transfers.remove(&transfer_id);
                    return ReceiveOutcome::Dropped;
                }
                if let Err(e) = t.file.write_all(&bytes) {
                    warn!(?e, transfer_id, "write chunk failed");
                    self.transfers.remove(&transfer_id);
                    return ReceiveOutcome::Dropped;
                }
                t.written_bytes += bytes.len() as u64;
                t.next_chunk_seq += 1;
                ReceiveOutcome::Progress
            }
            ControlMessage::FileTransferEnd {
                transfer_id,
                success,
            } => {
                let Some(t) = self.transfers.remove(&transfer_id) else {
                    warn!(transfer_id, "unknown transfer_id for end");
                    return ReceiveOutcome::Dropped;
                };
                let ok = success && t.written_bytes == t.total_bytes;
                if ok {
                    info!(transfer_id, path = %t.dest.display(), "file transfer complete");
                } else {
                    warn!(
                        transfer_id,
                        success,
                        written = t.written_bytes,
                        total = t.total_bytes,
                        "transfer did not complete cleanly; keeping partial file",
                    );
                }
                ReceiveOutcome::Completed {
                    dest: t.dest,
                    success: ok,
                }
            }
            _ => ReceiveOutcome::NotForUs,
        }
    }
}

/// Append "-N" before the extension until the path doesn't exist, so file
/// receivers never silently clobber existing files.
pub fn unique_path(base: &Path) -> PathBuf {
    if !base.exists() {
        return base.to_path_buf();
    }
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("");
    let parent = base.parent().unwrap_or(Path::new("."));
    for i in 1..10_000 {
        let candidate = if ext.is_empty() {
            parent.join(format!("{stem}-{i}"))
        } else {
            parent.join(format!("{stem}-{i}.{ext}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    base.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use prdt_protocol::ControlMessage;

    #[test]
    fn receiver_rejects_oversized_transfer() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = TransferReceiver::new(dir.path(), 1024);
        let msg = ControlMessage::FileTransferBegin {
            transfer_id: 1,
            filename: "x.bin".into(),
            total_bytes: 2048,
        };
        assert!(matches!(rx.handle(msg), ReceiveOutcome::Dropped));
    }

    #[test]
    fn receiver_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = TransferReceiver::new(dir.path(), DEFAULT_MAX_TRANSFER_BYTES);
        for bad in ["..", "", "a\0b"] {
            let msg = ControlMessage::FileTransferBegin {
                transfer_id: 1,
                filename: bad.into(),
                total_bytes: 1,
            };
            assert!(matches!(rx.handle(msg), ReceiveOutcome::Dropped));
        }
    }

    #[test]
    fn receiver_accepts_basename_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = TransferReceiver::new(dir.path(), DEFAULT_MAX_TRANSFER_BYTES);
        // Even with a rooted / traversing path, only the basename is used.
        let msg = ControlMessage::FileTransferBegin {
            transfer_id: 42,
            filename: "../../etc/passwd".into(),
            total_bytes: 3,
        };
        assert!(matches!(rx.handle(msg), ReceiveOutcome::Progress));
        let chunk = ControlMessage::FileChunk {
            transfer_id: 42,
            chunk_seq: 0,
            bytes: b"abc".to_vec(),
        };
        assert!(matches!(rx.handle(chunk), ReceiveOutcome::Progress));
        let end = ControlMessage::FileTransferEnd {
            transfer_id: 42,
            success: true,
        };
        match rx.handle(end) {
            ReceiveOutcome::Completed { dest, success } => {
                assert!(success);
                assert!(
                    dest.starts_with(dir.path()),
                    "dest {} should live under recv dir {}",
                    dest.display(),
                    dir.path().display(),
                );
                assert_eq!(
                    dest.file_name().and_then(|s| s.to_str()),
                    Some("passwd"),
                    "basename extracted from traversing filename",
                );
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn receiver_detects_out_of_order_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = TransferReceiver::new(dir.path(), DEFAULT_MAX_TRANSFER_BYTES);
        let begin = ControlMessage::FileTransferBegin {
            transfer_id: 7,
            filename: "f.txt".into(),
            total_bytes: 10,
        };
        assert!(matches!(rx.handle(begin), ReceiveOutcome::Progress));
        let wrong = ControlMessage::FileChunk {
            transfer_id: 7,
            chunk_seq: 5,
            bytes: vec![0; 5],
        };
        assert!(matches!(rx.handle(wrong), ReceiveOutcome::Dropped));
    }

    #[test]
    fn receiver_passes_through_non_filetransfer_messages() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = TransferReceiver::new(dir.path(), DEFAULT_MAX_TRANSFER_BYTES);
        let out = rx.handle(ControlMessage::RequestIdr);
        assert!(matches!(out, ReceiveOutcome::NotForUs));
    }

    #[test]
    fn unique_path_appends_suffix_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("a.txt");
        std::fs::write(&base, b"x").unwrap();
        let next = unique_path(&base);
        assert_eq!(next.file_name().unwrap().to_str().unwrap(), "a-1.txt");
    }
}
