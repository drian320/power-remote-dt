//! Media Foundation-based H.265 hardware decoder.
//!
//! Uses the system's MFT (Media Foundation Transform) with a D3D11 device
//! manager for GPU acceleration via DXVA2/D3D11VA. Exposes a small
//! [`H265Decoder`] type that wraps the MFT lifecycle plus the
//! `process_input`/`process_output` loop.

pub mod decoder;

pub use decoder::H265Decoder;
