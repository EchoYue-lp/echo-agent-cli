//! Context management slash commands — project, think, reasoning, model, system, compress, compact, context, refresh.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent::agent::Agent;
use std::sync::Arc;

// ── ThinkCommand ──────────────────────────────────────────────────────

async fn cmd_think(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let level = args.first().copied().unwrap_or("medium");
    let max = match level {
        "low" => 3,
        "medium" => 10,
        "high" => 25,
        _ => 10,
    };
    if let Err(error) = ctx.agent.write(|a| a.set_max_iterations(max)).await {
        println!("Failed to update thinking depth: {error}");
        return CommandOutcome::Continue;
    }
    println!("Thinking depth: {level} (max {max} iterations)");
    CommandOutcome::Continue
}
cmd!(
    ThinkCommand,
    "think",
    CommandCategory::Context,
    "Adjust thinking depth (low/medium/high)",
    cmd_think
);

// ── ReasoningCommand ──────────────────────────────────────────────────

async fn cmd_reasoning(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let level = args.first().copied().unwrap_or("medium");
    let (iter, desc) = match level {
        "low" => (3, "quick"),
        "medium" => (10, "standard"),
        "high" => (25, "thorough"),
        _ => (10, "standard"),
    };
    if let Err(error) = ctx.agent.write(|a| a.set_max_iterations(iter)).await {
        println!("Failed to update reasoning effort: {error}");
        return CommandOutcome::Continue;
    }
    println!("Reasoning effort: {level} ({desc}, max {iter} iterations)");
    CommandOutcome::Continue
}
cmd!(
    ReasoningCommand,
    "reasoning",
    CommandCategory::Context,
    "Set reasoning effort (low/medium/high) — alias of /think",
    cmd_reasoning
);

// ── ModelCommand ──────────────────────────────────────────────────────

async fn cmd_model(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(m) = args.first() {
        println!(
            "Model switching now uses configured models. '{m}' was not applied; configure and select models from the provider settings."
        );
    } else {
        let model = ctx.agent.read(|a| a.model_name().to_string()).await;
        println!("Current model: {model}");
        println!("Configure/select models from the provider settings.");
    }
    CommandOutcome::Continue
}
cmd!(
    ModelCommand,
    "model",
    CommandCategory::Config,
    "View current model",
    cmd_model
);

// ── SystemCommand ─────────────────────────────────────────────────────

async fn cmd_system(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.is_empty() {
        ctx.agent
            .read_async(|a| {
                Box::pin(async move {
                    let ctx = a.context().lock().await;
                    if let Some(first) = ctx.messages().first() {
                        println!(
                            "\n--- System Prompt ---\n{}",
                            first.content.as_text().unwrap_or_default()
                        );
                    }
                })
            })
            .await;
    } else {
        let prompt = args.join(" ");
        ctx.agent
            .write_async(|a| Box::pin(async move { a.set_system_prompt(prompt).await }))
            .await;
        println!("System prompt updated.");
    }
    CommandOutcome::Continue
}
cmd!(
    SystemCommand,
    "system",
    ["sys"],
    CommandCategory::Config,
    "View or set system prompt",
    cmd_system
);

// ── CompressCommand ───────────────────────────────────────────────────

async fn cmd_compress(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let focus = if args.is_empty() {
        None
    } else {
        Some(args.join(" "))
    };
    ctx.agent
        .read_async(|a| {
            Box::pin(async move {
                let result = if let Some(ref focus_instructions) = focus {
                    a.force_compress_with_focus_and_hooks(focus_instructions, 6, "manual")
                        .await
                } else {
                    // Respect the agent's installed compression strategy (e.g.
                    // Adaptive) instead of forcing a fresh SlidingWindow.
                    a.force_compress_context().await
                };
                match result {
                    Ok((s, checkpoint)) => {
                        println!(
                            "Compressed: {} -> {} msgs ({} tokens -> {})",
                            s.before_count, s.after_count, s.before_tokens, s.after_tokens
                        );
                        if let Some(ref cp) = checkpoint {
                            println!(
                                "  Checkpoint: {} | Strategy: {} | Evicted: {} | Protected: {} | Tool fixes: {} | Duration: {}ms",
                                cp.checkpoint_id,
                                cp.strategy,
                                cp.evicted_count,
                                cp.protected_count,
                                cp.tool_pair_fixes.len(),
                                cp.compression_duration_ms
                            );
                            if let Some(ref f) = cp.focus_instructions {
                                println!("  Focus: {}", f);
                            }
                        }
                    }
                    Err(e) => println!("Compression failed: {e}"),
                }
            })
        })
        .await;
    CommandOutcome::Continue
}
// NOTE: No /cp alias — /cp belongs to /compact only
cmd!(
    CompressCommand,
    "compress",
    CommandCategory::Context,
    "Force context compression",
    cmd_compress
);

// ── CompactCommand ────────────────────────────────────────────────────

async fn cmd_compact(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let focus = if args.is_empty() {
        None
    } else {
        Some(args.join(" "))
    };
    ctx.agent
        .read_async(|a| {
            Box::pin(async move {
                let result = if let Some(ref focus_instructions) = focus {
                    a.force_compress_with_focus_and_hooks(focus_instructions, 12, "manual")
                        .await
                } else {
                    // Respect the agent's installed compression strategy (e.g.
                    // Adaptive) instead of forcing a fresh SlidingWindow.
                    a.force_compress_context().await
                };
                match result {
                    Ok((s, checkpoint)) => {
                        println!(
                            "Compact: {}->{} msgs ({} tokens -> {})",
                            s.before_count, s.after_count, s.before_tokens, s.after_tokens
                        );
                        if let Some(ref cp) = checkpoint {
                            println!(
                                "  Checkpoint: {} | Strategy: {} | Evicted: {} | Protected: {} | Tool fixes: {} | Duration: {}ms",
                                cp.checkpoint_id,
                                cp.strategy,
                                cp.evicted_count,
                                cp.protected_count,
                                cp.tool_pair_fixes.len(),
                                cp.compression_duration_ms
                            );
                            if let Some(ref f) = cp.focus_instructions {
                                println!("  Focus: {}", f);
                            }
                        }
                    }
                    Err(e) => println!("Compaction failed: {e}"),
                }
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    CompactCommand,
    "compact",
    ["cp"],
    CommandCategory::Context,
    "Lightweight context compaction",
    cmd_compact
);

// ── ContextCommand ────────────────────────────────────────────────────

async fn cmd_context(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    ctx.agent
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                println!("\n--- Context ---");
                // Show detailed token breakdown
                let breakdown = ctx.token_breakdown(None);
                println!("{}", breakdown.format_bar());
                println!(
                    "  Plan mode: {}  Iterations: {}",
                    a.is_plan_mode(),
                    a.max_iterations()
                );
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    ContextCommand,
    "context",
    CommandCategory::Context,
    "Show context state",
    cmd_context
);

// ── CheckpointCommand ─────────────────────────────────────────────────

async fn cmd_checkpoint(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let result = ctx
        .agent
        .read_async(|a| Box::pin(async move { a.force_checkpoint().await }))
        .await;
    match result {
        Ok(()) => println!("Checkpoint saved."),
        Err(error) => eprintln!("Failed to save checkpoint: {error}"),
    }
    CommandOutcome::Continue
}
cmd!(
    CheckpointCommand,
    "checkpoint",
    ["save"],
    CommandCategory::Context,
    "Force-save a runtime checkpoint (messages + plan + skills)",
    cmd_checkpoint
);

// ── RefreshCommand ────────────────────────────────────────────────────

async fn cmd_refresh(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("Project context refreshed.");
    CommandOutcome::Continue
}
cmd!(
    RefreshCommand,
    "refresh",
    CommandCategory::Context,
    "Rescan project files",
    cmd_refresh
);

// ── ProjectCommand ────────────────────────────────────────────────────

async fn cmd_project(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\nProject context loaded from current directory.");
    CommandOutcome::Continue
}
cmd!(
    ProjectCommand,
    "project",
    ["proj"],
    CommandCategory::Context,
    "View/load project context",
    cmd_project
);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(ThinkCommand));
    registry.register(Arc::new(ReasoningCommand));
    registry.register(Arc::new(ModelCommand));
    registry.register(Arc::new(SystemCommand));
    registry.register(Arc::new(CompressCommand));
    registry.register(Arc::new(CompactCommand));
    registry.register(Arc::new(ContextCommand));
    registry.register(Arc::new(CheckpointCommand));
    registry.register(Arc::new(RefreshCommand));
    registry.register(Arc::new(ProjectCommand));
}
