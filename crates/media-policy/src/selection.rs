// Stub — populated in T4.
use crate::capability::{BackendKind, EncoderCapability};
pub struct PolicyContext {}
pub struct HistoryTable {}
pub struct BackendStats {}
pub struct ScoringWeights {}
pub struct ScoringPolicy {}
pub trait SelectionPolicy: Send + Sync {
    fn rank(&self, candidates: &[EncoderCapability], ctx: &PolicyContext, history: &HistoryTable)
        -> Vec<BackendKind>;
}
