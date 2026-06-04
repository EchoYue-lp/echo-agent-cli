//! Human checkpoint types — compatibility aliases for the unified HumanLoopProvider.
//!
//! The legacy `HumanGate` has been replaced by `HumanLoopProvider` with the
//! `Selection` kind. These aliases preserve existing import paths.

pub use echo_agent::human_loop::{
    HumanLoopRequest as HumanCheckpointRequest, HumanLoopResponse as HumanCheckpointResponse,
};
