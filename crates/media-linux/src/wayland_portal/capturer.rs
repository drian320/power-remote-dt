//! Wayland portal capture backend — real implementation (T6).
//!
//! Wires `PortalSession` + `PipeWireStream` + `PortalSessionToken`
//! through the `CaptureSource` trait.

#![cfg(target_os = "linux")]

use crate::capture_source::{CaptureSource, CaptureSourceError};
use thiserror::Error;

use super::{
    PipeWireStream, PipeWireStreamError, PortalSession, PortalSessionToken, WaylandPortalError,
};
use ashpd::desktop::{screencast::Screencast, Session};

/// Errors produced when constructing a `WaylandPortalCapturer`.
#[derive(Debug, Error)]
pub enum WaylandPortalCapturerInitError {
    #[error("portal session start failed: {0}")]
    PortalSession(#[from] WaylandPortalError),

    #[error("pipewire stream connect failed: {0}")]
    PipeWireStream(#[from] PipeWireStreamError),
}

/// Capture backend for the Wayland XDG ScreenCast portal.
///
/// Lifecycle: `new` → `capture_into` (repeated) → `shutdown`.
/// Dropping without `shutdown` is allowed but logs a warning; the
/// portal session will age out on the compositor side eventually.
pub struct WaylandPortalCapturer {
    /// Some until `shutdown()` is called; consumed for `session.close().await`.
    session: Option<Session<'static, Screencast<'static>>>,
    /// Always `Some` during lifetime — stream owns the pipewire mainloop thread.
    /// `None` only after `shutdown()`.
    stream: Option<PipeWireStream>,
    /// Frame receiver — separated from `PipeWireStream` since T3 (connect
    /// returns a 3-tuple). `None` only after `shutdown()`.
    frame_rx: Option<tokio::sync::mpsc::Receiver<super::stream::RawFrame>>,
    token_path: std::path::PathBuf,
    /// Set `true` by `shutdown()`; `Drop` warns if `false`.
    shutdown_completed: bool,
}

impl WaylandPortalCapturer {
    /// Build the capturer.
    ///
    /// Returns `(Self, cursor_rx)` where `cursor_rx` carries [`super::cursor::CursorUpdate`]
    /// values drained from `SPA_META_Cursor` on each PipeWire buffer. The cursor
    /// channel is empty when the portal negotiated `CursorMode::Embedded`.
    ///
    /// 1. Load restore token from disk (if any).
    /// 2. Open portal session — fires OS consent dialog first launch, or
    ///    restores previous grant if token is still valid.
    /// 3. On `RestoreTokenRejected`: delete token file, retry without token
    ///    (operator will see the dialog again).
    /// 4. Connect PipeWire stream to the node id returned by portal.
    /// 5. Persist any new restore token returned by the portal.
    pub async fn new(
        token_path: std::path::PathBuf,
    ) -> Result<
        (
            Self,
            tokio::sync::mpsc::Receiver<super::cursor::CursorUpdate>,
        ),
        WaylandPortalCapturerInitError,
    > {
        // Step 1 — load persisted token.
        let token = PortalSessionToken::load_or_default(&token_path);
        let token_opt = token.token_opt().map(str::to_owned);

        // Step 2 / 3 — open portal session with optional restore token.
        let output = {
            let first_try = PortalSession::start_with_token_opt(token_opt.as_deref()).await;

            match first_try {
                Ok(o) => o,
                Err(ref e) if e.is_token_invalid() => {
                    tracing::warn!(
                        token_path = %token_path.display(),
                        "portal rejected stored restore_token; deleting and retrying as first launch"
                    );
                    let _ = std::fs::remove_file(&token_path);
                    // Retry without token — propagate any error from this call.
                    PortalSession::start_with_token_opt(None).await?
                }
                Err(e) => return Err(WaylandPortalCapturerInitError::PortalSession(e)),
            }
        };

        let super::session::PortalStartOutput {
            session,
            pipewire_fd,
            pipewire_node_id,
            restore_token: new_token,
            cursor_mode: _cursor_mode,
        } = output;

        // Step 4 — connect PipeWire stream. connect() returns (stream, frame_rx, cursor_rx).
        let (stream, frame_rx, cursor_rx) =
            PipeWireStream::connect(pipewire_fd, pipewire_node_id, 2, 8)?;

        // Step 5 — persist new restore token if portal issued one.
        if let Some(tok) = new_token {
            let to_save = PortalSessionToken::with_token(tok, "unknown");
            if let Err(e) = to_save.save(&token_path) {
                tracing::warn!(
                    error = %e,
                    token_path = %token_path.display(),
                    "failed to persist portal restore token; next launch will show consent dialog"
                );
            }
        }

        let capturer = Self {
            session: Some(session),
            stream: Some(stream),
            frame_rx: Some(frame_rx),
            token_path,
            shutdown_completed: false,
        };
        Ok((capturer, cursor_rx))
    }

    /// Orderly shutdown: close PipeWire stream (joins thread), then close
    /// the D-Bus portal session.
    ///
    /// Sets `shutdown_completed` so `Drop` does not emit the leak warning.
    pub async fn shutdown(mut self) -> Result<(), WaylandPortalError> {
        // Drop frame_rx first so the PipeWire thread's try_send returns Closed
        // and it can unblock, then consume stream to join the thread.
        drop(self.frame_rx.take());
        if let Some(s) = self.stream.take() {
            s.shutdown();
        }

        // Close the portal session.
        if let Some(sess) = self.session.take() {
            PortalSession::close(sess).await?;
        }

        self.shutdown_completed = true;
        Ok(())
    }

    // ── #[cfg(test)] helper ───────────────────────────────────────────────────

    #[cfg(test)]
    fn with_test_state(
        stream: Option<PipeWireStream>,
        frame_rx: Option<tokio::sync::mpsc::Receiver<super::stream::RawFrame>>,
        session: Option<Session<'static, Screencast<'static>>>,
        token_path: std::path::PathBuf,
    ) -> Self {
        Self {
            session,
            stream,
            frame_rx,
            token_path,
            shutdown_completed: false,
        }
    }
}

impl CaptureSource for WaylandPortalCapturer {
    fn geometry(&self) -> (u32, u32) {
        self.stream
            .as_ref()
            .map(|s| s.current_size())
            .unwrap_or((0, 0))
    }

    fn capture_into(&mut self, out: &mut Vec<u8>) -> Result<(), CaptureSourceError> {
        let Some(frame_rx) = self.frame_rx.as_mut() else {
            return Err(CaptureSourceError::Terminal {
                backend: "wayland-portal",
                reason: "capturer shut down".into(),
            });
        };

        // The CaptureSource trait is sync; the producer wraps the call in
        // `spawn_blocking`, so blocking here is safe.
        let frame = frame_rx
            .blocking_recv()
            .ok_or_else(|| CaptureSourceError::Terminal {
                backend: "wayland-portal",
                reason: "pipewire stream closed".into(),
            })?;

        let width = frame.width as usize;
        let height = frame.height as usize;
        let stride = frame.stride as usize;

        if stride > width * 4 {
            // Row-by-row copy to strip Intel iGPU stride padding.
            out.clear();
            out.reserve(width * height * 4);
            for y in 0..frame.height {
                out.extend_from_slice(frame.row(y));
            }
        } else {
            out.clear();
            out.extend_from_slice(&frame.data);
        }

        Ok(())
    }
}

impl Drop for WaylandPortalCapturer {
    fn drop(&mut self) {
        if !self.shutdown_completed {
            tracing::warn!(
                "WaylandPortalCapturer dropped without explicit shutdown(); \
                 portal session will leak until the compositor times it out"
            );
        }
        // stream's own Drop fires the PipeWire quit signal best-effort (T5).
        // session: cannot .await session.close() inside Drop — log only.
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the `shutdown_completed` discipline: newly constructed state has the
    /// flag false; once set it suppresses the Drop warning.
    ///
    /// This exercises the private-fields invariant via the test constructor
    /// without needing a real portal or PipeWire daemon.
    #[test]
    fn shutdown_completed_default_is_false() {
        let capturer = WaylandPortalCapturer::with_test_state(
            None,
            None,
            None,
            std::path::PathBuf::from("/tmp/test-portal-session.toml"),
        );
        assert!(
            !capturer.shutdown_completed,
            "shutdown_completed must be false before explicit shutdown"
        );
        // Force the flag so Drop won't warn during this test.
        let mut c = capturer;
        c.shutdown_completed = true;
    }

    /// geometry() returns (0, 0) when the stream slot is None (post-shutdown
    /// safety net).
    #[test]
    fn geometry_returns_zero_when_stream_none() {
        let capturer = WaylandPortalCapturer::with_test_state(
            None,
            None,
            None,
            std::path::PathBuf::from("/tmp/test-portal-session.toml"),
        );
        assert_eq!(capturer.geometry(), (0, 0));
        let mut c = capturer;
        c.shutdown_completed = true; // suppress Drop warn
    }

    /// capture_into returns Terminal when frame_rx is None (post-shutdown).
    #[test]
    fn capture_into_terminal_when_stream_none() {
        let mut capturer = WaylandPortalCapturer::with_test_state(
            None,
            None,
            None,
            std::path::PathBuf::from("/tmp/test-portal-session.toml"),
        );
        let mut buf = Vec::new();
        let result = capturer.capture_into(&mut buf);
        assert!(
            matches!(
                result,
                Err(CaptureSourceError::Terminal {
                    backend: "wayland-portal",
                    ..
                })
            ),
            "expected Terminal error when frame_rx is None"
        );
        capturer.shutdown_completed = true; // suppress Drop warn
    }
}
