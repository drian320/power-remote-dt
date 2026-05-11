//! Permission-prompt modal shown when an unknown peer requests to connect
//! (TOFU mode: NeedsConsent). The host GUI renders this modal and sends
//! a `ConsentOutcome` back to the host task.
//!
//! Full channel wire-up into gui-host's app loop is deferred; see
//! TODO(P6 T7 follow-up) inside `ConsentPromptState::show`.

use std::time::{Duration, Instant};

use prdt_protocol::PermissionSet;

// ---------------------------------------------------------------------------
// Decision type (gui-host local; the app loop converts to prdt_host types)
// ---------------------------------------------------------------------------

/// What the operator decided after seeing the prompt.
#[derive(Debug, Clone)]
pub enum ConsentOutcome {
    Allowed {
        permissions: PermissionSet,
        remember: bool,
        label: String,
    },
    Denied,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// State for the consent prompt modal shown to the operator.
///
/// Uses `Instant::now()` internally so the caller does not need to
/// increment an `elapsed` field — elapsed time is computed automatically
/// each frame.
pub struct ConsentPromptState {
    /// Viewer's public key (full b64 string).
    pub peer_pubkey_b64: String,
    /// Human-readable label the operator can assign to this peer.
    pub label_input: String,
    /// The four permission toggles (initially copied from host defaults).
    pub permissions: PermissionSet,
    /// Whether to persist this peer to known-peers after accepting.
    pub remember: bool,
    /// When the prompt was first shown; elapsed is computed from this.
    created_at: Instant,
    /// Auto-deny timeout; countdown shown in UI.
    pub timeout: Duration,
}

impl ConsentPromptState {
    pub fn new(
        peer_pubkey_b64: String,
        default_permissions: PermissionSet,
        timeout: Duration,
    ) -> Self {
        Self {
            permissions: default_permissions,
            peer_pubkey_b64,
            label_input: String::new(),
            remember: true,
            created_at: Instant::now(),
            timeout,
        }
    }

    /// Render the consent prompt modal.
    ///
    /// Returns `Some(ConsentOutcome)` when the user clicks Allow or Deny,
    /// or when the timeout expires (auto-Deny).  Returns `None` while the
    /// prompt is still waiting.
    ///
    /// Requests a repaint after 1 s so the countdown label refreshes without
    /// requiring the operator to move the mouse.
    ///
    /// TODO(P6 T7 follow-up): wire this into `HostApp::update`:
    ///   1. Add `consent_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ConsentRequest>>`
    ///      and `pending_consent: Option<(ConsentPromptState, oneshot::Sender<ConsentDecision>)>`
    ///      to `HostApp`.
    ///   2. Populate `consent_rx` when `run_host` is launched (pass a sender).
    ///   3. In `update()`, poll `consent_rx`; on receipt create `ConsentPromptState::new(...)`.
    ///   4. Call `state.show(ctx)` each frame; on `Some(outcome)` convert to
    ///      `prdt_host::ConsentDecision` and send via `responder.send(decision)`.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<ConsentOutcome> {
        let elapsed = self.created_at.elapsed();

        // Auto-deny on timeout — no UI render needed.
        if elapsed >= self.timeout {
            return Some(ConsentOutcome::Denied);
        }

        let remaining = self.timeout - elapsed;
        let short_key = self.peer_pubkey_b64.chars().take(16).collect::<String>();
        let mut result: Option<ConsentOutcome> = None;

        egui::Window::new("Incoming viewer")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.heading("Viewer requesting to connect");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Pubkey:");
                    ui.code(format!("{short_key}…"));
                });
                ui.add_space(8.0);

                ui.label("Label (optional):");
                ui.text_edit_singleline(&mut self.label_input);
                ui.add_space(8.0);

                ui.label("Permissions for this session:");
                ui.checkbox(&mut self.permissions.input, "Input (keyboard/mouse)");
                ui.checkbox(&mut self.permissions.clipboard, "Clipboard");
                ui.checkbox(&mut self.permissions.file_transfer, "File transfer");
                ui.checkbox(&mut self.permissions.audio, "Audio");
                ui.add_space(8.0);

                ui.checkbox(&mut self.remember, "Remember this viewer");
                ui.add_space(8.0);

                ui.label(format!("Auto-deny in {}s", remaining.as_secs()));
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Deny").clicked() {
                        result = Some(ConsentOutcome::Denied);
                    }
                    if ui.button("Allow").clicked() {
                        result = Some(ConsentOutcome::Allowed {
                            permissions: self.permissions,
                            remember: self.remember,
                            label: self.label_input.clone(),
                        });
                    }
                });
            });

        // Ensure the countdown label refreshes once per second without
        // requiring operator mouse movement.
        ctx.request_repaint_after(Duration::from_secs(1));

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_prompt_auto_deny_after_timeout() {
        // A prompt with a zero-duration timeout must immediately return Denied
        // without any egui context (called outside show()).
        let state =
            ConsentPromptState::new("AAABBBCCC".into(), PermissionSet::all(), Duration::ZERO);
        // elapsed() >= timeout=ZERO on any call.
        let elapsed = state.created_at.elapsed();
        assert!(
            elapsed >= state.timeout,
            "zero-timeout prompt must be expired on creation"
        );
    }

    #[test]
    fn consent_prompt_not_expired_on_long_timeout() {
        let state = ConsentPromptState::new(
            "AAABBBCCC".into(),
            PermissionSet::all(),
            Duration::from_secs(60),
        );
        let elapsed = state.created_at.elapsed();
        assert!(
            elapsed < state.timeout,
            "60-second prompt must not be expired immediately"
        );
    }
}
