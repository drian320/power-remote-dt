//! E2E integration tests for bidirectional file transfer over InProcTransport.
//! Verifies that send_file + TransferReceiver composed in either direction
//! produces a byte-identical file on the far side.

use std::time::Duration;

use prdt_filetransfer::{send_file, ReceiveOutcome, TransferReceiver, DEFAULT_MAX_TRANSFER_BYTES};
use prdt_transport::{InProcTransport, LoopbackOptions, ReceivedMessage, Transport};

/// Drives a receiver until `FileTransferEnd` arrives (or the transport
/// closes). Returns the final dest path.
async fn receive_until_done<T: Transport>(
    rx: &T,
    recv_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let mut receiver = TransferReceiver::new(recv_dir, DEFAULT_MAX_TRANSFER_BYTES);
    loop {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(ReceivedMessage::Control(msg))) => match receiver.handle(msg) {
                ReceiveOutcome::Completed { dest, success } if success => return Some(dest),
                ReceiveOutcome::Completed { .. } | ReceiveOutcome::Dropped => return None,
                ReceiveOutcome::NotForUs | ReceiveOutcome::Progress => continue,
            },
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => return None,
        }
    }
}

fn make_temp_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write source");
    p
}

#[tokio::test]
async fn viewer_to_host_transfer_is_byte_identical() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..50_000u32).map(|i| (i & 0xFF) as u8).collect();
    let src = make_temp_file(src_dir.path(), "v2h.bin", &payload);

    let (viewer_side, host_side) = InProcTransport::pair(LoopbackOptions::default());

    let dst_dir_path = dst_dir.path().to_path_buf();
    let recv_task =
        tokio::spawn(async move { receive_until_done(&host_side, &dst_dir_path).await });

    send_file(&viewer_side, &src, DEFAULT_MAX_TRANSFER_BYTES)
        .await
        .expect("send_file viewer→host");

    let dest = recv_task
        .await
        .unwrap()
        .expect("receiver should produce a dest path");
    let back = std::fs::read(&dest).unwrap();
    assert_eq!(back, payload);
    assert_eq!(dest.file_name().unwrap().to_str().unwrap(), "v2h.bin");
}

#[tokio::test]
async fn host_to_viewer_transfer_is_byte_identical() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..75_000u32).map(|i| ((i * 31) & 0xFF) as u8).collect();
    let src = make_temp_file(src_dir.path(), "h2v.bin", &payload);

    let (viewer_side, host_side) = InProcTransport::pair(LoopbackOptions::default());

    let dst_dir_path = dst_dir.path().to_path_buf();
    let recv_task =
        tokio::spawn(async move { receive_until_done(&viewer_side, &dst_dir_path).await });

    send_file(&host_side, &src, DEFAULT_MAX_TRANSFER_BYTES)
        .await
        .expect("send_file host→viewer");

    let dest = recv_task
        .await
        .unwrap()
        .expect("receiver should produce a dest path");
    let back = std::fs::read(&dest).unwrap();
    assert_eq!(back, payload);
    assert_eq!(dest.file_name().unwrap().to_str().unwrap(), "h2v.bin");
}

#[tokio::test]
async fn empty_file_round_trips() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    let src = make_temp_file(src_dir.path(), "empty.bin", &[]);

    let (a, b) = InProcTransport::pair(LoopbackOptions::default());
    let dst_path = dst_dir.path().to_path_buf();
    let recv_task = tokio::spawn(async move { receive_until_done(&b, &dst_path).await });

    send_file(&a, &src, DEFAULT_MAX_TRANSFER_BYTES)
        .await
        .unwrap();

    let dest = recv_task.await.unwrap().expect("should complete");
    assert_eq!(std::fs::read(&dest).unwrap(), Vec::<u8>::new());
}

#[tokio::test]
async fn oversized_source_is_rejected_client_side() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    let payload = vec![0xAAu8; 4096];
    let src = make_temp_file(src_dir.path(), "too_big.bin", &payload);

    let (a, _b) = InProcTransport::pair(LoopbackOptions::default());

    // Cap smaller than the source → send_file must reject pre-send.
    let err = send_file(&a, &src, 1024).await.unwrap_err();
    assert!(
        matches!(err, prdt_filetransfer::SendError::TooLarge { .. }),
        "unexpected err: {err:?}",
    );
    // Dst was never touched.
    assert!(std::fs::read_dir(dst_dir.path()).unwrap().next().is_none());
}
