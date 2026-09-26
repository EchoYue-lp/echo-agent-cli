//! Session management slash commands — reset, new, undo, history, stats, status.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

async fn active_execution(
    ctx: &CommandContext,
) -> Option<echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease> {
    let state = ctx.app_state.as_ref()?;
    let conversation_id = ctx.conversation_id.as_deref()?;
    let runtime = state.current_chat_runtime().await.ok()?;
    runtime.agent_for(conversation_id).await.ok()
}

fn active_agent(
    execution: Option<&echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease>,
    fallback: &crate::agent_handle::AgentHandle,
) -> crate::agent_handle::AgentHandle {
    execution
        .map(echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease::agent)
        .unwrap_or_else(|| fallback.clone())
}

async fn clear_repl_conversation(ctx: &CommandContext) -> anyhow::Result<()> {
    let conversation_id = ctx
        .conversation_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("No active conversation"))?;
    let app_state = ctx
        .app_state
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Application state is unavailable"))?;
    let runtime = app_state
        .current_control_runtime()
        .await
        .map_err(anyhow::Error::msg)?;
    let _admission = runtime
        .begin_managed_replacement(&app_state.session.foreground_turns, conversation_id)
        .await?;
    let store = runtime
        .conversation_store()
        .ok_or_else(|| anyhow::anyhow!("Conversation store is unavailable"))?;
    let state_store = runtime
        .runtime_state_store()
        .ok_or_else(|| anyhow::anyhow!("Runtime state store is unavailable"))?;
    let execution = runtime.agent_for(conversation_id).await?;
    echo_agent_app_core::api::managed_conversation::replace_and_resume(
        store.as_ref(),
        state_store.as_ref(),
        &execution.agent(),
        conversation_id,
        &[],
    )
    .await?;
    Ok(())
}

// ── ResetCommand ─────────────────────────────────────────────────────

async fn cmd_reset(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    match clear_repl_conversation(ctx).await {
        Ok(()) => println!("Conversation reset."),
        Err(error) => println!("Reset failed: {error}"),
    }
    CommandOutcome::Continue
}
cmd!(
    ResetCommand,
    "reset",
    ["r"],
    CommandCategory::Session,
    "Reset conversation history",
    cmd_reset
);

// ── HistoryCommand ───────────────────────────────────────────────────

async fn cmd_history(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let execution = active_execution(ctx).await;
    active_agent(execution.as_ref(), &ctx.agent)
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                let msgs = ctx.messages().to_vec();
                println!("\n--- History ({} msgs) ---", msgs.len());
                for (i, m) in msgs.iter().enumerate() {
                    let preview: String = m
                        .content
                        .as_text()
                        .unwrap_or_default()
                        .chars()
                        .take(80)
                        .collect();
                    println!("  {i:3} [{}] {preview}", m.role.as_str());
                }
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    HistoryCommand,
    "history",
    ["hist"],
    CommandCategory::Session,
    "View conversation history",
    cmd_history
);

// ── StatsCommand ─────────────────────────────────────────────────────

async fn cmd_stats(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let execution = active_execution(ctx).await;
    active_agent(execution.as_ref(), &ctx.agent)
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                println!("\n--- Stats ---");
                println!("  Messages: {}", ctx.messages().len());
                println!("  Est. tokens: {}", ctx.token_estimate());
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    StatsCommand,
    "stats",
    ["st"],
    CommandCategory::Session,
    "Show context statistics",
    cmd_stats
);

// ── StatusCommand ────────────────────────────────────────────────────

async fn cmd_status(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\n--- Status ---");
    println!("  Mode: {}", ctx.current_mode);
    CommandOutcome::Continue
}
cmd!(
    StatusCommand,
    "status",
    CommandCategory::Session,
    "Show agent runtime status",
    cmd_status
);

// ── SessionsCommand ─────────────────────────────────────────────────

async fn cmd_sessions(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let Some(app_state) = ctx.app_state.as_ref() else {
        println!("Conversation persistence is unavailable in this runtime.");
        return CommandOutcome::Continue;
    };
    let store = match app_state.current_chat_runtime().await {
        Ok(runtime) => runtime.conversation_store(),
        Err(error) => {
            println!("Conversation runtime is unavailable: {error}");
            return CommandOutcome::Continue;
        }
    };
    let Some(store) = store else {
        println!("Conversation persistence is unavailable in this runtime.");
        return CommandOutcome::Continue;
    };
    let query = args.join(" ");
    let result = if query.trim().is_empty() {
        store
            .list_conversations(echo_agent::memory::ConversationFilter {
                limit: Some(30),
                ..Default::default()
            })
            .await
    } else {
        store.search_conversations(query.trim(), 30).await
    };
    match result {
        Ok(items) if items.is_empty() => println!("No persisted conversations."),
        Ok(items) => {
            println!("\n--- Conversations ---");
            for item in items {
                let marker =
                    if ctx.conversation_id.as_deref() == Some(item.conversation_id.as_str()) {
                        "*"
                    } else {
                        " "
                    };
                let title = item
                    .title
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("Untitled");
                println!(
                    "{marker} {}  {:>4} messages  {title}",
                    item.conversation_id, item.message_count
                );
            }
        }
        Err(error) => println!("Failed to list conversations: {error}"),
    }
    CommandOutcome::Continue
}
cmd!(
    SessionsCommand,
    "sessions",
    ["ss"],
    CommandCategory::Sessions,
    "List or search persisted conversations",
    cmd_sessions
);

// ── NewCommand ───────────────────────────────────────────────────────

async fn cmd_new(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    match clear_repl_conversation(ctx).await {
        Ok(()) => {
            crate::cli::repl::reset_usage_stats();
            println!("\nNew session started (context cleared).");
        }
        Err(error) => println!("New session failed: {error}"),
    }
    CommandOutcome::Continue
}
cmd!(
    NewCommand,
    "new",
    ["n"],
    CommandCategory::Session,
    "Start new session",
    cmd_new
);

// ── UndoCommand ──────────────────────────────────────────────────────

async fn cmd_undo(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let undone = async {
        let conversation_id = ctx
            .conversation_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("No active conversation"))?;
        let app_state = ctx
            .app_state
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Application state is unavailable"))?;
        let runtime = app_state
            .current_control_runtime()
            .await
            .map_err(anyhow::Error::msg)?;
        let _admission = runtime
            .begin_managed_replacement(&app_state.session.foreground_turns, conversation_id)
            .await?;
        let store = runtime
            .conversation_store()
            .ok_or_else(|| anyhow::anyhow!("Conversation store is unavailable"))?;
        let state_store = runtime
            .runtime_state_store()
            .ok_or_else(|| anyhow::anyhow!("Runtime state store is unavailable"))?;
        let mut stored = store.get_messages(conversation_id).await?;
        if echo_agent_app_core::api::managed_conversation::take_last_user_turn(&mut stored)
            .is_none()
        {
            return Ok::<bool, anyhow::Error>(false);
        }
        let execution = runtime.agent_for(conversation_id).await?;
        echo_agent_app_core::api::managed_conversation::replace_and_resume(
            store.as_ref(),
            state_store.as_ref(),
            &execution.agent(),
            conversation_id,
            &stored,
        )
        .await?;
        Ok(true)
    }
    .await;
    match undone {
        Ok(true) => println!("Undone last turn."),
        Ok(false) => println!("No turn to undo."),
        Err(error) => println!("Undo failed: {error}"),
    }
    CommandOutcome::Continue
}
cmd!(
    UndoCommand,
    "undo",
    ["u"],
    CommandCategory::Session,
    "Undo last assistant message",
    cmd_undo
);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(ResetCommand));
    registry.register(Arc::new(HistoryCommand));
    registry.register(Arc::new(StatsCommand));
    registry.register(Arc::new(StatusCommand));
    registry.register(Arc::new(SessionsCommand));
    registry.register(Arc::new(NewCommand));
    registry.register(Arc::new(UndoCommand));
}
