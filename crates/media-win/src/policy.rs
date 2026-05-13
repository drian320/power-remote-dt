//! P5A capability/factory integration for Windows.
//!
//! **Probe**: enumerates three backends — NVENC (priority 100), MF HEVC
//! (priority 80), and OpenH264 SW (priority 10).
//!
//! **Factory**: returns `FactoryError::Unavailable` for all three backends.
//! Each arm emits a distinct log level:
//!
//! * NVENC / MF HEVC — `warn!` (HW paths, expected on most machines)
//! * OpenH264 — `info!` (SW fallback, clearer deferral note)
//!
//! # Why all three backends are still stubbed
//!
//! **All three** Windows producers require a live `D3d11Device` and an
//! `OutputInfo` (DXGI output descriptor) — including `DxgiSwProducer`
//! (OpenH264), which uses DXGI Desktop Duplication for screen capture even
//! though it encodes in software.  These are runtime artefacts that live
//! inside `crates/host/src/platform/win.rs`; they are not part of
//! `ProducerConfig` and cannot be threaded through without significant
//! pipeline refactoring (P5C scope).
//!
//! `PolicyDriven::bootstrap` calls `WindowsFactory::create` and, when it
//! returns `Unavailable`, falls back through its ranked candidate list.
//! The host's `build_video_producer` (unchanged legacy path) is what actually
//! constructs producers in P5A. The policy layer is wired in for **probe +
//! ranking + CLI flag plumbing only**; factory construction is deferred to
//! P5C when `ProducerConfig` will be extended with
//! `Option<D3D11SetupContext>` (device + output).
//!
//! # P5C TODO
//!
//! Extend `ProducerConfig` with `Option<D3D11SetupContext>` and wire
//! `DxgiSwProducer::with_encoder` (OpenH264) here first, then NVENC/MfHevc.
//!
//! See the T7 / T8 status docs for the full deferral rationale.

#![cfg(windows)]

use prdt_media_policy::{
    BackendKind, CapabilityProbe, Codec, EncoderCapability, FactoryError, ProducerConfig,
    ProducerFactory,
};
use prdt_protocol::VideoProducer;

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

pub struct WindowsProbe;

impl CapabilityProbe for WindowsProbe {
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
            EncoderCapability {
                backend: BackendKind::Openh264,
                codec: Codec::H264,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: false,
                priority: 10,
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Factory (P5A stub — see module doc)
// ---------------------------------------------------------------------------

pub struct WindowsFactory;

impl ProducerFactory for WindowsFactory {
    fn create(
        &self,
        kind: BackendKind,
        _cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError> {
        // P5A: ALL three Windows producers (NVENC, MF HEVC, and OpenH264) require
        // a live D3d11Device + OutputInfo (DXGI Desktop Duplication) which are not
        // part of ProducerConfig. Even DxgiSwProducer (OpenH264) uses DXGI capture
        // and therefore cannot be wired without platform handles.
        //
        // P5C TODO: extend ProducerConfig with Option<D3D11SetupContext> and wire
        // DxgiSwProducer::with_encoder (OpenH264) here first, then NVENC/MfHevc.
        match kind {
            BackendKind::Nvenc | BackendKind::MfHevc => {
                tracing::warn!(
                    backend = ?kind,
                    "WindowsFactory: HW backend requires D3d11Device + OutputInfo \
                     not in ProducerConfig; deferred to P5C"
                );
            }
            BackendKind::Openh264 => {
                tracing::info!(
                    backend = ?kind,
                    "WindowsFactory: Openh264 (DxgiSwProducer) also requires \
                     D3d11Device + OutputInfo for DXGI capture; deferred to P5C \
                     when ProducerConfig gains Option<D3D11SetupContext>"
                );
            }
            BackendKind::Vaapi => {
                tracing::warn!(
                    backend = ?kind,
                    "WindowsFactory: Vaapi is Linux-only; should never be \
                     requested on Windows (policy ranking is per-platform). \
                     Returning Unavailable."
                );
            }
        }
        Err(FactoryError::Unavailable(
            kind,
            "Windows factory wiring deferred to P5C (D3d11Device + OutputInfo \
             not yet threaded through ProducerConfig); host uses legacy construction path"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_probe_lists_three_backends() {
        let probe = WindowsProbe;
        let caps = probe.list_encoders();
        assert_eq!(caps.len(), 3);
        assert_eq!(caps[0].backend, BackendKind::Nvenc);
        assert_eq!(caps[1].backend, BackendKind::MfHevc);
        assert_eq!(caps[2].backend, BackendKind::Openh264);
    }

    #[test]
    fn windows_probe_nvenc_is_highest_priority() {
        let probe = WindowsProbe;
        let caps = probe.list_encoders();
        let nvenc = caps
            .iter()
            .find(|c| c.backend == BackendKind::Nvenc)
            .unwrap();
        let mf = caps
            .iter()
            .find(|c| c.backend == BackendKind::MfHevc)
            .unwrap();
        let sw = caps
            .iter()
            .find(|c| c.backend == BackendKind::Openh264)
            .unwrap();
        assert!(nvenc.priority > mf.priority);
        assert!(mf.priority > sw.priority);
    }

    #[test]
    fn windows_probe_hw_codecs_are_h265() {
        let probe = WindowsProbe;
        let caps = probe.list_encoders();
        for cap in &caps {
            match cap.backend {
                BackendKind::Nvenc | BackendKind::MfHevc => {
                    assert_eq!(cap.codec, Codec::H265, "{:?} should be H265", cap.backend);
                    assert!(cap.zero_copy, "{:?} should be zero_copy", cap.backend);
                }
                BackendKind::Openh264 => {
                    assert_eq!(cap.codec, Codec::H264);
                    assert!(!cap.zero_copy);
                }
                BackendKind::Vaapi => {
                    panic!("WindowsProbe must not emit Vaapi (Linux-only backend)");
                }
            }
        }
    }

    #[test]
    fn windows_factory_returns_unavailable_for_all_backends() {
        let factory = WindowsFactory;
        let cfg = ProducerConfig {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 8_000_000,
            codec: Codec::H265,
        };
        for kind in [
            BackendKind::Nvenc,
            BackendKind::MfHevc,
            BackendKind::Openh264,
        ] {
            let result = factory.create(kind, &cfg);
            assert!(
                matches!(result, Err(FactoryError::Unavailable(_, _))),
                "expected Unavailable for {kind:?}"
            );
        }
    }
}
