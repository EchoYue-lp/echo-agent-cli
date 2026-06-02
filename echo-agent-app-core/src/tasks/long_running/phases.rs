//! Pipeline phase definitions — re-exported from the framework.
//!
//! The authoritative types live in `echo_orchestration::tasks::progress`
//! (exposed via `echo_agent::tasks::progress`). This module re-exports
//! them under the names used throughout this crate.

pub use echo_agent::tasks::progress::{Phase as PipelinePhase, PhasePlan};
