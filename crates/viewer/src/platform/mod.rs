//! Platform-specific viewer backends. cfg-aliased re-exports give lib.rs
//! a single OS-transparent symbol set. Mirrors crates/host/src/platform/mod.rs.

// Stub — fleshed out in T6.

#[cfg(windows)]
pub mod win;

#[cfg(target_os = "linux")]
pub mod linux;

pub mod input_map;
