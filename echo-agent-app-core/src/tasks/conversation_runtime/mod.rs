//! Unified conversation runtime events — shared by normal chat and TaskRuntime.
//!
//! Both paths emit the same `ConversationRuntimeEvent` variants, so the frontend
//! can render one coherent timeline instead of two separate rendering pipelines.

pub mod types;
pub use types::*;
