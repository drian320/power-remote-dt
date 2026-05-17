//! Concrete `VideoProducer` / `VideoConsumer` implementations wiring together
//! the DXGI capture, NVENC encode, and Media Foundation decode paths.

pub mod consumer;
pub mod producer;

pub use consumer::MfD3d11Consumer;
#[cfg(feature = "media-win-hevc-main10")]
pub use consumer::MfHevcMain10Consumer;
pub use producer::DxgiNvencProducer;
