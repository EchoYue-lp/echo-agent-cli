//! Compatibility path for the original shared channel implementation.
#[cfg(feature = "channels")]
pub use echo_agent_app_core::api::channel_runtime::{ChannelsModeArgs, run_channels_mode};
