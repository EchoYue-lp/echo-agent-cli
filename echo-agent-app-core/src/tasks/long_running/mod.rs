//! Long-running task infrastructure.
//!
//! Enables multi-phase pipelines (e.g., paper writing) with:
//! - Checkpoint/resume: persist state after each phase, resume on restart
//! - Progress reporting: real-time percentage, ETA, current phase
//! - Human checkpoints: pause for user input mid-pipeline
//! - Cancellation: graceful shutdown via CancellationToken
//! - Persistence: survives process restart via SQLite

pub mod checkpoint;
pub mod human_gate;
pub mod phases;
pub mod progress;
pub mod runner;

pub use checkpoint::*;
pub use human_gate::*;
pub use phases::*;
pub use progress::*;
pub use runner::*;
