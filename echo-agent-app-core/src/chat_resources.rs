//! Per-turn chat resources scoped into a tokio task_local so agent tools —
//! chiefly `create_complex_task` (Phase B3) — can reach the pool/store/sink
//! during a chat turn without them being threaded through the framework's tool
//! plumbing.
//!
//! `drive_chat` scopes an `Arc<ChatResources>` per turn; tools read it via
//! [`current_chat_resources`]. `pool`/`store` are `Option` because some
//! contexts (tests, a mode without a pool) lack them — `create_complex_task`
//! requires `Some(pool)` and `Some(store)` and errors otherwise.
use std::sync::Arc;

use echo_agent::agent::CancellationToken;
use echo_agent::evolution::MemoryLayerManager;

use crate::agent_pool::AgentPool;
use crate::attachments::AttachmentRef;
use crate::chat_driver::ChatSink;
use crate::tasks::task_runtime::store::TaskRuntimeStore;

/// Everything an agent may need during one chat turn. Built by the caller
/// (TUI `handle_enter`, channel `handle_stream`, GUI) and scoped by
/// `drive_chat` via [`with_chat_resources`].
pub struct ChatResources {
    /// Pool for acquiring an isolated agent per background run. `None` in
    /// contexts without a pool (tests); `create_complex_task` errors then.
    pub pool: Option<Arc<AgentPool>>,
    /// TaskRuntimeStore for create_run / cancel_run. `None` when unavailable.
    pub store: Option<Arc<TaskRuntimeStore>>,
    pub sink: Arc<dyn ChatSink>,
    /// Shared product webhook emitter. Event delivery is observed in
    /// `drive_chat`, so GUI/TUI/CLI/channel use identical lifecycle semantics.
    pub webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    pub conv_id: Option<String>,
    pub root_message_id: String,
    pub attachments: Vec<AttachmentRef>,
    /// The chat turn's cancel token. Foreground runs may share it; background
    /// runs MUST use an independent token (spec §5.5) — never this one.
    pub cancel: CancellationToken,
    /// Per-turn mode hint prepended to the user message by `drive_chat`
    /// (spec §8 / B4.3): e.g. Chat → "do NOT call create_complex_task, reply
    /// directly"; Task → "lean towards create_complex_task for complex work".
    /// None (Auto) adds nothing. Pure prompt — NO code route branch.
    pub mode_hint: Option<String>,
    /// The interaction mode for this turn (Chat/Task/Auto). P1.1: when this is
    /// `Chat`, `drive_chat` hides task-management tools through the invocation
    /// context so the model physically cannot call them — not just a prompt
    /// hint. Default `Auto` for callers that don't set it.
    pub interaction_mode: crate::tasks::task_runtime::InteractionMode,
    /// Memory layer manager for durable write-back of completed runs
    /// (B5.1): `create_complex_task` forwards this into the background Run's
    /// payload so `execute_run` (Blocking policy) lands the
    /// `taskrun:completed:{run_id}` memory before returning. `None` when the
    /// caller has no review/memory subsystem wired (minimal tests/embedders) — the
    /// write becomes a no-op.
    pub layer_manager: Option<Arc<MemoryLayerManager>>,
}

tokio::task_local! {
    /// The chat resources for the current turn, if any. `None` outside a
    /// `drive_chat` scope (e.g. cron, tests).
    static CURRENT_CHAT_RESOURCES: Arc<ChatResources>;
}

/// Run `f` with `res` scoped as the current chat resources. Async because the
/// scope spans `.await` points in `drive_chat`'s stream loop and any tool the
/// agent calls mid-ReAct.
pub async fn with_chat_resources<R>(
    res: Arc<ChatResources>,
    f: impl std::future::Future<Output = R>,
) -> R {
    CURRENT_CHAT_RESOURCES.scope(res, f).await
}

/// Read the current chat resources (set by `drive_chat`). `None` outside a
/// chat turn. Sync-callable so tools can read it without an async context.
pub fn current_chat_resources() -> Option<Arc<ChatResources>> {
    CURRENT_CHAT_RESOURCES.try_with(|r| r.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn current_chat_resources_is_none_outside_scope() {
        assert!(current_chat_resources().is_none());
    }
}
