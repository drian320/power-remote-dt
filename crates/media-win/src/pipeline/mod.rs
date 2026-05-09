//! Concrete `VideoProducer` / `VideoConsumer` implementations wiring together
//! the DXGI capture, NVENC encode, and Media Foundation decode paths.

pub mod consumer;
pub mod producer;

pub use consumer::MfD3d11Consumer;
pub use producer::DxgiNvencProducer;
