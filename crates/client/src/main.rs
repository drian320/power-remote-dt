//! `prdt` — unified CLI dispatcher.
//!
//! Subcommands forward all trailing args verbatim to the underlying lib
//! entrypoints, so `prdt host --bind 0.0.0.0:9000 ...` is identical in behavior
//! to running the legacy `prdt-host.exe --bind 0.0.0.0:9000 ...`. The bin
//! shims for the legacy names remain for one release as a compat layer.

use std::ffi::OsString;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "prdt",
    about = "power-remote-dt unified client",
    disable_help_subcommand = true
)]
struct Cli {
    /// Internal: after an elevated relaunch (Windows host-only UAC), auto-start
    /// the host listener in the GUI. Hidden; set by the self-elevation path.
    #[arg(long, hide = true)]
    host_autostart: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run as host (capture + encode + serve).
    Host {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Connect to a host as viewer.
    Connect {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Compatibility alias for `connect`.
    Viewer {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

/// Windows + Linux dispatcher. As of GUI modernization P2 the no-subcommand
/// path opens the unified launcher GUI (RustDesk-style: one window, "This
/// Device" + "Connect" tabs) on **both** OSes — the egui/eframe stack is
/// cross-platform, so the Linux "deferred to L2" bail is gone.
#[cfg(any(windows, target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    use clap::Parser as _;

    let cli = Cli::parse();
    match cli.cmd {
        // No subcommand → unified GUI.
        None => prdt_gui_client::run_client_gui(None, cli.host_autostart),
        Some(Cmd::Host { args }) => {
            let argv = std::iter::once(OsString::from("prdt-host")).chain(args);
            let host_args = prdt_host::parse_args_with_config(argv);
            prdt_host::run_with_args(host_args)
        }
        Some(Cmd::Connect { args }) | Some(Cmd::Viewer { args }) => {
            let argv = std::iter::once(OsString::from("prdt-viewer")).chain(args);
            let viewer_args = prdt_viewer::parse_args_with_config(argv);
            prdt_viewer::run_with_args(viewer_args)
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("prdt currently only supports Windows and Linux hosts");
}
