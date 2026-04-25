//! Shared GUI infrastructure used by `prdt-gui-host` and `prdt-gui-viewer`.

pub mod config;
pub mod log_tail;
pub mod paths;
pub mod qr;
pub mod style;

pub use config::{Config, ConfigError, HostConfig, HostEntry, ViewerConfig};
pub use log_tail::{TailHandle, TailLayer};
pub use paths::{config_root, default_config_path};
pub use qr::generate as generate_qr;
pub use style::install_jp_font;
