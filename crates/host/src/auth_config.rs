//! Host-side auth + permissions configuration (P6).
//!
//! The types live in `prdt-gui-common::auth_config` so that `prdt-gui-host`
//! can consume them without creating a dependency cycle.  This module
//! re-exports everything so existing callers inside `prdt-host` are unchanged.

pub use prdt_gui_common::auth_config::{AuthMode, HostAuthConfig, HostAuthConfigError};
