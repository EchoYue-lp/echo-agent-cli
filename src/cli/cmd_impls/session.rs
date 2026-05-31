//! Session management slash commands — reset, new, undo, history, stats, status.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent::agent::Agent;
use std::sync::Arc;

// ── ResetCommand ─────────────────────────────────────────────────────

async fn cmd_reset(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    ctx.agent
        .read_async(|a| {
            Box::pin(async move {
                let mut ctx = a.context().lock().await;
                ctx.clear();
                ctx.push(echo_agent::llm::types::Message::system(
                    "You are a helpful assistant.".to_string(),
                ));
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
    ctx.agent
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
    ctx.agent
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

// ── NewCommand ───────────────────────────────────────────────────────

async fn cmd_new(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    crate::cli::repl::reset_usage_stats();
    // N3 fix: actually reset the conversation, not just usage stats
    ctx.agent
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
    ctx.agent
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
    registry.register(Arc::new(NewCommand));
    registry.register(Arc::new(UndoCommand));
}
