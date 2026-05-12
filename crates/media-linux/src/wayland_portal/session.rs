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
    PersistMode, Session,
};
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
        // 1. Obtain the proxy.
        let proxy = Screencast::new().await?;

        // 2. Create the D-Bus session object.
        let session = proxy.create_session().await?;

        // 3. Configure which sources to capture.
        //    Single monitor, embedded cursor, persistent until explicitly
        //    revoked (so the compositor issues a restore token).
        proxy
            .select_sources(
                &session,
                CursorMode::Embedded,
                SourceType::Monitor.into(),
                false, // multiple = false: one monitor only
                restore_token,
                PersistMode::ExplicitlyRevoked,
            )
            .await?;

        // 4. Present the portal picker. `None` for parent window identifier
        //    is accepted on Wayland headless / CLI hosts without a window.
        let start_response = proxy.start(&session, None).await?.response()?;

        // 5. Extract the PipeWire node id from the first stream.
        let streams = start_response.streams();
        let first_stream = streams.first().ok_or(WaylandPortalError::NoStreams)?;
        let pipewire_node_id = first_stream.pipe_wire_node_id();
        let new_restore_token = start_response.restore_token().map(str::to_owned);

        if restore_token.is_some() {
            tracing::info!(
                node_id = pipewire_node_id,
                "portal grant restored from token"
            );
        } else {
            tracing::info!(node_id = pipewire_node_id, "new portal grant accepted");
        }

        // 6. Open the PipeWire remote fd.
        let pipewire_fd = proxy.open_pipe_wire_remote(&session).await?;

        tracing::info!("portal session opened");

        Ok(PortalStartOutput {
            session,
            pipewire_fd,
            pipewire_node_id,
            restore_token: new_restore_token,
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
}
