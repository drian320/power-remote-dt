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

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use clap::Parser as _;

    match Cli::parse().cmd {
        // No subcommand → unified GUI (RustDesk-style: one window, two tabs).
        None => prdt_gui_client::run_client_gui(None),
        Some(Cmd::Host { args }) => {
            let argv = std::iter::once(OsString::from("prdt-host")).chain(args);
            let host_args = prdt_host::Args::parse_from(argv);
            prdt_host::run_with_args(host_args)
        }
        Some(Cmd::Connect { args }) | Some(Cmd::Viewer { args }) => {
            let argv = std::iter::once(OsString::from("prdt-viewer")).chain(args);
            let viewer_args = prdt_viewer::Args::parse_from(argv);
            prdt_viewer::run_with_args(viewer_args)
        }
    }
}

/// Linux dispatcher: only `prdt host` works on Linux for L1.5a. The GUI
/// (`None`) and viewer paths remain Windows-only — they're deferred to
/// L2 per the plan §3 scope. Invoking either on Linux exits with an
/// informative non-zero status.
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use clap::Parser as _;

    match Cli::parse().cmd {
        None => Err(anyhow::anyhow!(
            "GUI mode is not yet implemented on Linux (deferred to L2). \
             Use `prdt host` to run the Linux host CLI."
        )),
        Some(Cmd::Host { args }) => {
            let argv = std::iter::once(OsString::from("prdt-host")).chain(args);
            let host_args = prdt_host::Args::parse_from(argv);
            prdt_host::run_with_args(host_args)
        }
        Some(Cmd::Connect { .. }) | Some(Cmd::Viewer { .. }) => Err(anyhow::anyhow!(
            "Viewer mode is not yet implemented on Linux (deferred to L2). \
             Use `prdt host` to run the Linux host CLI."
        )),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("prdt currently only supports Windows and Linux hosts");
}
