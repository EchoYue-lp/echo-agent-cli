//! Human checkpoint gate — re-exported from the framework.
//!
//! The authoritative types live in `echo_orchestration::tasks::human_gate`
//! (exposed via `echo_agent::tasks::human_gate`). This module re-exports
//! them under the names used throughout this crate.

pub use echo_agent::tasks::human_gate::{
    HumanGate as HumanCheckpointGate, HumanRequest as HumanCheckpointRequest,
    HumanResponse as HumanCheckpointResponse,
};
