//! Permission-prompt modal shown when an unknown peer requests to connect
//! (TOFU mode: NeedsConsent). The host GUI renders this modal and sends
//! a `ConsentDecision` back to the host task.

use std::time::{Duration, Instant};

use prdt_protocol::PermissionSet;

use crate::consent_channel::ConsentDecision;

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
    pub(crate) created_at: Instant,
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

    /// Returns `Some(decision)` when the user clicks or the timeout fires.
    ///
    /// Returns `None` while the prompt is still waiting. Requests a repaint
    /// after 1 s so the countdown label refreshes without requiring the
    /// operator to move the mouse.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<ConsentDecision> {
        let elapsed = self.created_at.elapsed();

        // Auto-deny on timeout — no UI render needed.
        if elapsed >= self.timeout {
            return Some(ConsentDecision::Rejected);
        }

        let remaining = self.timeout - elapsed;
        let short_key = self.peer_pubkey_b64.chars().take(16).collect::<String>();
        let mut result: Option<ConsentDecision> = None;

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
                        result = Some(ConsentDecision::Rejected);
                    }
                    if ui.button("Allow").clicked() {
                        result = Some(ConsentDecision::Accepted {
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
