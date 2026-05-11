//! Auth settings panel for the host GUI (P6 T7).
//!
//! Mounted inside the Settings window.  Allows changing AuthMode, PIN,
//! default permissions, and managing saved peers.

use prdt_gui_common::{auth_config::HostAuthConfig, AuthMode};
use prdt_protocol::PermissionSet;

use crate::onboarding::WizardError;

// ---------------------------------------------------------------------------
// PIN change modal state
// ---------------------------------------------------------------------------

pub enum PinEditMode {
    Idle,
    /// Fields: (current_pin_input, new_pin_input, confirm_new_pin).
    Verifying(String, String, String),
}

// ---------------------------------------------------------------------------
// Testable submit handler
// ---------------------------------------------------------------------------

/// Change the PIN on an existing `HostAuthConfig`.
///
/// Requires the correct current PIN before accepting the new one. Returns
/// `WizardError::WrongCurrentPin` if `current_pin` does not match the stored
/// hash, or `WizardError::PinTooShort` if `new_pin` is < 6 chars.
pub fn apply_pin_change(
    current_pin: &str,
    new_pin: &str,
    host_auth: &mut HostAuthConfig,
) -> Result<(), WizardError> {
    if !host_auth.verify_pin(current_pin) {
        return Err(WizardError::WrongCurrentPin);
    }
    if new_pin.len() < 6 {
        return Err(WizardError::PinTooShort);
    }
    host_auth.pin_hash = Some(HostAuthConfig::hash_pin(new_pin)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// Settings panel state
// ---------------------------------------------------------------------------

/// State for the auth section of the Settings panel.
pub struct AuthSettingsState {
    /// Working copy of the auth config; caller must flush to disk on save.
    pub host_auth: HostAuthConfig,
    pub pin_edit_mode: PinEditMode,
    pub pin_error: Option<String>,
    /// Ephemeral code currently shown (only meaningful in Ephemeral mode).
    pub current_ephemeral: Option<String>,
    /// Whether the ephemeral is currently visible to the user.
    ephemeral_visible: bool,
}

impl AuthSettingsState {
    pub fn new(host_auth: HostAuthConfig) -> Self {
        Self {
            host_auth,
            pin_edit_mode: PinEditMode::Idle,
            pin_error: None,
            current_ephemeral: None,
            ephemeral_visible: false,
        }
    }

    /// Render the auth settings UI inside an existing `ui` layout.
    ///
    /// Returns `true` if the caller should flush `self.host_auth` to disk
    /// (i.e., the user changed something and confirmed).
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let mut dirty = false;

        ui.heading("Authentication");
        ui.add_space(4.0);

        // ----------------------------------------------------------------
        // Mode radio
        // ----------------------------------------------------------------
        ui.label("Mode:");
        let mut mode = self.host_auth.mode;
        ui.radio_value(&mut mode, AuthMode::Tofu, "TOFU (prompt unknown viewers)");
        ui.radio_value(&mut mode, AuthMode::Pin, "PIN (required every connection)");
        ui.radio_value(&mut mode, AuthMode::Ephemeral, "Ephemeral (rotating code)");
        if mode != self.host_auth.mode {
            self.host_auth.mode = mode;
            dirty = true;
            // TODO(P6 T7 follow-up): trigger reload_auth() callback so the
            // running AuthValidator picks up the new mode without a restart.
        }

        ui.add_space(8.0);

        // ----------------------------------------------------------------
        // PIN section
        // ----------------------------------------------------------------
        if self.host_auth.mode == AuthMode::Pin {
            // Render the PIN UI in a sub-scope so the mutable borrow on
            // self.pin_edit_mode is released before we potentially reassign it.
            enum PinAction {
                OpenEdit,
                Cancel,
                Apply(String, String), // (current, new)
                None,
            }
            let action = match &mut self.pin_edit_mode {
                PinEditMode::Idle => {
                    let hash_status = if self.host_auth.pin_hash.is_some() {
                        "PIN set"
                    } else {
                        "No PIN set"
                    };
                    ui.label(hash_status);
                    if ui.button("Change PIN…").clicked() {
                        PinAction::OpenEdit
                    } else {
                        PinAction::None
                    }
                }
                PinEditMode::Verifying(current, new_pin, confirm) => {
                    ui.label("Current PIN:");
                    ui.add(egui::TextEdit::singleline(current).password(true));
                    ui.label("New PIN (min 6 chars):");
                    ui.add(egui::TextEdit::singleline(new_pin).password(true));
                    ui.label("Confirm new PIN:");
                    ui.add(egui::TextEdit::singleline(confirm).password(true));
                    if let Some(err) = &self.pin_error {
                        ui.colored_label(egui::Color32::RED, err);
                    }
                    let cancel = ui.button("Cancel").clicked();
                    let apply = ui.button("Apply").clicked();
                    if cancel {
                        PinAction::Cancel
                    } else if apply {
                        if new_pin != confirm {
                            self.pin_error = Some("New PINs do not match.".into());
                            PinAction::None
                        } else {
                            PinAction::Apply(current.clone(), new_pin.clone())
                        }
                    } else {
                        PinAction::None
                    }
                }
            };
            // Apply the action now that the borrow on pin_edit_mode is released.
            match action {
                PinAction::OpenEdit => {
                    self.pin_edit_mode =
                        PinEditMode::Verifying(String::new(), String::new(), String::new());
                    self.pin_error = None;
                }
                PinAction::Cancel => {
                    self.pin_edit_mode = PinEditMode::Idle;
                    self.pin_error = None;
                }
                PinAction::Apply(current_str, new_str) => {
                    match apply_pin_change(&current_str, &new_str, &mut self.host_auth) {
                        Ok(()) => {
                            self.pin_edit_mode = PinEditMode::Idle;
                            self.pin_error = None;
                            dirty = true;
                        }
                        Err(e) => {
                            self.pin_error = Some(e.to_string());
                        }
                    }
                }
                PinAction::None => {}
            }
            ui.add_space(8.0);
        }

        // ----------------------------------------------------------------
        // Ephemeral section
        // ----------------------------------------------------------------
        if self.host_auth.mode == AuthMode::Ephemeral {
            // Ensure we have an ephemeral code, then extract a clone to avoid
            // holding a borrow on self.current_ephemeral inside the closure.
            if self.current_ephemeral.is_none() {
                self.current_ephemeral = Some(HostAuthConfig::generate_ephemeral());
            }
            let code_str = self.current_ephemeral.as_deref().unwrap_or("").to_string();
            let visible = self.ephemeral_visible;
            let mut rotate = false;
            let mut toggle_visible = false;
            ui.horizontal(|ui| {
                ui.label("Current ephemeral:");
                if visible {
                    ui.code(&code_str);
                } else {
                    ui.code("••••••••");
                }
                if ui.button(if visible { "Hide" } else { "Show" }).clicked() {
                    toggle_visible = true;
                }
                if ui.button("Rotate").clicked() {
                    rotate = true;
                }
            });
            if toggle_visible {
                self.ephemeral_visible = !self.ephemeral_visible;
            }
            if rotate {
                self.current_ephemeral = Some(HostAuthConfig::generate_ephemeral());
                dirty = true;
            }
            ui.add_space(8.0);
        }

        // ----------------------------------------------------------------
        // Default permissions
        // ----------------------------------------------------------------
        ui.label("Default permissions for new viewers:");
        let p = &mut self.host_auth.default_permissions;
        let prev: PermissionSet = *p;
        ui.checkbox(&mut p.input, "Allow input (keyboard/mouse)");
        ui.checkbox(&mut p.clipboard, "Allow clipboard");
        ui.checkbox(&mut p.file_transfer, "Allow file transfer");
        ui.checkbox(&mut p.audio, "Allow audio");
        if *p != prev {
            dirty = true;
        }

        dirty
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn host_auth_with_pin(pin: &str) -> HostAuthConfig {
        HostAuthConfig {
            mode: AuthMode::Pin,
            pin_hash: Some(HostAuthConfig::hash_pin(pin).unwrap()),
            ..Default::default()
        }
    }

    #[test]
    fn apply_pin_change_requires_correct_current() {
        let mut auth = host_auth_with_pin("correctpass");

        // Wrong current PIN must fail.
        let err = apply_pin_change("wrongpass", "newpassword", &mut auth).unwrap_err();
        assert!(
            matches!(err, WizardError::WrongCurrentPin),
            "expected WrongCurrentPin, got {err}"
        );
        // The hash must NOT have changed.
        assert!(
            auth.verify_pin("correctpass"),
            "original PIN must still work after failed change"
        );

        // Correct current PIN + valid new PIN must succeed.
        apply_pin_change("correctpass", "newpassword", &mut auth).unwrap();
        assert!(
            auth.verify_pin("newpassword"),
            "new PIN must verify after successful change"
        );
        assert!(
            !auth.verify_pin("correctpass"),
            "old PIN must not verify after change"
        );
    }
}
