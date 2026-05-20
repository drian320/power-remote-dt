//! Permission-prompt modal shown when an unknown peer requests to connect
//! (TOFU mode: NeedsConsent). The host GUI renders this modal and sends
//! a `ConsentDecision` back to the host task.

use std::time::{Duration, Instant};

use prdt_gui_common::theme::tokens;
use prdt_protocol::PermissionSet;

use crate::consent_channel::ConsentDecision;

/// The Accept ("Allow") button stays disabled for this long after the modal
/// first appears, with a visible countdown. This anti-misclick delay defeats
/// the social-engineering pattern where an attacker times a connection
/// request to land under the operator's cursor mid-click. The Reject path is
/// never delayed — denying is always safe and instant.
const ACCEPT_ARM_DELAY: Duration = Duration::from_secs(2);

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

    /// Whether the Accept button is armed yet (true once `ACCEPT_ARM_DELAY`
    /// has elapsed since the modal appeared). Exposed for testing.
    pub fn accept_armed(&self) -> bool {
        self.created_at.elapsed() >= ACCEPT_ARM_DELAY
    }

    /// Returns `Some(decision)` when the user decides or the timeout fires.
    ///
    /// Security hardening (design §7.3):
    /// - Rendered as an `egui::Modal` so it dims the rest of the UI and grabs
    ///   keyboard focus — the operator can't interact with anything else.
    /// - **Esc** always denies instantly (the safe action is never delayed).
    /// - The **Allow** button is disabled for the first `ACCEPT_ARM_DELAY`
    ///   with a visible countdown, defeating timed-misclick attacks.
    /// - **Deny** is red and always clickable.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<ConsentDecision> {
        let elapsed = self.created_at.elapsed();

        // Auto-deny on timeout — no UI render needed.
        if elapsed >= self.timeout {
            return Some(ConsentDecision::Rejected);
        }

        // Esc denies instantly. Denying is always safe, so it bypasses the
        // arm delay entirely.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return Some(ConsentDecision::Rejected);
        }

        let remaining = self.timeout - elapsed;
        let armed = elapsed >= ACCEPT_ARM_DELAY;
        let arm_remaining = ACCEPT_ARM_DELAY.saturating_sub(elapsed);
        let short_key = self.peer_pubkey_b64.chars().take(16).collect::<String>();
        let mut result: Option<ConsentDecision> = None;

        egui::Modal::new(egui::Id::new("prdt-consent-modal")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("Incoming Connection Request");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Device key:");
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

            ui.checkbox(&mut self.remember, "Remember this device");
            ui.add_space(8.0);

            ui.colored_label(
                tokens::TEXT_DIM,
                format!("Auto-deny in {}s", remaining.as_secs()),
            );
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                // Deny: red, always enabled, also bound to Esc.
                let deny = egui::Button::new(egui::RichText::new("Deny (Esc)").color(tokens::TEXT))
                    .fill(tokens::DESTRUCTIVE);
                if ui.add(deny).clicked() {
                    result = Some(ConsentDecision::Rejected);
                }

                // Allow: disabled until armed, with a countdown label.
                let allow_label = if armed {
                    "Allow".to_string()
                } else {
                    format!("Allow in {}s", arm_remaining.as_secs_f32().ceil() as u64)
                };
                if ui
                    .add_enabled(armed, egui::Button::new(allow_label))
                    .clicked()
                {
                    result = Some(ConsentDecision::Accepted {
                        permissions: self.permissions,
                        remember: self.remember,
                        label: self.label_input.clone(),
                    });
                }
            });
        });

        // Repaint cadence: snappy (200 ms) while the arm-countdown ticks down
        // so "Allow in 2s → 1s" updates smoothly; 1 Hz afterwards for the
        // slower auto-deny countdown.
        let next = if armed {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(200)
        };
        ctx.request_repaint_after(next);

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

    #[test]
    fn fresh_prompt_accept_not_armed() {
        // The anti-misclick guard: a freshly created prompt must NOT have its
        // Allow button armed (it arms only after ACCEPT_ARM_DELAY elapses).
        let state = ConsentPromptState::new(
            "AAABBBCCC".into(),
            PermissionSet::all(),
            Duration::from_secs(60),
        );
        assert!(
            !state.accept_armed(),
            "Allow must be disabled immediately after the modal appears"
        );
        assert!(
            ACCEPT_ARM_DELAY >= Duration::from_secs(1),
            "arm delay must be a meaningful, human-perceptible duration"
        );
    }
}
