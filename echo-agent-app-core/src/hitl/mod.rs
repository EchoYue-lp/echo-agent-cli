//! HITL (Human-in-the-Loop) unification module.
//!
//! Provides a dispatcher that routes approval/input requests to the
//! currently active interface (WebSocket, TUI, REPL, Tauri).

pub mod channel_provider;
pub mod dispatcher;
pub mod repl_provider;
pub mod tui_provider;

pub use channel_provider::{ChannelHumanLoopProvider, ChannelHumanLoopResolution};
pub use dispatcher::HitlDispatcher;
pub use repl_provider::ReplHumanLoopProvider;
pub use tui_provider::{PendingApproval, PendingHumanLoopKind, TuiHumanLoopProvider};
