//! XDG ScreenCast portal session lifecycle — open, restore, close.
//!
//! `PortalSession` drives the canonical flow:
//!   1. `Screencast::new()` — obtain proxy
//!   2. `create_session()` — allocate a D-Bus session object
//!   3. `select_sources(...)` — configure source type / cursor / persistence
//!   4. `start(...)` — present the portal picker dialog
//!   5. Extract `pipe_wire_node_id` + optional `restore_token` from the response
//!   6. `open_pipe_wire_remote()` — obtain `OwnedFd` for PipeWire
//!
//! On success the session handle and PipeWire fd are bundled into
//! `PortalStartOutput` and moved to the caller. The caller is responsible for
//! calling `PortalSession::close()` when done to release compositor resources.

#![cfg(target_os = "linux")]

use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SourceType},
    PersistMode, ResponseError, Session,
};
use ashpd::PortalError;
use std::os::fd::OwnedFd;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by the XDG ScreenCast portal session lifecycle.
#[derive(Debug, Error)]
pub enum WaylandPortalError {
    /// The underlying ashpd / D-Bus call failed.
    #[error("ashpd portal error: {0}")]
    Ashpd(String),

    /// The user dismissed or cancelled the portal authorization dialog.
    #[error("user cancelled portal authorization")]
    UserCancelled,

    /// The compositor rejected the supplied restore token (stale / unknown).
    /// Callers should delete the persisted token and retry without one.
    #[error("restore token rejected by compositor: {0}")]
    RestoreTokenRejected(String),

    /// The portal returned zero PipeWire streams after a successful start.
    #[error("portal returned no PipeWire streams")]
    NoStreams,
}

impl WaylandPortalError {
    /// Returns `true` only for [`WaylandPortalError::RestoreTokenRejected`].
    /// Used by the token manager to decide whether to delete a persisted
    /// token after a failed session attempt.
    pub fn is_token_invalid(&self) -> bool {
        matches!(self, WaylandPortalError::RestoreTokenRejected(_))
    }
}

impl From<ashpd::Error> for WaylandPortalError {
    fn from(e: ashpd::Error) -> Self {
        WaylandPortalError::Ashpd(e.to_string())
    }
}

/// Classify an `ashpd::Error` into the structured [`WaylandPortalError`] variants.
///
/// `token_was_passed` indicates whether a restore token was supplied to the
/// current call site. If true, any non-cancel failure is treated as the
/// compositor rejecting the stored token (grant revoked / token rotated /
/// monitor disconnected), so T6's delete-and-retry branch fires correctly.
///
/// Classification priority:
/// 1. Structured variant match — `Response(Cancelled)` → `UserCancelled`.
/// 2. Portal-layer cancel — `Portal(Cancelled(_))` → `UserCancelled`.
/// 3. String-sniff fallback — contains "cancel" → `UserCancelled`.
/// 4. Token context — token was passed → `RestoreTokenRejected`.
/// 5. Default — `Ashpd`.
fn classify_portal_error(e: ashpd::Error, token_was_passed: bool) -> WaylandPortalError {
    // 1. Structured match on ashpd::Error variants.
    match &e {
        ashpd::Error::Response(ResponseError::Cancelled) => {
            return WaylandPortalError::UserCancelled;
        }
        ashpd::Error::Portal(PortalError::Cancelled(_)) => {
            return WaylandPortalError::UserCancelled;
        }
        // ResponseError::Other is ambiguous — fall through to string check.
        _ => {}
    }

    // 2. String-sniff fallback: last resort for cancel paths missed above.
    // NOTE: ashpd 0.12.3 surfaces most portal-side errors through
    // Error::Response (two variants: Cancelled / Other) or Error::Portal.
    // The structured matches above cover the known cancel paths; this string
    // check is retained as a safety net for any undocumented code path.
    let s = e.to_string();
    if s.to_ascii_lowercase().contains("cancel") {
        return WaylandPortalError::UserCancelled;
    }

    // 3. If a restore token was in play, any remaining failure is almost
    // certainly the compositor rejecting the stale/rotated token.
    if token_was_passed {
        tracing::warn!(
            error = %s,
            "portal session: stored restore_token rejected; T6 should delete and retry"
        );
        WaylandPortalError::RestoreTokenRejected(s)
    } else {
        WaylandPortalError::Ashpd(s)
    }
}

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

/// Successful output from `PortalSession::start_with_token_opt`.
pub struct PortalStartOutput {
    /// Live D-Bus session handle. Must be closed via `PortalSession::close`
    /// when the caller is done capturing.
    pub session: Session<'static, Screencast<'static>>,
    /// PipeWire remote file descriptor. Pass to `pw_core_connect_fd` or
    /// equivalent. The fd is owned; dropping it closes the remote.
    pub pipewire_fd: OwnedFd,
    /// PipeWire stream node ID. Used to link the PipeWire graph node to the
    /// capture stream offered by the compositor.
    pub pipewire_node_id: u32,
    /// New restore token issued by the compositor (if any). Persist this with
    /// `PortalSessionToken::save` so the next session can skip the picker UI.
    pub restore_token: Option<String>,
    /// Cursor mode that was actually negotiated with the portal.
    /// `CursorMode::Metadata` when the compositor advertises it; otherwise
    /// `CursorMode::Embedded` (cursor baked into the video frame).
    pub cursor_mode: CursorMode,
}

// ---------------------------------------------------------------------------
// Session driver
// ---------------------------------------------------------------------------

/// Thin driver around the ashpd `Screencast` proxy.
///
/// All methods are free functions on a ZST; construction is deliberately
/// omitted to keep the type stateless — the proxy is created per-call.
pub struct PortalSession;

impl PortalSession {
    /// Run the full ScreenCast portal flow and return a live `PortalStartOutput`.
    ///
    /// When `restore_token` is `Some`, the flow passes it to `select_sources`
    /// via `PersistMode::ExplicitlyRevoked`; if the compositor rejects it
    /// the error is mapped to `WaylandPortalError::RestoreTokenRejected`.
    ///
    /// When `restore_token` is `None` this is treated as a first-launch path
    /// and the user will see the full source-picker dialog.
    pub async fn start_with_token_opt(
        restore_token: Option<&str>,
    ) -> Result<PortalStartOutput, WaylandPortalError> {
        let token_was_passed = restore_token.is_some();
        tracing::info!(has_token = token_was_passed, "portal session: starting");

        // 1. Obtain the proxy.
        let proxy = Screencast::new().await?;

        // 2. Create the D-Bus session object.
        let session = proxy.create_session().await?;
        tracing::info!("portal session: created");

        // 3. Configure which sources to capture.
        //    Single monitor, persistent until explicitly revoked (so the
        //    compositor issues a restore token).
        //
        //    P5B-2b: probe available cursor modes and prefer Metadata so the
        //    host receives per-frame spa_meta_cursor updates instead of baking
        //    the cursor into the video frame. Fall back to Embedded if the
        //    portal (or the compositor behind it) doesn't advertise Metadata.
        let available_modes = proxy.available_cursor_modes().await.unwrap_or_default();
        let cursor_mode = if available_modes.contains(CursorMode::Metadata) {
            tracing::info!("portal advertises Metadata cursor mode — using it");
            CursorMode::Metadata
        } else {
            tracing::warn!(
                ?available_modes,
                "portal does not advertise Metadata cursor mode — falling back to Embedded"
            );
            CursorMode::Embedded
        };
        proxy
            .select_sources(
                &session,
                cursor_mode,
                SourceType::Monitor.into(),
                false, // multiple = false: one monitor only
                restore_token,
                PersistMode::ExplicitlyRevoked,
            )
            .await
            .map_err(|e| classify_portal_error(e, token_was_passed))?;

        // 4. Present the portal picker. `None` for parent window identifier
        //    is accepted on Wayland headless / CLI hosts without a window.
        let start_response = proxy
            .start(&session, None)
            .await
            .map_err(|e| classify_portal_error(e, token_was_passed))?
            .response()
            .map_err(|e| classify_portal_error(e, token_was_passed))?;

        // 5. Extract the PipeWire node id from the first stream.
        let streams = start_response.streams();
        let first_stream = streams.first().ok_or(WaylandPortalError::NoStreams)?;
        let pipewire_node_id = first_stream.pipe_wire_node_id();
        let new_restore_token = start_response.restore_token().map(str::to_owned);

        tracing::info!(
            pipewire_node_id,
            has_new_token = new_restore_token.is_some(),
            "portal session: started"
        );

        // 6. Open the PipeWire remote fd.
        let pipewire_fd = proxy.open_pipe_wire_remote(&session).await?;

        tracing::info!("portal session opened");

        Ok(PortalStartOutput {
            session,
            pipewire_fd,
            pipewire_node_id,
            restore_token: new_restore_token,
            cursor_mode,
        })
    }

    /// Explicitly close an open portal session.
    ///
    /// This releases the compositor-side capture grant and the D-Bus session
    /// object. After this call the associated PipeWire fd will become invalid.
    pub async fn close(
        session: Session<'static, Screencast<'static>>,
    ) -> Result<(), WaylandPortalError> {
        session.close().await.map_err(WaylandPortalError::from)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_error_display_user_cancelled() {
        let e = WaylandPortalError::UserCancelled;
        assert_eq!(e.to_string(), "user cancelled portal authorization");
    }

    #[test]
    fn portal_error_token_invalid_triggers_deletion_signal() {
        // Only RestoreTokenRejected should return true.
        assert!(WaylandPortalError::RestoreTokenRejected("stale".into()).is_token_invalid());

        // All other variants must return false.
        assert!(!WaylandPortalError::Ashpd("dbus failed".into()).is_token_invalid());
        assert!(!WaylandPortalError::UserCancelled.is_token_invalid());
        assert!(!WaylandPortalError::NoStreams.is_token_invalid());
    }

    #[test]
    fn classify_cancel_string_fallback_maps_to_user_cancelled() {
        // ashpd::Error::NoResponse doesn't match a structured cancel variant,
        // but if its Display contains "cancel" the string fallback fires.
        // We use a Zbus error whose message embeds "cancel" to exercise the
        // string-sniff branch directly.
        let e = ashpd::Error::ParseError("user cancel requested");
        let result = classify_portal_error(e, false);
        assert!(
            matches!(result, WaylandPortalError::UserCancelled),
            "expected UserCancelled for cancel-containing error string"
        );
    }

    #[test]
    fn classify_non_cancel_with_token_maps_to_token_rejected() {
        // A non-cancel error when a token was passed should surface as
        // RestoreTokenRejected so T6's delete-and-retry branch fires.
        let e = ashpd::Error::ParseError("grant revoked by compositor");
        let result = classify_portal_error(e, true);
        assert!(
            matches!(result, WaylandPortalError::RestoreTokenRejected(_)),
            "expected RestoreTokenRejected when token_was_passed=true and no cancel in message"
        );
        assert!(result.is_token_invalid());
    }
}
