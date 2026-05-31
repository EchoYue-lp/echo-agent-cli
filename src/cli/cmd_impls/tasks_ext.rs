//! Extended task management commands — progress tracking, dependency visualization.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

// ── TaskProgressCommand ──────────────────────────────────────────────

async fn cmd_task_progress(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let handle = ctx.agent.clone();
    handle
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                let msg_count = ctx.messages().len();
                let tokens = ctx.token_estimate();

                println!("\n--- Task Progress ---");
                println!("  Messages: {}", msg_count);
                println!("  Est. tokens: ~{}", tokens);
                println!(
                    "  Plan mode: {}",
                    if a.is_plan_mode() { "ON" } else { "OFF" }
                );
                println!("\n  Use /tasks to manage coding tasks.");
                println!("  Use /plan to toggle plan mode.");
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    TaskProgressCommand,
    "task-progress",
    ["tp"],
    CommandCategory::Advanced,
    "Show current task progress",
    cmd_task_progress
);

// ── TaskTreeCommand ─────────────────────────────────────────────────

async fn cmd_task_tree(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let handle = ctx.agent.clone();
    handle
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                let messages = ctx.messages();

                println!("\n--- Conversation Flow ---");
                println!("  Total messages: {}", messages.len());

                // Show message flow with roles
                let mut entries: Vec<(usize, String, String)> = Vec::new();
                for (i, msg) in messages.iter().enumerate() {
                    let role = msg.role.as_str().to_string();
                    let preview = msg
                        .content
                        .as_text_ref()
                        .map(|s| {
                            let truncated: String = s.chars().take(80).collect();
                            if s.len() > 80 {
                                format!("{}...", truncated)
                            } else {
                                truncated
                            }
                        })
                        .unwrap_or_else(|| "[non-text]".to_string());
                    entries.push((i, role, preview));
                }

                if entries.is_empty() {
                    println!("  No messages recorded.");
                } else {
                    println!("  Message flow:\n");
                    for (idx, role, preview) in entries.iter().take(40) {
                        let role_tag = match role.as_str() {
                            "user" => "USR",
                            "assistant" => "AST",
                            "system" => "SYS",
                            "tool" | "ToolResult" => "TLR",
                            _ => role,
                        };
                        println!("    [{:>3}] {:>3}: {}", idx, role_tag, preview);
                    }
                    if entries.len() > 40 {
                        println!("    ... and {} more", entries.len() - 40);
                    }
                }
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    TaskTreeCommand,
    "task-tree",
    ["tt"],
    CommandCategory::Advanced,
    "Show conversation/task flow tree",
    cmd_task_tree
);

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(TaskProgressCommand));
    registry.register(Arc::new(TaskTreeCommand));
}
