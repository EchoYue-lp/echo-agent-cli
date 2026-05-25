//! Context management slash commands — mode, project, think, reasoning, model, system, compress, compact, context, refresh.

use std::sync::Arc;
use echo_agent::agent::Agent;
use crate::cli::command::{cmd, CommandCategory, CommandContext, CommandOutcome};

// ── ModeCommand ───────────────────────────────────────────────────────

async fn cmd_mode(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(mode) = args.first() {
        match crate::project::modes::AgentMode::from_str(mode) {
            Some(agent_mode) => {
                let prompt = agent_mode.system_prompt().to_string();
                ctx.agent.write_async(|a| Box::pin(async move { a.set_system_prompt(prompt).await })).await;
                println!("Mode: {} {}", agent_mode.icon(), agent_mode.display_name());
                println!("System prompt updated.");
            }
            None => {
                println!("Unknown mode: {mode}");
                println!("Available: general, coding, research, data, writing");
            }
        }
    } else {
        println!("Current mode: {}", ctx.current_mode);
        println!("Available: general, coding, research, data, writing");
    }
    CommandOutcome::Continue
}
cmd!(ModeCommand, "mode", CommandCategory::Context, "View or switch agent mode", cmd_mode);

// ── ThinkCommand ──────────────────────────────────────────────────────

async fn cmd_think(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let level = args.first().copied().unwrap_or("medium");
    let max = match level { "low" => 3, "medium" => 10, "high" => 25, _ => 10 };
    ctx.agent.write(|a| a.set_max_iterations(max)).await;
    println!("Thinking depth: {level} (max {max} iterations)");
    CommandOutcome::Continue
}
cmd!(ThinkCommand, "think", CommandCategory::Context, "Adjust thinking depth (low/medium/high)", cmd_think);

// ── ReasoningCommand ──────────────────────────────────────────────────

async fn cmd_reasoning(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let level = args.first().copied().unwrap_or("medium");
    let (iter, desc) = match level { "low"=> (3,"quick"), "medium"=> (10,"standard"), "high"=> (25,"thorough"), _=> (10,"standard") };
    ctx.agent.write(|a| a.set_max_iterations(iter)).await;
    println!("Reasoning effort: {level} ({desc}, max {iter} iterations)");
    CommandOutcome::Continue
}
cmd!(ReasoningCommand, "reasoning", CommandCategory::Context, "Set reasoning effort (low/medium/high)", cmd_reasoning);

// ── ModelCommand ──────────────────────────────────────────────────────

async fn cmd_model(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(m) = args.first() {
        ctx.agent.write(|a| a.set_model(m)).await;
        println!("Model set to: {m}");
    } else {
        let model = ctx.agent.read(|a| a.model_name().to_string()).await;
        println!("Current model: {model}");
        println!("Use /model <name> to switch models");
    }
    CommandOutcome::Continue
}
cmd!(ModelCommand, "model", CommandCategory::Config, "View or switch model", cmd_model);

// ── SystemCommand ─────────────────────────────────────────────────────

async fn cmd_system(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.is_empty() {
        ctx.agent.read_async(|a| Box::pin(async move {
            let ctx = a.context().lock().await;
            if let Some(first) = ctx.messages().first() {
                println!("\n--- System Prompt ---\n{}", first.content.as_text().unwrap_or_default());
            }
        })).await;
    } else {
        let prompt = args.join(" ");
        ctx.agent.write_async(|a| Box::pin(async move { a.set_system_prompt(prompt).await })).await;
        println!("System prompt updated.");
    }
    CommandOutcome::Continue
}
cmd!(SystemCommand, "system", ["sys"], CommandCategory::Config, "View or set system prompt", cmd_system);

// ── CompressCommand ───────────────────────────────────────────────────

async fn cmd_compress(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    ctx.agent.read_async(|a| Box::pin(async move {
        let mut ctx = a.context().lock().await;
        match ctx.force_compress(6).await {
            Ok(s) => println!("Compressed: {} -> {} msgs ({} tokens -> {})",
                s.before_count, s.after_count, s.before_tokens, s.after_tokens),
            Err(e) => println!("Compression failed: {e}"),
        }
    })).await;
    CommandOutcome::Continue
}
// NOTE: No /cp alias — /cp belongs to /compact only
cmd!(CompressCommand, "compress", CommandCategory::Context, "Force context compression", cmd_compress);

// ── CompactCommand ────────────────────────────────────────────────────

async fn cmd_compact(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    ctx.agent.read_async(|a| Box::pin(async move {
        let mut ctx = a.context().lock().await;
        match ctx.force_compress(6).await {
            Ok(s) => println!("Compact: {}->{} msgs", s.before_count, s.after_count),
            Err(e) => println!("Compaction failed: {e}"),
        }
    })).await;
    CommandOutcome::Continue
}
cmd!(CompactCommand, "compact", ["cp"], CommandCategory::Context, "Lightweight context compaction", cmd_compact);

// ── ContextCommand ────────────────────────────────────────────────────

async fn cmd_context(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    ctx.agent.read_async(|a| Box::pin(async move {
        let ctx = a.context().lock().await;
        println!("\n--- Context ---");
        println!("  Messages: {}  Tokens: {}", ctx.messages().len(), ctx.token_estimate());
        println!("  Plan mode: {}  Iterations: {}", a.is_plan_mode(), a.max_iterations());
    })).await;
    CommandOutcome::Continue
}
cmd!(ContextCommand, "context", CommandCategory::Context, "Show context state", cmd_context);

// ── RefreshCommand ────────────────────────────────────────────────────

async fn cmd_refresh(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("Project context refreshed.");
    CommandOutcome::Continue
}
cmd!(RefreshCommand, "refresh", CommandCategory::Context, "Rescan project files", cmd_refresh);

// ── ProjectCommand ────────────────────────────────────────────────────

async fn cmd_project(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\nProject context loaded from current directory.");
    CommandOutcome::Continue
}
cmd!(ProjectCommand, "project", ["proj"], CommandCategory::Context, "View/load project context", cmd_project);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(ModeCommand));
    registry.register(Arc::new(ThinkCommand));
    registry.register(Arc::new(ReasoningCommand));
    registry.register(Arc::new(ModelCommand));
    registry.register(Arc::new(SystemCommand));
    registry.register(Arc::new(CompressCommand));
    registry.register(Arc::new(CompactCommand));
    registry.register(Arc::new(ContextCommand));
    registry.register(Arc::new(RefreshCommand));
    registry.register(Arc::new(ProjectCommand));
}
