//! Integration test: scripted MockProducer A/B verifies that
//! DeviceLost on backend A causes PolicyDriven to swap to backend B
//! and the next next_frame() call succeeds via B.

use async_trait::async_trait;
use bytes::Bytes;
use prdt_media_policy::{
    BackendKind, CapabilityProbe, Codec, EncoderCapability, FactoryError, PolicyContext,
    PolicyDriven, ProducerConfig, ProducerFactory, ScoringPolicy, ScoringWeights,
};
use prdt_protocol::{EncodedFrame, ProducerError, VideoProducer};
// prdt_protocol::Codec is a separate type from prdt_media_policy::Codec;
// use an alias to disambiguate when constructing EncodedFrame.
use prdt_protocol::Codec as ProtoCodec;
use std::sync::{Arc, Mutex};

/// A scripted producer: each next_frame() call pops one entry from the
/// scripted result list. Empty result list returns Other("script exhausted").
struct ScriptedProducer {
    name: &'static str,
    script: Mutex<Vec<Result<(), ProducerError>>>, // () = success placeholder
    backend_name: &'static str,
}

impl ScriptedProducer {
    fn new(
        name: &'static str,
        backend_name: &'static str,
        script: Vec<Result<(), ProducerError>>,
    ) -> Self {
        Self {
            name,
            script: Mutex::new(script),
            backend_name,
        }
    }
}

#[async_trait]
impl VideoProducer for ScriptedProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        let next = self.script.lock().unwrap().drain(..1).next();
        match next {
            None => Err(ProducerError::Other(format!(
                "script exhausted for {}",
                self.name
            ))),
            Some(Err(e)) => Err(e),
            Some(Ok(())) => Ok(EncodedFrame {
                seq: 1,
                timestamp_host_us: 0,
                is_keyframe: true,
                nal_units: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65]), // dummy IDR start code
                width: 1920,
                height: 1080,
                codec: ProtoCodec::H265,
            }),
        }
    }
    fn request_idr(&mut self) {}
    fn set_target_bitrate(&mut self, _bps: u32) {}
    fn backend_name(&self) -> &'static str {
        self.backend_name
    }
}

struct TwoBackendProbe;
impl CapabilityProbe for TwoBackendProbe {
    fn list_encoders(&self) -> Vec<EncoderCapability> {
        vec![
            EncoderCapability {
                backend: BackendKind::Nvenc,
                codec: Codec::H265,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: true,
                priority: 100,
            },
            EncoderCapability {
                backend: BackendKind::MfHevc,
                codec: Codec::H265,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: true,
                priority: 80,
            },
        ]
    }
}

/// Each call to create() pops one ScriptedProducer from the per-kind queue.
struct QueuedFactory {
    nvenc_queue: Mutex<Vec<ScriptedProducer>>,
    mf_queue: Mutex<Vec<ScriptedProducer>>,
}

impl ProducerFactory for QueuedFactory {
    fn create(
        &self,
        kind: BackendKind,
        _cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError> {
        let queue = match kind {
            BackendKind::Nvenc => &self.nvenc_queue,
            BackendKind::MfHevc => &self.mf_queue,
            _ => return Err(FactoryError::Unavailable(kind, "not in queue".into())),
        };
        let p = queue.lock().unwrap().drain(..1).next();
        p.map(|sp| Box::new(sp) as Box<dyn VideoProducer>)
            .ok_or_else(|| FactoryError::Unavailable(kind, "queue empty".into()))
    }
}

#[tokio::test]
async fn device_lost_on_nvenc_swaps_to_mf_and_recovers() {
    // NVENC scripted to fail with DeviceLost on the first call.
    // MF scripted to succeed on its first call.
    let nvenc = ScriptedProducer::new(
        "nvenc",
        "nvenc-h265",
        vec![Err(ProducerError::DeviceLost {
            backend: "nvenc-h265".into(),
            reason: "DXGI_ERROR_DEVICE_REMOVED".into(),
        })],
    );
    let mf = ScriptedProducer::new("mf", "mf-h265", vec![Ok(())]);

    let factory = Arc::new(QueuedFactory {
        nvenc_queue: Mutex::new(vec![nvenc]),
        mf_queue: Mutex::new(vec![mf]),
    });
    let probe = Arc::new(TwoBackendProbe);
    let policy = Arc::new(ScoringPolicy::new(ScoringWeights::default()));

    let cfg = ProducerConfig {
        width: 1920,
        height: 1080,
        fps: 60,
        initial_bitrate_bps: 8_000_000,
        codec: Codec::H265,
    };
    let ctx = PolicyContext {
        target_resolution: (1920, 1080),
        target_fps: 60,
        target_bitrate_bps: 8_000_000,
        codec: Codec::H265,
        user_override: None,
        user_hint: None,
        force_sw: false,
    };

    let mut driven = PolicyDriven::bootstrap(probe, factory, policy, cfg, ctx)
        .expect("bootstrap should succeed (NVENC factory call OK)");

    // Sanity: bootstrap chose NVENC (priority 100 wins).
    assert_eq!(driven.backend_name(), "nvenc-h265");

    // First next_frame: NVENC returns DeviceLost; PolicyDriven should swap
    // to MF and retry. Outer call therefore returns Ok via MF.
    let frame = driven
        .next_frame()
        .await
        .expect("after swap, MF should yield a frame");
    assert!(frame.is_keyframe);
    assert_eq!(
        driven.backend_name(),
        "mf-h265",
        "PolicyDriven should now be on MF"
    );
}
