//! Info slash commands — tools, cost, usage, debug.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use crate::cli::commands::estimate_price;
use echo_agent::agent::Agent;
use std::sync::Arc;

// ── ToolsCommand ──────────────────────────────────────────────────────

async fn cmd_tools(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    ctx.agent
        .read_async(|a| {
            Box::pin(async move {
                let names = a.tool_names();
                let defs = a.tool_definitions();
                println!("\n--- Registered Tools ({}) ---", names.len());
                for name in &names {
                    if let Some(def) = defs.iter().find(|d| &d.function.name == name) {
                        let desc: String = def.function.description.chars().take(60).collect();
                        println!("  * {} - {}", name, desc);
                    } else {
                        println!("  * {}", name);
                    }
                }
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    ToolsCommand,
    "tools",
    ["t"],
    CommandCategory::Info,
    "List registered tools",
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
                let (in_price, out_price, currency) = estimate_price(&model);
                let input_cost = (input_tokens as f64 / 1000.0) * in_price;
                let output_cost = (output_tokens as f64 / 1000.0) * out_price;
                let total = input_cost + output_cost;

                println!();
                println!("--- Session Cost ---");
                println!("  Model:         {model}");
                println!("  Context tokens: {}", tokens);
                println!("  Input tokens:   {input_tokens}  ({input_cost:.4} {currency})");
                println!("  Output tokens:  {output_tokens}  ({output_cost:.4} {currency})");
                println!("  Tool calls:     {tool_calls}");
                println!("  Total est:      {total:.4} {currency}");
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
    let (in_price, out_price, currency) = estimate_price(&model);
    let total =
        (input_tokens as f64 / 1000.0) * in_price + (output_tokens as f64 / 1000.0) * out_price;

    println!();
    println!("--- Token Usage ---");
    println!("  Model:    {model}");
    println!("  Input:    {input_tokens} tokens ({in_price} {currency}/1k)");
    println!("  Output:   {output_tokens} tokens ({out_price} {currency}/1k)");
    println!("  Tools:    {tool_calls} calls");
    println!("  Est cost: {total:.4} {currency}");
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
