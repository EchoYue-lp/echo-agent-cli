//! Session management slash commands — reset, new, undo, history, stats, status.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent::agent::Agent;
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

// ── ResetCommand ─────────────────────────────────────────────────────

async fn cmd_reset(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let execution = active_execution(ctx).await;
    active_agent(execution.as_ref(), &ctx.agent)
        .read_async(|a| {
            Box::pin(async move {
                let system_prompt = a.system_prompt().to_string();
                let mut ctx = a.context().lock().await;
                ctx.clear();
                ctx.push(echo_agent::llm::types::Message::system(system_prompt));
            })
        })
        .await;
    println!("Conversation reset.");
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
    crate::cli::repl::reset_usage_stats();
    // N3 fix: actually reset the conversation, not just usage stats
    let execution = active_execution(ctx).await;
    active_agent(execution.as_ref(), &ctx.agent)
        .read_async(|a| {
            Box::pin(async move {
                a.reset().await;
            })
        })
        .await;
    println!("\nNew session started (context cleared).");
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
    let execution = active_execution(ctx).await;
    active_agent(execution.as_ref(), &ctx.agent)
        .read_async(|a| {
            Box::pin(async move {
                let mut messages = a.get_messages().await;
                let original_len = messages.len();
                for _ in 0..2 {
                    messages.pop();
                }
                if original_len != messages.len() {
                    a.load_messages(messages).await;
                }
            })
        })
        .await;
    println!("Undone last turn.");
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
