//! First-run onboarding wizard for the host GUI (P6 T7).
//!
//! The wizard is shown when `config.gui.onboarded == false`.  It walks the
//! user through choosing an AuthMode, optionally setting a PIN, and picking
//! default permissions.  When the user clicks Finish the wizard calls
//! `apply_wizard`, saves both configs, and sets `onboarded = true`.
//!
//! The *submit handler* (`apply_wizard`) is pure logic tested headlessly;
//! the egui rendering (`WizardState::show`) is verified only by manual smoke
//! (T9).

use prdt_gui_common::{auth_config::HostAuthConfig, AuthMode, Config};
use prdt_protocol::PermissionSet;

// ---------------------------------------------------------------------------
// Submission types
// ---------------------------------------------------------------------------

/// All data the wizard collects; passed to `apply_wizard` when Finish is
/// clicked.
#[derive(Debug, Clone)]
pub struct WizardSubmission {
    pub mode: AuthMode,
    /// `None` when `mode != Pin`.
    pub pin_plain: Option<String>,
    pub default_permissions: PermissionSet,
}

/// Errors that can occur when applying the wizard result or a PIN change.
#[derive(Debug, thiserror::Error)]
pub enum WizardError {
    #[error("PIN is required when AuthMode::Pin is selected")]
    PinRequired,
    #[error("PIN must be at least 6 characters")]
    PinTooShort,
    #[error("New PIN must differ from the current PIN")]
    PinUnchanged,
    #[error("Wrong current PIN")]
    WrongCurrentPin,
    #[error("PIN hashing failed: {0}")]
    BcryptError(#[from] bcrypt::BcryptError),
}

// ---------------------------------------------------------------------------
// Testable submit handler
// ---------------------------------------------------------------------------

/// Apply a completed wizard submission to `config` and `host_auth`.
///
/// On success:
/// - `host_auth.mode` is updated.
/// - `host_auth.pin_hash` is set (Pin mode) or left as-is (other modes).
/// - `host_auth.default_permissions` is updated.
/// - `config.gui.onboarded` is set to `true`.
pub fn apply_wizard(
    submission: WizardSubmission,
    config: &mut Config,
    host_auth: &mut HostAuthConfig,
) -> Result<(), WizardError> {
    host_auth.mode = submission.mode;
    if submission.mode == AuthMode::Pin {
        let pin = submission.pin_plain.ok_or(WizardError::PinRequired)?;
        if pin.len() < 6 {
            return Err(WizardError::PinTooShort);
        }
        host_auth.pin_hash = Some(HostAuthConfig::hash_pin(&pin)?);
    }
    host_auth.default_permissions = submission.default_permissions;
    config.gui.onboarded = true;
    Ok(())
}

// ---------------------------------------------------------------------------
// Wizard step enum + state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    Welcome,
    AuthMode,
    /// Only entered when `selected_mode == AuthMode::Pin`.
    PinSetup,
    Defaults,
    Done,
}

/// Mutable state for the multi-step wizard window.
pub struct WizardState {
    pub step: WizardStep,
    pub selected_mode: AuthMode,
    pub pin_input: String,
    pub pin_confirm: String,
    pub permissions: PermissionSet,
    pub error: Option<String>,
}

impl Default for WizardState {
    fn default() -> Self {
        Self::new()
    }
}

impl WizardState {
    pub fn new() -> Self {
        Self {
            step: WizardStep::Welcome,
            selected_mode: AuthMode::Tofu,
            pin_input: String::new(),
            pin_confirm: String::new(),
            permissions: PermissionSet::all(),
            error: None,
        }
    }

    /// Render the wizard modal.  Returns `Some(WizardSubmission)` when the
    /// user clicks Finish; `None` while the wizard is still in progress.
    ///
    /// The caller must call `apply_wizard` with the returned submission and
    /// then save both configs before the next frame.
    pub fn show(&mut self, ctx: &egui::Context, host_id: &str) -> Option<WizardSubmission> {
        let mut result: Option<WizardSubmission> = None;

        egui::Window::new("Setup Wizard")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| match self.step {
                WizardStep::Welcome => {
                    ui.heading("Welcome to Power Remote DT");
                    ui.add_space(8.0);
                    ui.label("This wizard configures authentication and permissions.");
                    ui.add_space(8.0);
                    ui.label(format!("Host ID: {host_id}"));
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if ui.button("Next").clicked() {
                            self.step = WizardStep::AuthMode;
                            self.error = None;
                        }
                        if ui.button("Skip").clicked() {
                            result = Some(WizardSubmission {
                                mode: AuthMode::Tofu,
                                pin_plain: None,
                                default_permissions: PermissionSet::all(),
                            });
                        }
                    });
                }

                WizardStep::AuthMode => {
                    ui.heading("Authentication Mode");
                    ui.add_space(8.0);
                    ui.radio_value(
                        &mut self.selected_mode,
                        AuthMode::Tofu,
                        "TOFU (Trust On First Use) — prompt once, then remember",
                    );
                    ui.radio_value(
                        &mut self.selected_mode,
                        AuthMode::Pin,
                        "PIN — viewers must enter a PIN each connection",
                    );
                    ui.radio_value(
                        &mut self.selected_mode,
                        AuthMode::Ephemeral,
                        "Ephemeral — rotating one-time code (shown on screen)",
                    );
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            self.step = WizardStep::Welcome;
                            self.error = None;
                        }
                        if ui.button("Next").clicked() {
                            self.error = None;
                            if self.selected_mode == AuthMode::Pin {
                                self.step = WizardStep::PinSetup;
                            } else {
                                self.step = WizardStep::Defaults;
                            }
                        }
                        if ui.button("Skip").clicked() {
                            result = Some(WizardSubmission {
                                mode: AuthMode::Tofu,
                                pin_plain: None,
                                default_permissions: PermissionSet::all(),
                            });
                        }
                    });
                }

                WizardStep::PinSetup => {
                    ui.heading("Set PIN");
                    ui.add_space(8.0);
                    ui.label("PIN (min 6 characters):");
                    ui.add(egui::TextEdit::singleline(&mut self.pin_input).password(true));
                    ui.label("Confirm PIN:");
                    ui.add(egui::TextEdit::singleline(&mut self.pin_confirm).password(true));
                    if let Some(err) = &self.error {
                        ui.colored_label(egui::Color32::RED, err);
                    }
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            self.step = WizardStep::AuthMode;
                            self.error = None;
                        }
                        if ui.button("Next").clicked() {
                            if self.pin_input.len() < 6 {
                                self.error = Some("PIN must be at least 6 characters.".into());
                            } else if self.pin_input != self.pin_confirm {
                                self.error = Some("PINs do not match.".into());
                            } else {
                                self.step = WizardStep::Defaults;
                                self.error = None;
                            }
                        }
                    });
                }

                WizardStep::Defaults => {
                    ui.heading("Default Permissions");
                    ui.add_space(8.0);
                    ui.label("These apply to new viewers before you customise per-peer:");
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.permissions.input, "Allow input (keyboard/mouse)");
                    ui.checkbox(&mut self.permissions.clipboard, "Allow clipboard");
                    ui.checkbox(&mut self.permissions.file_transfer, "Allow file transfer");
                    ui.checkbox(&mut self.permissions.audio, "Allow audio");
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            self.error = None;
                            if self.selected_mode == AuthMode::Pin {
                                self.step = WizardStep::PinSetup;
                            } else {
                                self.step = WizardStep::AuthMode;
                            }
                        }
                        if ui.button("Next").clicked() {
                            self.step = WizardStep::Done;
                            self.error = None;
                        }
                    });
                }

                WizardStep::Done => {
                    ui.heading("All set!");
                    ui.add_space(8.0);
                    ui.label("Your host is configured. Click Finish to save and start.");
                    if let Some(err) = &self.error {
                        ui.colored_label(egui::Color32::RED, err);
                    }
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            self.step = WizardStep::Defaults;
                            self.error = None;
                        }
                        if ui.button("Finish").clicked() {
                            let pin_plain = if self.selected_mode == AuthMode::Pin {
                                Some(self.pin_input.clone())
                            } else {
                                None
                            };
                            result = Some(WizardSubmission {
                                mode: self.selected_mode,
                                pin_plain,
                                default_permissions: self.permissions,
                            });
                        }
                    });
                }
            });

        result
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_perms() -> PermissionSet {
        PermissionSet::all()
    }

    #[test]
    fn apply_wizard_writes_pin_hash() {
        let mut config = Config::default();
        let mut host_auth = HostAuthConfig::default();
        let submission = WizardSubmission {
            mode: AuthMode::Pin,
            pin_plain: Some("correct-horse".into()),
            default_permissions: default_perms(),
        };
        apply_wizard(submission, &mut config, &mut host_auth).unwrap();
        assert!(host_auth.pin_hash.is_some(), "pin_hash should be set");
        assert!(
            host_auth.verify_pin("correct-horse"),
            "verify_pin should succeed with the submitted PIN"
        );
        assert!(
            config.gui.onboarded,
            "onboarded should be true after wizard"
        );
    }

    #[test]
    fn apply_wizard_pin_too_short_rejects() {
        let mut config = Config::default();
        let mut host_auth = HostAuthConfig::default();
        let submission = WizardSubmission {
            mode: AuthMode::Pin,
            pin_plain: Some("abc".into()),
            default_permissions: default_perms(),
        };
        let err = apply_wizard(submission, &mut config, &mut host_auth).unwrap_err();
        assert!(
            matches!(err, WizardError::PinTooShort),
            "expected PinTooShort, got {err}"
        );
        assert!(
            !config.gui.onboarded,
            "onboarded must stay false when wizard fails"
        );
    }

    #[test]
    fn apply_wizard_tofu_skips_pin() {
        let mut config = Config::default();
        let mut host_auth = HostAuthConfig::default();
        let custom_perms = PermissionSet {
            input: false,
            clipboard: true,
            file_transfer: false,
            audio: true,
        };
        let submission = WizardSubmission {
            mode: AuthMode::Tofu,
            pin_plain: None,
            default_permissions: custom_perms,
        };
        apply_wizard(submission, &mut config, &mut host_auth).unwrap();
        assert_eq!(host_auth.mode, AuthMode::Tofu);
        assert!(
            host_auth.pin_hash.is_none(),
            "pin_hash must stay None for Tofu mode"
        );
        assert_eq!(
            host_auth.default_permissions, custom_perms,
            "custom permissions must be applied"
        );
        assert!(
            config.gui.onboarded,
            "onboarded should be true after wizard"
        );
    }
}
