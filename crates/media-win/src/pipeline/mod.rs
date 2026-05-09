//! Concrete `VideoProducer` / `VideoConsumer` implementations wiring together
//! the DXGI capture, NVENC encode, and Media Foundation decode paths.

pub mod consumer;
#[cfg(prdt_nvenc_bindings)]
pub mod producer;

pub use consumer::MfD3d11Consumer;
#[cfg(prdt_nvenc_bindings)]
pub use producer::DxgiNvencProducer;
