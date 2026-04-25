//! Shared GUI infrastructure used by `prdt-gui-host` and `prdt-gui-viewer`.

pub mod config;
pub mod paths;

pub use config::{Config, ConfigError, HostConfig, HostEntry, ViewerConfig};
pub use paths::{config_root, default_config_path};
