// Stub — populated in T3.
use crate::capability::BackendKind;
#[derive(Debug, thiserror::Error)]
pub enum FactoryError {
    #[error("unimplemented")]
    Unimplemented,
}
pub struct ProducerConfig {}
pub trait ProducerFactory: Send + Sync {
    fn create(&self, _kind: BackendKind, _cfg: &ProducerConfig)
        -> Result<Box<dyn prdt_protocol::VideoProducer>, FactoryError> {
        Err(FactoryError::Unimplemented)
    }
}
