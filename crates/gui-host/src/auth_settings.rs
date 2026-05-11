//! Auth settings panel for the host GUI (P6 T7).
//!
//! Mounted inside the Settings window. Allows changing AuthMode, PIN,
//! default permissions, and managing saved peers (read + delete + persist).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use prdt_crypto::known_peers::KnownPeers;
use prdt_gui_common::{auth_config::HostAuthConfig, AuthMode};
use prdt_protocol::PermissionSet;
use tracing::warn;

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
// Testable submit handlers
// ---------------------------------------------------------------------------

/// Change the PIN on an existing `HostAuthConfig`.
///
/// Requires the correct current PIN before accepting the new one.
/// - Returns `WizardError::WrongCurrentPin` if `current_pin` is wrong.
/// - Returns `WizardError::PinUnchanged` if `new_pin == current_pin`.
/// - Returns `WizardError::PinTooShort` if `new_pin` is < 6 chars.
pub fn apply_pin_change(
    current_pin: &str,
    new_pin: &str,
    host_auth: &mut HostAuthConfig,
) -> Result<(), WizardError> {
    if !host_auth.verify_pin(current_pin) {
        return Err(WizardError::WrongCurrentPin);
    }
    if new_pin == current_pin {
        return Err(WizardError::PinUnchanged);
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
///
/// Covers:
/// - AuthMode radio (Tofu / Pin / Ephemeral)
/// - PIN change modal (requires current PIN before accepting new PIN)
/// - Ephemeral code display + Rotate / Show-Hide
/// - Default permissions toggles
/// - Saved peers list (read + delete + persist)
pub struct AuthSettingsState {
    /// Working copy of the auth config; caller must flush to disk on save.
    pub host_auth: HostAuthConfig,
    pub pin_edit_mode: PinEditMode,
    pub pin_error: Option<String>,
    /// Ephemeral code currently shown (only meaningful in Ephemeral mode).
    pub current_ephemeral: Option<String>,
    ephemeral_visible: bool,
    /// P6 T7: in-memory copy of the known-peers store. Mutations are flushed
    /// to disk immediately when the user clicks Delete.
    pub known_peers: KnownPeers,
    /// Path to host-peers.toml for persistence.
    pub known_peers_path: PathBuf,
    /// Error from the last known-peers save attempt.
    pub peers_save_error: Option<String>,
}

impl AuthSettingsState {
    pub fn new(host_auth: HostAuthConfig, known_peers_path: PathBuf) -> Self {
        let known_peers = KnownPeers::load_or_default(&known_peers_path).unwrap_or_default();
        Self {
            host_auth,
            pin_edit_mode: PinEditMode::Idle,
            pin_error: None,
            current_ephemeral: None,
            ephemeral_visible: false,
            known_peers,
            known_peers_path,
            peers_save_error: None,
        }
    }

    /// Render the auth settings UI inside an existing `ui` layout.
    ///
    /// Returns `true` if the caller should flush `self.host_auth` to disk
    /// (i.e., the user changed something and confirmed).
    ///
    /// Reloads the known-peers list from disk on every call so that peers
    /// accepted by a concurrently-running host task appear immediately without
    /// needing to reopen the Settings panel.
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let mut dirty = false;

        // Refresh peers from disk each frame. Settings panels are infrequently
        // opened so the I/O cost is negligible and the list stays current.
        self.known_peers =
            KnownPeers::load_or_default(&self.known_peers_path).unwrap_or_else(|e| {
                warn!(error = %e, path = %self.known_peers_path.display(), "failed to reload known peers");
                std::mem::take(&mut self.known_peers)
            });

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
            // Use an action enum to avoid holding a mutable borrow on
            // self.pin_edit_mode across the egui button calls.
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

        ui.add_space(12.0);
        ui.separator();

        // ----------------------------------------------------------------
        // Saved peers list
        // ----------------------------------------------------------------
        ui.heading("Saved Peers");
        ui.add_space(4.0);

        if self.known_peers.peers.is_empty() {
            ui.label("No saved peers yet.");
        } else {
            // Collect deletions outside the iteration to avoid mid-iteration mutation.
            let mut delete_pubkey: Option<String> = None;

            for peer in &self.known_peers.peers {
                ui.horizontal(|ui| {
                    // Label + truncated pubkey
                    let label = if peer.label.is_empty() {
                        "<unnamed>".to_string()
                    } else {
                        peer.label.clone()
                    };
                    let short_key = peer.pubkey_b64.chars().take(12).collect::<String>();
                    ui.label(format!("{label}  ({short_key}…)"));

                    // Permission icons (compact)
                    let perm_str = format!(
                        "{}{}{}{}",
                        if peer.permissions.input { "K" } else { "-" },
                        if peer.permissions.clipboard { "C" } else { "-" },
                        if peer.permissions.file_transfer {
                            "F"
                        } else {
                            "-"
                        },
                        if peer.permissions.audio { "A" } else { "-" },
                    );
                    ui.code(perm_str);

                    // Last-seen relative time
                    let age = relative_age(peer.last_seen_at);
                    ui.label(age);

                    if ui.button("Delete").clicked() {
                        delete_pubkey = Some(peer.pubkey_b64.clone());
                    }
                });
            }

            if let Some(pk) = delete_pubkey {
                self.known_peers.remove_by_pubkey(&pk);
                match self.known_peers.save(&self.known_peers_path) {
                    Ok(()) => self.peers_save_error = None,
                    Err(e) => {
                        warn!(
                            error = %e,
                            path = %self.known_peers_path.display(),
                            "failed to save host-peers.toml after Delete"
                        );
                        self.peers_save_error = Some(format!("Save failed: {e}"));
                    }
                }
            }

            if let Some(err) = &self.peers_save_error {
                ui.colored_label(egui::Color32::RED, err);
            }
        }

        dirty
    }
}

/// Format a `SystemTime` as a human-readable relative age string.
fn relative_age(t: SystemTime) -> String {
    match SystemTime::now().duration_since(t) {
        Ok(d) => {
            let secs = d.as_secs();
            if secs < 60 {
                "just now".into()
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86400 {
                format!("{}h ago", secs / 3600)
            } else {
                format!("{}d ago", secs / 86400)
            }
        }
        // `t` is in the future or equals UNIX_EPOCH (default / unknown).
        Err(_) => {
            if t == UNIX_EPOCH {
                "unknown".into()
            } else {
                "just now".into()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use prdt_crypto::known_peers::KnownPeer;

    fn host_auth_with_pin(pin: &str) -> HostAuthConfig {
        HostAuthConfig {
            mode: AuthMode::Pin,
            pin_hash: Some(HostAuthConfig::hash_pin(pin).unwrap()),
            ..Default::default()
        }
    }

    fn make_peer(pubkey: &str, label: &str) -> KnownPeer {
        KnownPeer {
            pubkey_b64: pubkey.into(),
            label: label.into(),
            permissions: prdt_protocol::PermissionSet::all(),
            first_seen_at: UNIX_EPOCH,
            last_seen_at: UNIX_EPOCH,
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
        // Hash must NOT have changed.
        assert!(
            auth.verify_pin("correctpass"),
            "original PIN must still work after failed change"
        );

        // Correct current + valid new PIN must succeed.
        apply_pin_change("correctpass", "newpassword", &mut auth).unwrap();
        assert!(auth.verify_pin("newpassword"), "new PIN must verify");
        assert!(!auth.verify_pin("correctpass"), "old PIN must not verify");
    }

    #[test]
    fn apply_pin_change_rejects_unchanged() {
        let mut auth = host_auth_with_pin("samepassword");
        let err = apply_pin_change("samepassword", "samepassword", &mut auth).unwrap_err();
        assert!(
            matches!(err, WizardError::PinUnchanged),
            "expected PinUnchanged, got {err}"
        );
        // Hash must be untouched.
        assert!(auth.verify_pin("samepassword"));
    }

    #[test]
    fn known_peers_delete_removes_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let peers_path = dir.path().join("host-peers.toml");

        // Seed the file with two peers.
        let mut initial = KnownPeers::default();
        initial.peers.push(make_peer("AAAA", "alice"));
        initial.peers.push(make_peer("BBBB", "bob"));
        initial.save(&peers_path).unwrap();

        // Load into AuthSettingsState and delete alice.
        let mut state = AuthSettingsState::new(HostAuthConfig::default(), peers_path.clone());
        assert_eq!(state.known_peers.peers.len(), 2);
        state.known_peers.remove_by_pubkey("AAAA");
        state.known_peers.save(&state.known_peers_path).unwrap();

        // Reload from disk — only bob should remain.
        let reloaded = KnownPeers::load_or_default(&peers_path).unwrap();
        assert_eq!(reloaded.peers.len(), 1);
        assert_eq!(reloaded.peers[0].pubkey_b64, "BBBB");
    }
}
