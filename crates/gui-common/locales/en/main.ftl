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
