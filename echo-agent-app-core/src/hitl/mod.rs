//! HITL (Human-in-the-Loop) unification module.
//!
//! Provides a dispatcher that routes approval/input requests to the
//! currently active interface (WebSocket, TUI, REPL, Tauri).

pub mod dispatcher;
pub mod repl_provider;

pub use dispatcher::HitlDispatcher;
pub use repl_provider::ReplHumanLoopProvider;
