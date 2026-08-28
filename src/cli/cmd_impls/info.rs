//! Info slash commands — tools, cost, usage, debug.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent::agent::Agent;
use std::sync::Arc;

// ── ToolsCommand ──────────────────────────────────────────────────────

async fn cmd_tools(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(state) = ctx.app_state.as_ref() {
        println!(
            "{}",
            echo_agent_app_core::tool_control::execute_tool_control_command(
                state,
                &ctx.agent,
                &args.join(" "),
            )
            .await
        );
    } else {
        let names = ctx.agent.read(|agent| agent.tool_names()).await;
        println!("Registered tools ({}):\n{}", names.len(), names.join("\n"));
    }
    CommandOutcome::Continue
}
cmd!(
    ToolsCommand,
    "tools",
    ["t"],
    CommandCategory::Info,
    "List or enable/disable registered tools",
    cmd_tools
);

// ── CostCommand ───────────────────────────────────────────────────────

async fn cmd_cost(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let (input_tokens, output_tokens, tool_calls) = crate::cli::repl::get_usage_stats();
    ctx.agent
        .read_async(|a| {
            Box::pin(async move {
                let model = a.model_name().to_string();
                let ctx = a.context().lock().await;
                let tokens = ctx.token_estimate();

                println!();
                println!("--- Session Usage ---");
                println!("  Model:         {model}");
                println!("  Context tokens: {}", tokens);
                println!("  Input tokens:   {input_tokens}");
                println!("  Output tokens:  {output_tokens}");
                println!("  Tool calls:     {tool_calls}");
                println!();
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    CostCommand,
    "cost",
    CommandCategory::Info,
    "Show session cost estimate",
    cmd_cost
);

// ── UsageCommand ──────────────────────────────────────────────────────

async fn cmd_usage(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let (input_tokens, output_tokens, tool_calls) = crate::cli::repl::get_usage_stats();
    let model = ctx.agent.read(|a| a.model_name().to_string()).await;

    println!();
    println!("--- Token Usage ---");
    println!("  Model:    {model}");
    println!("  Input:    {input_tokens} tokens");
    println!("  Output:   {output_tokens} tokens");
    println!("  Tools:    {tool_calls} calls");
    println!();
    CommandOutcome::Continue
}
cmd!(
    UsageCommand,
    "usage",
    CommandCategory::Info,
    "Show token usage and cost",
    cmd_usage
);

// ── DebugCommand ──────────────────────────────────────────────────────

async fn cmd_debug(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("");
    match sub {
        "on" | "enable" => println!("Debug logging enabled."),
        "off" | "disable" => println!("Debug logging disabled."),
        "stats" => {
            ctx.agent
                .read_async(|a| {
                    Box::pin(async move {
                        let ctx = a.context().lock().await;
                        println!("\n--- Debug Stats ---");
                        println!("  Messages: {}", ctx.messages().len());
                        println!("  Tokens:   {}", ctx.token_estimate());
                        println!("  Plan:     {}", a.is_plan_mode());
                        println!("  Iter max: {}", a.max_iterations());
                    })
                })
                .await;
        }
        "recent" => println!("Recent calls: use /trace for last run timeline."),
        "clear" => println!("Debug history cleared."),
        _ => {
            println!("Usage: /debug [on|off|stats|recent|clear]");
        }
    }
    CommandOutcome::Continue
}
cmd!(
    DebugCommand,
    "debug",
    ["dbg"],
    CommandCategory::Debug,
    "Show debug information",
    cmd_debug
);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(ToolsCommand));
    registry.register(Arc::new(CostCommand));
    registry.register(Arc::new(UsageCommand));
    registry.register(Arc::new(DebugCommand));
}
