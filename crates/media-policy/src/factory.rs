//! Producer factory trait. OS-specific factory impls live in backend crates.
//!
//! All errors during `create()` collapse into `FactoryError`. Once a producer
//! is constructed, runtime errors flow through `ProducerError` (defined in
//! `prdt-protocol`).

use crate::capability::{BackendKind, Codec};
use prdt_protocol::VideoProducer;

#[derive(Debug, thiserror::Error)]
pub enum FactoryError {
    #[error("backend {0:?} unavailable: {1}")]
    Unavailable(BackendKind, String),
    #[error("config invalid for backend {0:?}: {1}")]
    InvalidConfig(BackendKind, String),
}

#[derive(Debug, Clone)]
pub struct ProducerConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub initial_bitrate_bps: u32,
    pub codec: Codec,
}

pub trait ProducerFactory: Send + Sync {
    fn create(
        &self,
        kind: BackendKind,
        cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use prdt_protocol::{EncodedFrame, ProducerError, VideoProducer};
    use std::sync::Mutex;

    /// Deterministic in-memory producer used in this file's unit tests.
    /// Integration tests in `tests/` and downstream tasks (T6) define their
    /// own scripted producer inline (the `cfg(test)` module is not reachable
    /// across crate boundaries).
    pub struct ScriptedProducer {
        pub name: &'static str,
    }

    #[async_trait]
    impl VideoProducer for ScriptedProducer {
        async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
            Err(ProducerError::Other("scripted: not used in factory test".into()))
        }
        fn request_idr(&mut self) {}
        fn set_target_bitrate(&mut self, _bps: u32) {}
        fn backend_name(&self) -> &'static str { self.name }
    }

    /// Factory that returns one ScriptedProducer per call, recording every
    /// (kind, width, height, fps, bps) invocation.
    pub struct MockFactory {
        #[allow(clippy::type_complexity)]
        pub calls: Mutex<Vec<(BackendKind, u32, u32, u32, u32)>>,
    }

    impl ProducerFactory for MockFactory {
        fn create(
            &self,
            kind: BackendKind,
            cfg: &ProducerConfig,
        ) -> Result<Box<dyn VideoProducer>, FactoryError> {
            self.calls.lock().unwrap().push((
                kind, cfg.width, cfg.height, cfg.fps, cfg.initial_bitrate_bps,
            ));
            let name = match kind {
                BackendKind::Nvenc => "nvenc-mock",
                BackendKind::MfHevc => "mf-mock",
                BackendKind::Openh264 => "openh264-mock",
            };
            Ok(Box::new(ScriptedProducer { name }))
        }
    }

    #[test]
    fn mock_factory_records_call() {
        let f = MockFactory { calls: Mutex::new(vec![]) };
        let cfg = ProducerConfig {
            width: 1920, height: 1080, fps: 60,
            initial_bitrate_bps: 8_000_000, codec: Codec::H265,
        };
        let prod = f.create(BackendKind::Nvenc, &cfg).unwrap();
        assert_eq!(prod.backend_name(), "nvenc-mock");

        let calls = f.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], (BackendKind::Nvenc, 1920, 1080, 60, 8_000_000));
    }

    #[test]
    fn factory_error_display() {
        let e = FactoryError::Unavailable(BackendKind::Nvenc, "no NVIDIA driver".into());
        assert_eq!(e.to_string(), "backend Nvenc unavailable: no NVIDIA driver");
        let e2 = FactoryError::InvalidConfig(BackendKind::Openh264, "fps=0".into());
        assert_eq!(e2.to_string(), "config invalid for backend Openh264: fps=0");
    }
}
