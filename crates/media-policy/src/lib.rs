//! Capability/Policy layer for the prdt media pipeline.
//!
//! This crate enumerates encoder backends (`CapabilityProbe`), ranks them
//! against runtime context (`SelectionPolicy`), watches encode performance
//! for degradation or device loss (`HealthMonitor`), constructs them
//! (`ProducerFactory`), and presents the result to host code as a single
//! `Box<dyn VideoProducer>` (`PolicyDriven`).
//!
//! See `docs/superpowers/specs/2026-05-11-p5a-capability-policy-design.md`
//! for the full design.

// Module shells; populated in T2-T6.
pub mod capability;
pub mod driver;
pub mod factory;
pub mod health;
pub mod selection;

// Re-exports for ergonomic consumer use:
pub use capability::{BackendKind, CapabilityProbe, Codec, EncoderCapability};
pub use driver::PolicyDriven;
pub use factory::{FactoryError, ProducerConfig, ProducerFactory};
pub use health::{FailoverReason, HealthAction, HealthMonitor, HealthState};
pub use selection::{
    BackendStats, HistoryTable, PolicyContext, ScoringPolicy, ScoringWeights, SelectionPolicy,
};
