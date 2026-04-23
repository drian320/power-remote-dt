use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use prdt_protocol::{control::ControlMessage, input::InputEvent, wire::AudioPacket, EncodedFrame};
use tokio::sync::mpsc;

use crate::error::TransportError;
use crate::transport_trait::{ReceivedMessage, Transport};

/// Options for simulating network degradation during tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopbackOptions {
    /// Per-message drop probability in ppm (0..=1_000_000).
    pub drop_ppm: u32,
    /// Fixed latency to add to every delivered message.
    pub latency: Option<Duration>,
}

/// An in-process transport that delivers messages directly via channels.
/// Used for unit/integration tests and for Phase 0 latency-bench M2.
pub struct InProcTransport {
    send_tx: mpsc::UnboundedSender<ReceivedMessage>,
    recv_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ReceivedMessage>>>,
    opts: LoopbackOptions,
}

impl InProcTransport {
    /// Create a connected pair (like `tokio::sync::mpsc::channel` but
    /// bidirectional and typed for our messages). Both ends can send and
    /// receive.
    pub fn pair(opts: LoopbackOptions) -> (Self, Self) {
        let (a_to_b_tx, a_to_b_rx) = mpsc::unbounded_channel();
        let (b_to_a_tx, b_to_a_rx) = mpsc::unbounded_channel();
        let side_a = InProcTransport {
            send_tx: a_to_b_tx,
            recv_rx: Arc::new(tokio::sync::Mutex::new(b_to_a_rx)),
            opts,
        };
        let side_b = InProcTransport {
            send_tx: b_to_a_tx,
            recv_rx: Arc::new(tokio::sync::Mutex::new(a_to_b_rx)),
            opts,
        };
        (side_a, side_b)
    }

    fn should_drop(&self) -> bool {
        if self.opts.drop_ppm == 0 {
            return false;
        }
        // xorshift64-ish per-call (cheap, good-enough for tests)
        use std::sync::atomic::{AtomicU64, Ordering};
        static STATE: AtomicU64 = AtomicU64::new(0x2545F4914F6CDD1D);
        let mut x = STATE.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        STATE.store(x, Ordering::Relaxed);
        let r = (x % 1_000_000) as u32;
        r < self.opts.drop_ppm
    }

    async fn send_msg(&self, msg: ReceivedMessage) -> Result<(), TransportError> {
        if self.should_drop() {
            return Ok(()); // silently drop, simulating UDP loss
        }
        if let Some(d) = self.opts.latency {
            tokio::time::sleep(d).await;
        }
        self.send_tx
            .send(msg)
            .map_err(|_| TransportError::PeerClosed)?;
        Ok(())
    }
}

#[async_trait]
impl Transport for InProcTransport {
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError> {
        self.send_msg(ReceivedMessage::Video(frame)).await
    }

    async fn send_input(&self, ev: InputEvent) -> Result<(), TransportError> {
        self.send_msg(ReceivedMessage::Input(ev)).await
    }

    async fn send_control(&self, msg: ControlMessage) -> Result<(), TransportError> {
        self.send_msg(ReceivedMessage::Control(msg)).await
    }

    async fn send_audio(&self, pkt: AudioPacket) -> Result<(), TransportError> {
        self.send_msg(ReceivedMessage::Audio(pkt)).await
    }

    async fn recv(&self) -> Result<ReceivedMessage, TransportError> {
        let mut rx = self.recv_rx.lock().await;
        rx.recv().await.ok_or(TransportError::PeerClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use prdt_protocol::frame::Codec;

    fn frame(seq: u64) -> EncodedFrame {
        EncodedFrame {
            seq,
            timestamp_host_us: seq,
            is_keyframe: seq == 0,
            nal_units: Bytes::from_static(&[0xAA; 10]),
            width: 1920,
            height: 1080,
            codec: Codec::H265,
        }
    }

    #[tokio::test]
    async fn loopback_basic_round_trip() {
        let (a, b) = InProcTransport::pair(LoopbackOptions::default());
        a.send_video(frame(1)).await.unwrap();
        let msg = b.recv().await.unwrap();
        match msg {
            ReceivedMessage::Video(f) => assert_eq!(f.seq, 1),
            _ => panic!("expected Video"),
        }
    }

    #[tokio::test]
    async fn loopback_input_and_control() {
        let (a, b) = InProcTransport::pair(LoopbackOptions::default());
        a.send_input(InputEvent::Key {
            scancode: 0x1E,
            pressed: true,
        })
        .await
        .unwrap();
        a.send_control(ControlMessage::RequestIdr).await.unwrap();
        let m1 = b.recv().await.unwrap();
        assert!(matches!(m1, ReceivedMessage::Input(_)));
        let m2 = b.recv().await.unwrap();
        assert!(matches!(
            m2,
            ReceivedMessage::Control(ControlMessage::RequestIdr)
        ));
    }

    #[tokio::test]
    async fn loopback_audio_roundtrip() {
        let (a, b) = InProcTransport::pair(LoopbackOptions::default());
        let pkt = AudioPacket {
            seq: 1,
            timestamp_us: 100,
            opus_bytes: vec![0xAA; 200],
        };
        a.send_audio(pkt.clone()).await.unwrap();
        let m = b.recv().await.unwrap();
        match m {
            ReceivedMessage::Audio(got) => {
                assert_eq!(got.seq, 1);
                assert_eq!(got.timestamp_us, 100);
                assert_eq!(got.opus_bytes.len(), 200);
                assert!(got.opus_bytes.iter().all(|&b| b == 0xAA));
            }
            other => panic!("unexpected {:?}", other),
        }
    }
}
