// Stub — populated in T4.
use crate::capability::{BackendKind, EncoderCapability};
pub struct PolicyContext {}
pub struct HistoryTable {}
pub struct BackendStats {}
pub struct ScoringWeights {}
pub struct ScoringPolicy {}
pub trait SelectionPolicy: Send + Sync {
    fn rank(&self, _candidates: &[EncoderCapability], _ctx: &PolicyContext, _history: &HistoryTable)
        -> Vec<BackendKind> { Vec::new() }
}
