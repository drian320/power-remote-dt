//! Cross-crate channel types for the host ↔ GUI consent prompt flow.
//!
//! `prdt-host` (which depends on `prdt-gui-host`) constructs the receiving end
//! and drives the GUI from the network task; `HostApp` polls the receiver and
//! renders the modal. Both crates share the types defined here so they speak
//! the same protocol across the dependency edge.

use prdt_crypto::PubKey;
use prdt_protocol::control::PermissionSet;

/// A request originating from the host network task asking the GUI to show
/// a consent prompt for an unknown peer.
///
/// `responder` is a one-shot reply channel. The GUI sends exactly one
/// `ConsentDecision` (Accepted or Rejected) back through it. Dropping the
/// responder without sending counts as rejection on the receiving side.
#[derive(Debug)]
pub struct ConsentRequest {
    pub peer_pubkey: PubKey,
    pub responder: tokio::sync::oneshot::Sender<ConsentDecision>,
}

/// What the operator decided after seeing the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentDecision {
    /// Viewer is accepted. `permissions` caps the session, `remember` causes
    /// the peer to be persisted to known-peers so future connections are
    /// silent, and `label` is the human-readable name to store.
    Accepted {
        permissions: PermissionSet,
        remember: bool,
        label: String,
    },
    Rejected,
}

/// Sender half of the consent channel: held by `prdt-host`'s run loop and
/// passed into the network task. `None` means "headless host with no GUI" —
/// callers auto-reject in that case.
pub type ConsentSender = tokio::sync::mpsc::UnboundedSender<ConsentRequest>;

/// Receiver half of the consent channel: held by `HostApp`. Each item is one
/// `ConsentRequest`. The receiver is dropped when listening stops, which
/// closes the channel and causes the next `send` on the host side to fail —
/// the host treats that as rejection.
pub type ConsentReceiver = tokio::sync::mpsc::UnboundedReceiver<ConsentRequest>;
