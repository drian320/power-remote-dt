# Common buttons / labels
common-button-cancel = Cancel
common-button-save = Save
common-button-copy = Copy
common-button-browse = Browse

# Host window
host-window-title = Power Remote Desktop — Host
host-welcome-heading = Welcome
host-welcome-body = Generate a host key to start. The key uniquely identifies this machine to viewers.
host-button-generate-key = Generate host key
host-key-file-label = Key file: { $path }
host-status-idle = Status: Idle
host-status-listening = Status: ● Listening on { $bind }
host-button-start-listening = Start listening
host-button-stop = Stop
host-button-settings = Settings…
host-pubkey-label = Public key:
host-recent-activity = Recent activity:
host-error-key-load = key load failed: { $error }
host-error-qr = qr generation failed: { $error }
host-error-config-save = config save failed: { $error }
host-error-autostart = autostart toggle failed: { $error }

# Host settings
host-settings-bind = Bind:
host-settings-monitor = Monitor:
host-settings-bitrate = Bitrate (Mbps):
host-settings-outgoing = Outgoing dir:
host-settings-signaling-optional = Signaling URL (optional):
settings-autostart-label = Auto-start on login

# Viewer launcher
viewer-window-title = Power Remote Desktop — Viewer
viewer-launcher-heading = Saved connections
viewer-no-connections = (no saved connections)
viewer-host-entry = { $label } — { $detail } ({ $mode })
viewer-button-add = + Add new connection
viewer-button-connect = Connect
viewer-button-quit = Quit
viewer-decoder-label = Decoder:
viewer-error-config-save = config save failed: { $error }

# Viewer add-connection form
viewer-form-title = Add Connection
viewer-form-label = Label:
viewer-form-mode = Mode:
viewer-form-mode-direct = Direct
viewer-form-mode-signaling = Signaling
viewer-form-addr = Address (host:port):
viewer-form-host-id = Host ID (e.g. 123-456-789):
viewer-form-pubkey = Public key (base64; leave empty for TOFU):

# Viewer settings
viewer-settings-title = Viewer Settings
viewer-settings-decoder-mf = MF (default)
viewer-settings-decoder-nvdec = NVDEC (zero-copy)
viewer-settings-resolution = Default resolution:
viewer-settings-fps = Default fps:
viewer-settings-recv-dir = Receive directory:
viewer-settings-signaling-url = Signaling URL:

# Settings (shared)
settings-window-title = Settings
settings-language = Language:
settings-language-auto = Auto
settings-language-english = English
settings-language-japanese = 日本語

# Viewer overlay (Phase 4 G2)
overlay-window-title = Power Remote Desktop — Overlay
overlay-host-label = Connected to: { $host }
overlay-stats-latency = Latency
overlay-stats-samples = samples: { $n }
overlay-stats-decoder = Decoder: { $name }
overlay-stats-connecting = Connecting…
overlay-button-resume = Resume
overlay-button-disconnect = Disconnect

# Tray + notifications (Phase 4 G3)
tray-tooltip = PrdtHost
tray-menu-open = Open settings
tray-menu-stop = Stop listening
tray-menu-show-logs = Show logs
tray-menu-quit = Quit
notif-connected = Viewer connected: { $detail }
notif-disconnected = Viewer disconnected
notif-error = Host error: { $detail }

# Auto-update (Phase 4 G4)
update-section-heading = Updates
update-button-check = Check for updates
update-button-install = Install
update-checking = Checking for updates…
update-up-to-date = You are running the latest version.
update-available = Update available: { $version }
update-error = Update check failed: { $error }

# Crash reporter (Phase 4 G5)
crashlog-pending-heading = Last session crashed ({ $n } reports):
crashlog-button-open-folder = Open crashes folder
crashlog-button-acknowledge = Acknowledge all
crashlog-no-pending = No pending crash reports.
crashlog-row-format = { $timestamp }  { $binary }  "{ $message }"

# Unified home (RustDesk-style single screen)
nav-home = Home
nav-settings = Settings
nav-logs = Logs

# Logs view (in-app tail of the rolling GUI log file)
logs-heading = Logs
logs-card-eyebrow = Activity
logs-file-path-label = Log file
logs-empty = No log output yet. Start sharing or connect to generate activity.
logs-no-path = Could not resolve the log directory.

home-identity-error = Failed to initialise device identity: { $error }
home-identity-error-hint = Check the host key file and config directory permissions, then restart.

# This device
home-this-device-title = This device
home-device-id-label = Device ID
home-unprovisioned = Unprovisioned — cannot reach the signaling server
home-unprovisioned-no-url = No signaling server URL set. Enter one below and save to get your ID.
home-signaling-url-label = Signaling server URL
home-button-save-signaling = Save & get ID
home-button-retry = Retry
home-provisioning = Provisioning…
home-provisioned = Provisioning complete.
home-provision-failed = Provisioning failed: { $error }
home-pin-label = PIN
home-pin-show = Show
home-pin-hide = Hide
home-pin-none = No PIN set (auth mode is not PIN).
home-button-regenerate = Regenerate
home-button-generate-pin = Generate PIN
home-fingerprint-label = Key fingerprint
home-fingerprint-hint = Verify this out-of-band with the other device to prevent impersonation.
home-button-show-qr = Show QR
home-button-hide-qr = Hide QR
home-sharing-label = Share this device
home-sharing-on = Sharing
home-sharing-off = Idle
home-button-start-sharing = Start sharing
home-button-stop-sharing = Stop sharing

# Connect to a device
home-connect-title = Connect to a device
home-peer-id-label = Peer ID (9-digit)
home-peer-pin-label = PIN
home-peer-pin-hint = PIN shown on the other device
home-button-connect = Connect
home-button-disconnect = Disconnect
home-connect-need-id = Enter the peer's ID.
home-connect-need-signaling = No signaling server URL set. Enter one in Settings.
home-connect-need-host = Enter a host address (host:port).
home-connect-launched = Launched viewer (pid { $pid }).
home-connect-connecting = Connecting to { $target }… (pid { $pid })
home-connect-active = Session running — { $target } (pid { $pid })
home-connect-disconnected = Disconnected (exited cleanly).
home-connect-failed = The viewer exited abnormally ({ $detail }). Check the peer ID/PIN, signaling settings, and decoder settings.
home-connect-exit-code = exit code { $code }
home-connect-exit-signal = terminated by signal
home-connect-already-active = A session is already running (one outbound session at a time). Disconnect before connecting again.
home-connect-single-session-note = Only one outbound session can be connected at a time.
home-host-elevation-skipped-viewer = Skipped admin elevation to keep the active session; sharing started with normal privileges (input into admin windows such as Task Manager is unavailable — disconnect, then start sharing again to elevate).
home-advanced = Advanced
home-advanced-host = Host host:port (direct mode)
home-advanced-pubkey = Host public key (base64)
home-codec-label = Codec
home-decoder-label = Decoder
home-button-connect-direct = Connect (direct)
home-recent-title = Recent connections
home-recent-empty = (no recent connections)
home-button-connect-short = Connect
home-recent-remove = Remove

# Consent prompt (host-side, unknown-peer approval modal)
consent-heading = Incoming Connection Request
consent-device-key-label = Device key:
consent-label-optional = Label (optional):
consent-permissions-heading = Permissions for this session:
consent-permission-input = Input (keyboard/mouse)
consent-permission-clipboard = Clipboard
consent-permission-file-transfer = File transfer
consent-permission-audio = Audio
consent-remember = Remember this device
consent-auto-deny = Auto-deny in { $seconds }s
consent-deny = Deny (Esc)
consent-allow = Allow
consent-allow-armed-in = Allow in { $seconds }s
