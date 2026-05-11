//! Backend capability descriptors and probe trait.
//!
//! `CapabilityProbe` impls live in OS-specific backend crates
//! (`media-win::policy`, `media-linux::policy`, etc.). This file holds only
//! the platform-agnostic types they emit.

use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendKind {
    Nvenc,
    MfHevc,
    Openh264,
    // future: Vaapi, V4L2M2M, VideoToolbox, MediaCodec
}

impl BackendKind {
    /// Stable lowercase identifier for logs / config files. The CLI
    /// short-form for `MfHevc` is `"mf"`; both round-trip via `FromStr`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nvenc => "nvenc",
            Self::MfHevc => "mf-hevc",
            Self::Openh264 => "openh264",
        }
    }
}

/// Parses both the canonical log identifier (e.g. `"mf-hevc"`) and the
/// CLI short form (e.g. `"mf"`). Used by host CLI flags in T7.
impl FromStr for BackendKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nvenc" => Ok(Self::Nvenc),
            "mf" | "mf-hevc" => Ok(Self::MfHevc),
            "openh264" => Ok(Self::Openh264),
            other => Err(format!("unknown BackendKind: {other:?}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    H264,
    H265,
    // future: AV1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderCapability {
    pub backend: BackendKind,
    pub codec: Codec,
    pub max_resolution: (u32, u32), // (width, height)
    pub max_fps: u32,
    pub zero_copy: bool,
    /// OS-fixed default priority. NVENC=100, VAAPI=90, MfHevc=80, Openh264=10.
    pub priority: i32,
}

pub trait CapabilityProbe: Send + Sync {
    fn list_encoders(&self) -> Vec<EncoderCapability>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic in-memory probe used in this file's unit tests. Integration tests in `tests/` define their own probe types inline (the cfg(test) module is not reachable across crate boundaries).
    pub struct MockProbe(pub Vec<EncoderCapability>);

    impl CapabilityProbe for MockProbe {
        fn list_encoders(&self) -> Vec<EncoderCapability> {
            self.0.clone()
        }
    }

    #[test]
    fn backend_kind_as_str_is_stable() {
        assert_eq!(BackendKind::Nvenc.as_str(), "nvenc");
        assert_eq!(BackendKind::MfHevc.as_str(), "mf-hevc");
        assert_eq!(BackendKind::Openh264.as_str(), "openh264");
    }

    #[test]
    fn backend_kind_round_trips_via_from_str() {
        // canonical (log) form round-trips
        for k in [BackendKind::Nvenc, BackendKind::MfHevc, BackendKind::Openh264] {
            let s = k.as_str();
            assert_eq!(s.parse::<BackendKind>().unwrap(), k, "round-trip failed for {k:?}");
        }
        // CLI short form for MfHevc also parses
        assert_eq!("mf".parse::<BackendKind>().unwrap(), BackendKind::MfHevc);
        // unknown identifier rejected
        assert!("xyz".parse::<BackendKind>().is_err());
    }

    #[test]
    fn mock_probe_returns_fixture() {
        let probe = MockProbe(vec![
            EncoderCapability {
                backend: BackendKind::Nvenc,
                codec: Codec::H265,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: true,
                priority: 100,
            },
            EncoderCapability {
                backend: BackendKind::Openh264,
                codec: Codec::H264,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: false,
                priority: 10,
            },
        ]);

        let caps = probe.list_encoders();
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].backend, BackendKind::Nvenc);
        assert_eq!(caps[1].backend, BackendKind::Openh264);
    }

    #[test]
    fn capability_round_trips_via_serde_json() {
        let cap = EncoderCapability {
            backend: BackendKind::MfHevc,
            codec: Codec::H265,
            max_resolution: (1920, 1080),
            max_fps: 60,
            zero_copy: true,
            priority: 80,
        };
        let json = serde_json::to_string(&cap).unwrap();
        let back: EncoderCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(back.backend, BackendKind::MfHevc);
        assert_eq!(back.max_resolution, (1920, 1080));
    }
}
