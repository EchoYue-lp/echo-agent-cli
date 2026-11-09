//! HITL (Human-in-the-Loop) unification module.
//!
//! Provides a dispatcher that routes approval/input requests to the
//! 当前GUI与IM渠道；dispatcher、配对身份和请求结算保持原权威。

pub mod channel_entry;
pub mod channel_provider;
pub mod dispatcher;
pub mod pairing;

pub use channel_provider::{ChannelHumanLoopProvider, ChannelHumanLoopResolution};
pub use dispatcher::{HitlDispatcher, HitlProviderRegistration};
pub use pairing::{
    HitlRequestIdentity, HitlRequestKind, HitlRequestScope, HitlScopeResolver,
    PairedHumanLoopProvider, PairedHumanLoopRequest, pair_request,
};

pub use channel_entry::{ChannelGuiHitlFactory, ChannelGuiHitlFactorySlot, ChannelHumanLoopEntry};
