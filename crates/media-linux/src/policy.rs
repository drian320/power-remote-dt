//! P5A capability/factory integration + P5B-1 capture-backend probe.
//!
//! `LinuxSwProbe` (P5A) reports the encoder side (Openh264 only on Linux).
//! `CaptureBackend` (P5B-1) selects the *capture* side: X11 MIT-SHM or the
//! xdg-desktop-portal ScreenCast path. The two axes don't interact today —
//! Linux ships Openh264 regardless of capture choice — so the policy stays
//! single-axis (P5C may revisit when VAAPI/NVENC-Linux land).

#![cfg(target_os = "linux")]

use prdt_media_policy::{
    BackendKind, CapabilityProbe, Codec, EncoderCapability, FactoryError, ProducerConfig,
    ProducerFactory,
};
use prdt_protocol::VideoProducer;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Encoder-side probe (unchanged from P5A)
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
// Capture-side backend
// ---------------------------------------------------------------------------

/// Concrete capture-side choice as resolved by `detect_capture_backend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackend {
    X11Shm,
    WaylandPortal,
}

/// CLI-level choice. `Auto` is the default and runs the 3-step probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackendChoice {
    Auto,
    X11,
    Wayland,
}

impl CaptureBackendChoice {
    /// Parse the `--capture-backend <auto|x11|wayland>` CLI value. Returns
    /// `Auto` for unknown strings after logging a warn — matches the
    /// `--encoder` parser's tolerance.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "x11" => Self::X11,
            "wayland" => Self::Wayland,
            other => {
                tracing::warn!(
                    capture_backend = %other,
                    "unknown --capture-backend value; treating as auto"
                );
                Self::Auto
            }
        }
    }
}

/// Resolve the capture-side backend choice.
///
/// 1. Honour an explicit CLI override (`X11` / `Wayland`).
/// 2. Otherwise check `WAYLAND_DISPLAY`: if unset, pick X11 (this covers WSLg
///    and pure X11 sessions cheaply, with no D-Bus traffic).
/// 3. Otherwise call `portal_runtime_available_blocking` (D-Bus `NameHasOwner`
///    against `org.freedesktop.portal.Desktop`, 1s timeout). If the call
///    fails or the portal isn't there, log a warn and fall back to X11.
///
/// The probe never calls `CreateSession` — that would fire the consent
/// dialog every time we probe. The dialog only fires inside
/// `WaylandPortalCapturer::new` when we actually intend to capture.
pub fn detect_capture_backend(choice: CaptureBackendChoice) -> CaptureBackend {
    match choice {
        CaptureBackendChoice::X11 => return CaptureBackend::X11Shm,
        CaptureBackendChoice::Wayland => return CaptureBackend::WaylandPortal,
        CaptureBackendChoice::Auto => {}
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        tracing::info!("WAYLAND_DISPLAY unset; selecting X11 capture backend");
        return CaptureBackend::X11Shm;
    }
    match portal_runtime_available_blocking(Duration::from_secs(1)) {
        Ok(true) => {
            tracing::info!("xdg-desktop-portal reachable; selecting Wayland capture backend");
            CaptureBackend::WaylandPortal
        }
        Ok(false) => {
            tracing::warn!(
                "WAYLAND_DISPLAY set but xdg-desktop-portal unreachable; falling back to X11"
            );
            CaptureBackend::X11Shm
        }
        Err(e) => {
            tracing::warn!(error = %e, "portal probe failed; falling back to X11");
            CaptureBackend::X11Shm
        }
    }
}

/// Synchronous D-Bus probe. Spins up a tiny `current_thread` tokio runtime
/// (so we don't depend on being called from one), opens the session bus,
/// asks `NameHasOwner("org.freedesktop.portal.Desktop")`, and tears down.
///
/// Wall-clock timeout = `timeout`. On timeout returns `Ok(false)` (treat as
/// "portal not available") rather than `Err`, so a slow login doesn't kill
/// startup; if the timeout proves too tight in smoke (spec §11), bump to 3s
/// as a follow-up commit — do not bump pre-emptively.
pub fn portal_runtime_available_blocking(timeout: Duration) -> Result<bool, anyhow::Error> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("portal probe tokio runtime: {e}"))?;
    rt.block_on(async move {
        let fut = async {
            let conn = zbus::Connection::session().await?;
            let proxy = zbus::fdo::DBusProxy::new(&conn).await?;
            let has = proxy
                .name_has_owner(zbus::names::BusName::WellKnown(
                    zbus::names::WellKnownName::try_from("org.freedesktop.portal.Desktop")?,
                ))
                .await?;
            Ok::<bool, anyhow::Error>(has)
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(b)) => Ok(b),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "portal probe NameHasOwner returned err");
                Ok(false)
            }
            Err(_elapsed) => {
                tracing::debug!(?timeout, "portal probe timed out");
                Ok(false)
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Producer factory. `capture_backend` is fixed at construction time: the
/// host resolves it once via `detect_capture_backend(args.into())` before
/// building the factory.
pub struct LinuxSwFactory {
    capture_backend: CaptureBackend,
}

impl LinuxSwFactory {
    pub fn new(capture_backend: CaptureBackend) -> Self {
        Self { capture_backend }
    }

    pub fn capture_backend(&self) -> CaptureBackend {
        self.capture_backend
    }
}

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
        // T7 fills the Wayland arm in; for now route both through the X11
        // helper so the test gate stays green between T2 and T7.
        let producer = match self.capture_backend {
            CaptureBackend::X11Shm => crate::build_video_producer(cfg.initial_bitrate_bps, cfg.fps)
                .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?,
            CaptureBackend::WaylandPortal => {
                crate::build_video_producer(cfg.initial_bitrate_bps, cfg.fps).map_err(|e| {
                    FactoryError::InvalidConfig(
                        kind,
                        format!(
                        "wayland-portal capturer not wired yet (T7); legacy X11 path failed: {e}"
                    ),
                    )
                })?
            }
        };
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
        let factory = LinuxSwFactory::new(CaptureBackend::X11Shm);
        let cfg = ProducerConfig {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 8_000_000,
            codec: Codec::H264,
        };
        let result = factory.create(BackendKind::Nvenc, &cfg);
        assert!(matches!(
            result,
            Err(FactoryError::Unavailable(BackendKind::Nvenc, _))
        ));
    }

    #[test]
    fn linux_factory_rejects_mf_hevc() {
        let factory = LinuxSwFactory::new(CaptureBackend::X11Shm);
        let cfg = ProducerConfig {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 8_000_000,
            codec: Codec::H264,
        };
        let result = factory.create(BackendKind::MfHevc, &cfg);
        assert!(matches!(
            result,
            Err(FactoryError::Unavailable(BackendKind::MfHevc, _))
        ));
    }

    // ----- P5B-1 probe tests -----

    use std::env;

    /// Helper to clear/set env vars for the duration of one test.
    /// Uses `unsafe` because `std::env::set_var` / `remove_var` are not
    /// thread-safe; we rely on `cargo test --lib` running these probe tests
    /// sequentially (they are not marked `#[tokio::test]` so no parallel
    /// async executor is involved).
    struct ScopedEnv {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl ScopedEnv {
        fn unset(key: &'static str) -> Self {
            let prev = env::var_os(key);
            // SAFETY: single-threaded test runner; no concurrent env reads.
            unsafe { env::remove_var(key) };
            Self { key, prev }
        }
        fn set(key: &'static str, val: &str) -> Self {
            let prev = env::var_os(key);
            // SAFETY: single-threaded test runner; no concurrent env reads.
            unsafe { env::set_var(key, val) };
            Self { key, prev }
        }
    }
    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            match &self.prev {
                // SAFETY: single-threaded test runner; no concurrent env reads.
                Some(v) => unsafe { env::set_var(self.key, v) },
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn detect_backend_x11_when_wayland_display_unset() {
        let _guard = ScopedEnv::unset("WAYLAND_DISPLAY");
        let got = detect_capture_backend(CaptureBackendChoice::Auto);
        assert_eq!(got, CaptureBackend::X11Shm);
    }

    #[test]
    fn detect_backend_cli_override_forces_x11_even_with_wayland_display() {
        let _guard = ScopedEnv::set("WAYLAND_DISPLAY", "wayland-fake");
        let got = detect_capture_backend(CaptureBackendChoice::X11);
        assert_eq!(got, CaptureBackend::X11Shm);
    }

    #[test]
    fn detect_backend_cli_override_forces_wayland_even_without_display() {
        let _guard = ScopedEnv::unset("WAYLAND_DISPLAY");
        let got = detect_capture_backend(CaptureBackendChoice::Wayland);
        assert_eq!(got, CaptureBackend::WaylandPortal);
    }

    #[test]
    fn detect_backend_auto_falls_back_to_x11_when_portal_unreachable() {
        // Simulate "WAYLAND_DISPLAY set but no session bus" by pointing
        // DBUS_SESSION_BUS_ADDRESS at a path that can't be opened. The probe
        // should warn + return X11Shm, not panic, not hang.
        let _g1 = ScopedEnv::set("WAYLAND_DISPLAY", "wayland-fake");
        let _g2 = ScopedEnv::set(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/prdt-test",
        );
        let got = detect_capture_backend(CaptureBackendChoice::Auto);
        assert_eq!(got, CaptureBackend::X11Shm);
    }
}
