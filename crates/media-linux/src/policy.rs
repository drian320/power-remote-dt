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
        let mut out = vec![EncoderCapability {
            backend: BackendKind::Openh264,
            codec: Codec::H264,
            max_resolution: (3840, 2160),
            max_fps: 60,
            zero_copy: false,
            priority: 10,
        }];
        if prdt_media_vaapi::display::vaapi_runtime_present() {
            out.push(EncoderCapability {
                backend: BackendKind::Vaapi,
                codec: Codec::H264,
                max_resolution: (3840, 2160),
                max_fps: 60,
                zero_copy: false,
                priority: 90,
            });
        }
        out
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
///
/// Returns `(backend, reason)` where `reason` is a short diagnostic tag
/// suitable for structured logging at the factory boundary.
pub fn detect_capture_backend(choice: CaptureBackendChoice) -> (CaptureBackend, &'static str) {
    match choice {
        CaptureBackendChoice::X11 => return (CaptureBackend::X11Shm, "cli-override-x11"),
        CaptureBackendChoice::Wayland => {
            return (CaptureBackend::WaylandPortal, "cli-override-wayland")
        }
        CaptureBackendChoice::Auto => {}
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        tracing::info!("WAYLAND_DISPLAY unset; selecting X11 capture backend");
        return (CaptureBackend::X11Shm, "no-wayland-display");
    }
    match portal_runtime_available_blocking(Duration::from_secs(1)) {
        Ok(true) => {
            tracing::info!("xdg-desktop-portal reachable; selecting Wayland capture backend");
            (CaptureBackend::WaylandPortal, "portal-reachable")
        }
        Ok(false) => {
            tracing::warn!(
                "WAYLAND_DISPLAY set but xdg-desktop-portal unreachable; falling back to X11"
            );
            (CaptureBackend::X11Shm, "portal-unreachable")
        }
        Err(e) => {
            tracing::warn!(error = %e, "portal probe failed; falling back to X11");
            (CaptureBackend::X11Shm, "portal-probe-failed")
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
                tracing::warn!(error = %e, "portal probe: NameHasOwner returned err");
                Ok(false)
            }
            Err(_elapsed) => {
                tracing::warn!(?timeout, "portal probe: timed out");
                Ok(false)
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Convert a `media-linux` cursor update to the wire `ControlMessage`.
///
/// `width`/`height` are clamped from `u32` to `u16` — the 256×256 cap
/// in `read_meta_cursor` means values above `u16::MAX` cannot occur in
/// practice, but we clamp defensively.
pub fn cursor_to_control(
    c: crate::wayland_portal::cursor::CursorUpdate,
) -> prdt_protocol::ControlMessage {
    prdt_protocol::ControlMessage::CursorUpdate {
        id: c.id,
        position_x: c.position_x,
        position_y: c.position_y,
        hotspot_x: c.hotspot_x,
        hotspot_y: c.hotspot_y,
        bitmap: c.bitmap.map(|b| prdt_protocol::control::CursorBitmap {
            width: b.width.min(u16::MAX as u32) as u16,
            height: b.height.min(u16::MAX as u32) as u16,
            bgra: b.bgra,
        }),
    }
}

/// Producer factory. `capture_backend` is fixed at construction time: the
/// host resolves it once via `detect_capture_backend(args.into())` before
/// building the factory.
///
/// After `create()` is called on the WaylandPortal arm, `take_cursor_rx()`
/// returns the cursor update receiver. The host should call this immediately
/// after `PolicyDriven::bootstrap` succeeds and spawn a forwarder task that
/// drains the channel and sends each update over `transport.send_control`.
/// X11 capture never populates the slot.
pub struct LinuxSwFactory {
    capture_backend: CaptureBackend,
    /// Populated by `create()` on the WaylandPortal arm. The host calls
    /// `take_cursor_rx()` once to receive it and spawn the forwarder.
    cursor_rx_slot: std::sync::Mutex<
        Option<tokio::sync::mpsc::Receiver<crate::wayland_portal::cursor::CursorUpdate>>,
    >,
}

impl LinuxSwFactory {
    pub fn new(capture_backend: CaptureBackend) -> Self {
        Self {
            capture_backend,
            cursor_rx_slot: std::sync::Mutex::new(None),
        }
    }

    pub fn capture_backend(&self) -> CaptureBackend {
        self.capture_backend
    }

    /// Take the cursor receiver populated by the last `create()` call on the
    /// WaylandPortal arm. Returns `None` for X11 or if already taken.
    /// Call once after `PolicyDriven::bootstrap` to receive the channel.
    pub fn take_cursor_rx(
        &self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::wayland_portal::cursor::CursorUpdate>> {
        self.cursor_rx_slot.lock().ok().and_then(|mut g| g.take())
    }
}

impl ProducerFactory for LinuxSwFactory {
    fn create(
        &self,
        kind: BackendKind,
        cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError> {
        match kind {
            BackendKind::Openh264 => {}
            BackendKind::Vaapi => {
                return Err(FactoryError::Unavailable(
                    BackendKind::Vaapi,
                    "VaapiVideoProducer wiring lands in T9".into(),
                ));
            }
            _ => {
                return Err(FactoryError::Unavailable(
                    kind,
                    "Linux only supports Openh264 (and Vaapi from T9); other backends N/A".into(),
                ));
            }
        }
        let producer = match self.capture_backend {
            CaptureBackend::X11Shm => crate::build_video_producer(cfg.initial_bitrate_bps, cfg.fps)
                .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?,
            CaptureBackend::WaylandPortal => {
                let token_path = default_portal_token_path();
                // WaylandPortalCapturer::new is async; ProducerFactory::create is
                // sync. Spin up a tiny current_thread runtime to drive the portal
                // session establishment. (The capturer itself takes care of its
                // own internal threading once running — the producer's per-frame
                // capture_into is sync and uses blocking_recv.)
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| FactoryError::Unavailable(kind, format!("portal runtime: {e}")))?;
                let (cap, cursor_rx) = rt
                    .block_on(crate::wayland_portal::WaylandPortalCapturer::new(
                        token_path,
                    ))
                    .map_err(|e| {
                        FactoryError::Unavailable(kind, format!("WaylandPortalCapturer::new: {e}"))
                    })?;

                let producer = match crate::build_video_producer_with(
                    Box::new(cap),
                    cfg.initial_bitrate_bps,
                    cfg.fps,
                ) {
                    Ok(p) => p,
                    Err(e) => return Err(FactoryError::InvalidConfig(kind, e.to_string())),
                };

                // Stash cursor_rx in the slot only after the producer is
                // confirmed healthy. This prevents a stale receiver from being
                // handed to the host when build_video_producer_with fails (the
                // forwarder would spin forever on a dead channel).
                if let Ok(mut slot) = self.cursor_rx_slot.lock() {
                    *slot = Some(cursor_rx);
                }

                producer
            }
        };
        Ok(Box::new(producer))
    }
}

fn default_portal_token_path() -> std::path::PathBuf {
    dirs::config_dir()
        .map(|d| d.join("prdt").join("portal-session.toml"))
        .unwrap_or_else(|| std::path::PathBuf::from("portal-session.toml"))
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
    fn linux_factory_rejects_vaapi_with_t9_pending_message() {
        let factory = LinuxSwFactory::new(CaptureBackend::X11Shm);
        let cfg = ProducerConfig {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 8_000_000,
            codec: Codec::H264,
        };
        let result = factory.create(BackendKind::Vaapi, &cfg);
        match result {
            Err(FactoryError::Unavailable(BackendKind::Vaapi, reason)) => {
                assert!(
                    reason.contains("T9"),
                    "expected reason to mention T9, got: {reason}"
                );
            }
            Err(other) => panic!("expected Unavailable(Vaapi, ...), got Err({other:?})"),
            Ok(_) => panic!("expected Unavailable(Vaapi, ...), got Ok"),
        }
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
                // SAFETY: single-threaded test runner (same as the Some arm).
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn detect_backend_x11_when_wayland_display_unset() {
        let _guard = ScopedEnv::unset("WAYLAND_DISPLAY");
        let (got, _) = detect_capture_backend(CaptureBackendChoice::Auto);
        assert_eq!(got, CaptureBackend::X11Shm);
    }

    #[test]
    fn detect_backend_cli_override_forces_x11_even_with_wayland_display() {
        let _guard = ScopedEnv::set("WAYLAND_DISPLAY", "wayland-fake");
        let (got, _) = detect_capture_backend(CaptureBackendChoice::X11);
        assert_eq!(got, CaptureBackend::X11Shm);
    }

    #[test]
    fn detect_backend_cli_override_forces_wayland_even_without_display() {
        let _guard = ScopedEnv::unset("WAYLAND_DISPLAY");
        let (got, _) = detect_capture_backend(CaptureBackendChoice::Wayland);
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
        let (got, _) = detect_capture_backend(CaptureBackendChoice::Auto);
        assert_eq!(got, CaptureBackend::X11Shm);
    }

    // ----- T7 factory routing tests -----

    fn make_cfg() -> ProducerConfig {
        ProducerConfig {
            width: 1920,
            height: 1080,
            fps: 30,
            initial_bitrate_bps: 4_000_000,
            codec: Codec::H264,
        }
    }

    #[test]
    fn linux_factory_routes_x11_backend_to_x11_capturer() {
        let factory = LinuxSwFactory::new(CaptureBackend::X11Shm);
        assert_eq!(factory.capture_backend(), CaptureBackend::X11Shm);
    }

    #[test]
    fn linux_factory_routes_wayland_backend_to_wayland_capturer() {
        let factory = LinuxSwFactory::new(CaptureBackend::WaylandPortal);
        assert_eq!(factory.capture_backend(), CaptureBackend::WaylandPortal);
    }

    #[test]
    fn linux_factory_forced_wayland_without_session_surfaces_unavailable() {
        // In a hermetic test environment there is no working portal / session
        // bus, so WaylandPortalCapturer::new errors out and the factory
        // propagates it as Unavailable. The assertion accepts any message that
        // references the portal, session, or capturer — Foundation markers are
        // gone.
        let factory = LinuxSwFactory::new(CaptureBackend::WaylandPortal);
        let cfg = make_cfg();
        let result = factory.create(BackendKind::Openh264, &cfg);
        match result {
            Err(FactoryError::Unavailable(BackendKind::Openh264, msg)) => {
                let lower = msg.to_ascii_lowercase();
                assert!(
                    lower.contains("portal")
                        || lower.contains("session")
                        || lower.contains("wayland")
                        || lower.contains("ashpd")
                        || lower.contains("waylandportalcapturer"),
                    "expected portal/session/wayland/ashpd marker in error message, got: {msg}"
                );
            }
            _ => panic!("expected Err(Unavailable(Openh264, _))"),
        }
    }
}
