//! DXGI-based screen capture. Desktop Duplication API wrapper + Output
//! enumeration.

pub mod duplication;
pub mod output;

pub use duplication::{AcquiredFrame, DesktopDuplication};
pub use output::{enumerate_outputs_for_adapter, OutputInfo};
