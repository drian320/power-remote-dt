//! Shared GUI infrastructure used by `prdt-gui-host` and `prdt-gui-viewer`.

pub mod config;
pub mod i18n;
pub mod log_tail;
pub mod paths;
pub mod qr;
pub mod style;

pub use config::{Config, ConfigError, GuiConfig, HostConfig, HostEntry, ViewerConfig};
pub use i18n::{
    current_locale, detect_locale, init as init_locale, set_locale, tr, tr_args, FluentValue,
    Locale,
};
pub use log_tail::{TailHandle, TailLayer};
pub use paths::{config_root, default_config_path};
pub use qr::generate as generate_qr;
pub use style::install_jp_font;
