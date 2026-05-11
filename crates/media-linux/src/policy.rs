//! P5A capability/factory integration for Linux.
//!
//! Only the OpenH264 SW path is available today. VAAPI / V4L2 / NVENC-Linux
//! are P5C scope.
//!
//! # Factory design note
//!
//! `LinuxSwFactory::create` wraps `prdt_media_linux::build_video_producer`
//! which internally creates an `X11ShmCapturer` (reads geometry from the X
//! server) and a `LinuxSwEncoder`. Width/height therefore come from the live X
//! session rather than from `ProducerConfig`; the config values are used only
//! for `bitrate_bps` and `fps`. This is a Linux-specific constraint: X11 root
//! geometry is implicit, not passed as a constructor argument.

#![cfg(target_os = "linux")]

use prdt_media_policy::{
    BackendKind, CapabilityProbe, Codec, EncoderCapability, FactoryError, ProducerConfig,
    ProducerFactory,
};
use prdt_protocol::VideoProducer;

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

pub struct LinuxSwProbe;

impl CapabilityProbe for LinuxSwProbe {
    fn list_encoders(&self) -> Vec<EncoderCapability> {
        vec![EncoderCapability {
            backend: BackendKind::Openh264,
            codec: Codec::H264,
            max_resolution: (3840, 2160),
            max_fps: 60,
            zero_copy: false,
            priority: 10,
        }]
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct LinuxSwFactory;

impl ProducerFactory for LinuxSwFactory {
    fn create(
        &self,
        kind: BackendKind,
        cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError> {
        if !matches!(kind, BackendKind::Openh264) {
            return Err(FactoryError::Unavailable(
                kind,
                "Linux P5A only supports Openh264; VAAPI/V4L2/NVENC-Linux deferred to P5C".into(),
            ));
        }

        // `build_video_producer` derives width/height from the live X11 root
        // window via X11ShmCapturer. The ProducerConfig w/h values are advisory
        // (used for validation / policy decisions) but not passed to the
        // constructor here — they are implicit from the X server.
        let producer = crate::build_video_producer(cfg.initial_bitrate_bps, cfg.fps)
            .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?;

        Ok(Box::new(producer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_probe_lists_openh264_only() {
        let probe = LinuxSwProbe;
        let caps = probe.list_encoders();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].backend, BackendKind::Openh264);
        assert_eq!(caps[0].codec, Codec::H264);
        assert!(!caps[0].zero_copy);
    }

    #[test]
    fn linux_factory_rejects_nvenc() {
        let factory = LinuxSwFactory;
        let cfg = ProducerConfig {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 8_000_000,
            codec: Codec::H264,
        };
        let result = factory.create(BackendKind::Nvenc, &cfg);
        assert!(result.is_err());
        match result {
            Err(FactoryError::Unavailable(BackendKind::Nvenc, _)) => {}
            Err(other) => panic!("expected Unavailable(Nvenc), got Err({other})"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }

    #[test]
    fn linux_factory_rejects_mf_hevc() {
        let factory = LinuxSwFactory;
        let cfg = ProducerConfig {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 8_000_000,
            codec: Codec::H264,
        };
        let result = factory.create(BackendKind::MfHevc, &cfg);
        assert!(result.is_err());
        match result {
            Err(FactoryError::Unavailable(BackendKind::MfHevc, _)) => {}
            Err(other) => panic!("expected Unavailable(MfHevc), got Err({other})"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }
}
