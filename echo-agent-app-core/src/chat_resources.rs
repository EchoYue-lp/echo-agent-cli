//! Per-turn chat resources scoped into a tokio task-local for app-owned work
//! on the current async task. Framework-spawned tool execution instead uses
//! value-carried `ToolContext`; Tokio task-locals do not cross that boundary.
//!
//! `drive_chat` scopes an `Arc<ChatResources>` per turn; tools read it via
//! [`current_chat_resources`]. `pool`/`store` are `Option` because some
//! contexts (tests or a surface without a pool) lack them — `create_complex_task`
//! requires `Some(pool)` and `Some(store)` and errors otherwise.
use std::sync::Arc;

use echo_agent::agent::CancellationToken;
use echo_agent::human_loop::HumanLoopProvider;

use crate::agent_pool::AgentPool;
use crate::attachments::AttachmentRef;
use crate::chat_driver::ChatSink;
use crate::tasks::task_runtime::store::TaskRuntimeStore;

/// Everything an agent may need during one chat turn. Built by the caller
/// (TUI `handle_enter`, channel `handle_stream`, GUI) and scoped by
/// `drive_chat` via [`with_chat_resources`].
pub struct ChatResources {
    /// Immutable identity/root for every foreground, pooled, and continuation
    /// operation spawned by this turn.
    pub execution_scope: crate::workspace::WorkspaceExecutionScope,
    /// Exact workspace lifetime and immutable EKO data root retained by every
    /// Agent/tool spawn belonging to this turn. Minimal test fixtures may use
    /// `None`; product surfaces must always provide a receipt.
    pub workspace_io_receipt: Option<crate::state::ScopedWorkspaceIoReceipt>,
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
    /// Canonical source for workspace-bound memory admission. Product
    /// surfaces pass this Arc through unchanged; `drive_chat` acquires the
    /// generation only after foreground admission succeeds.
    pub review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    /// Pins every memory/evidence write initiated by this turn to one shared
    /// generation-bound manager and projection source.
    pub memory_generation: Option<crate::evolution::ReviewGenerationLease>,
    /// Surface-owned approval/input transport. Long-horizon continuation
    /// replays this onto its run-scoped pooled agent for every finite turn.
    pub human_loop_provider: Option<Arc<dyn HumanLoopProvider>>,
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
