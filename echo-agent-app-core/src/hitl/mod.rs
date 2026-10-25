//! HITL (Human-in-the-Loop) unification module.
//!
//! Provides a dispatcher that routes approval/input requests to the
//! currently active interface (WebSocket, TUI, REPL, Tauri).

pub mod channel_entry;
pub mod channel_provider;
pub mod dispatcher;
pub mod pairing;
pub mod repl_provider;
pub mod tui_provider;

pub use channel_provider::{ChannelHumanLoopProvider, ChannelHumanLoopResolution};
pub use dispatcher::{HitlDispatcher, HitlProviderRegistration};
pub use pairing::{
    HitlRequestIdentity, HitlRequestKind, HitlRequestScope, HitlScopeResolver,
    PairedHumanLoopProvider, PairedHumanLoopRequest, pair_request,
};
pub use repl_provider::{PendingReplHumanLoopRequest, ReplHumanLoopProvider};
pub use tui_provider::{
    PendingApproval, PendingApprovalQueue, PendingHumanLoopKind, TuiHumanLoopProvider,
    prune_closed_pending,
};

pub use channel_entry::{ChannelGuiHitlFactory, ChannelGuiHitlFactorySlot, ChannelHumanLoopEntry};
