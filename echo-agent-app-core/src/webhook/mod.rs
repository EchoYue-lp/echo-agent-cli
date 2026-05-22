//! Webhook 事件回调
//!
//! 非阻塞地向外部 URL 推送事件，支持 HMAC-SHA256 签名。

pub mod emitter;
pub mod events;

pub use emitter::WebhookEmitter;
pub use events::WebhookEvent;
