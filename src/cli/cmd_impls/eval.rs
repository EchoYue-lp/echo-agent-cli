//! Evaluation & trace slash commands — trace.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

// ── TraceCommand ───────────────────────────────────────────────────────

async fn cmd_trace(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.run_store.clone()).await;
    if let Some(ref store) = store {
        match store.list_all(1).await {
            Ok(runs) if !runs.is_empty() => {
                let run_id = &runs[0].run_id;
                if let Ok(Some(run)) = store.load(run_id).await {
                    println!("\nRun: {} (status: {:?})\n", run_id, run.status);
                    for event in &run.events {
                        use echo_agent::trace::RunEvent;
                        match event {
                            RunEvent::LlmCall {
                                prompt_tokens,
                                completion_tokens,
                                ..
                            } => {
                                println!(
                                    "  LLM Call: {}->{} tokens",
                                    prompt_tokens, completion_tokens
                                );
                            }
                            RunEvent::ToolCall { name, .. } => println!("  Tool Call: {}", name),
                            RunEvent::ToolResult {
                                name,
                                output_truncated,
                                ..
                            } => {
                                println!(
                                    "  Tool Result: {} {}",
                                    name,
                                    if *output_truncated { "(truncated)" } else { "" }
                                );
                            }
                            RunEvent::ToolError { name, message, .. } => {
                                println!("  Tool Error: {} - {}", name, message)
                            }
                            RunEvent::Error { message } => println!("  Error: {}", message),
                            RunEvent::Checkpoint { id } => println!("  Checkpoint: {}", id),
                            RunEvent::PhaseTransition { phase, iteration } => {
                                println!("  Phase: {} (iteration {})", phase, iteration)
                            }
                            RunEvent::PermissionDecision {
                                tool,
                                decision,
                                reason,
                            } => println!("  Permission: {} -> {} ({})", tool, decision, reason),
                            RunEvent::FileEdit { tool, path } => {
                                println!("  File Edit: {} -> {}", tool, path)
                            }
                            RunEvent::TestRun {
                                command,
                                passed,
                                failure_count,
                            } => {
                                println!(
                                    "  Test Run [{}]: {} ({} failures)",
                                    if *passed { "PASS" } else { "FAIL" },
                                    command,
                                    failure_count
                                );
                            }
                            RunEvent::SubAgentRun {
                                agent_name,
                                task,
                                outcome,
                            } => println!("  SubAgent: {} -> {} ({})", agent_name, task, outcome),
                        }
                    }
                    return CommandOutcome::Continue;
                }
            }
            _ => {}
        }
    }
    println!("No trace data available (run a conversation first)");
    CommandOutcome::Continue
}
cmd!(
    TraceCommand,
    "trace",
    CommandCategory::Debug,
    "Show execution timeline of last run",
    cmd_trace
);

// ── SelfReviewCommand ──────────────────────────────────────────────────

async fn cmd_self_review(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.run_store.clone()).await;
    if let Some(ref s) = store
        && let Ok(runs) = s.list_all(1).await
        && let Some(r) = runs.first()
        && let Ok(Some(run)) = s.load(&r.run_id).await
    {
        let critique = echo_agent::improve::Analyzer::analyze(&run);
        println!("\n{}", critique.format_report());
        return CommandOutcome::Continue;
    }
    println!("No runs to review.");
    CommandOutcome::Continue
}
cmd!(
    SelfReviewCommand,
    "self-review",
    CommandCategory::Advanced,
    "Analyze last run for improvements",
    cmd_self_review
);

// ── ImproveCommand ────────────────────────────────────────────────────

async fn cmd_improve(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("");
    match sub {
        "prompt" => {
            println!("Analyzing failures for prompt improvements... Use /self-review first.")
        }
        "policy" => println!("Analyzing for policy suggestions... Use /self-review first."),
        "eval" => println!("Generating eval cases from failures... Use /self-review first."),
        _ => println!("Usage: /improve prompt|policy|eval"),
    }
    CommandOutcome::Continue
}
cmd!(
    ImproveCommand,
    "improve",
    CommandCategory::Advanced,
    "Improve prompt/policy/eval from runs",
    cmd_improve
);

// ── RunsCommand ───────────────────────────────────────────────────────

async fn cmd_runs(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.run_store.clone()).await;
    if let Some(ref s) = store {
        match s.list_all(10).await {
            Ok(runs) => {
                println!("\n--- Recent Runs ---");
                for r in &runs {
                    println!(
                        "  {:?} {} — {}",
                        r.status,
                        &r.run_id[..12.min(r.run_id.len())],
                        r.input_preview
                    );
                }
            }
            _ => println!("No runs recorded."),
        }
    } else {
        println!("Run store not configured.");
    }
    CommandOutcome::Continue
}
cmd!(
    RunsCommand,
    "runs",
    CommandCategory::Debug,
    "List recent runs",
    cmd_runs
);

// ── RunCommand ────────────────────────────────────────────────────────

async fn cmd_run_show(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.is_empty() {
        println!("Usage: /run show <id> | /run export <id>");
    } else {
        let sub = args[0];
        let id = args.get(1).copied().unwrap_or("");
        let store = ctx.agent.read(|a| a.run_store.clone()).await;
        if let Some(ref s) = store {
            match sub {
                "export" => {
                    if let Ok(Some(run)) = s.load(id).await {
                        println!("{}", serde_json::to_string_pretty(&run).unwrap_or_default());
                    }
                }
                _ => {
                    if let Ok(Some(run)) = s.load(id).await {
                        println!(
                            "\nRun: {}\nInput: {}\nEvents: {}",
                            run.run_id,
                            run.input,
                            run.events.len()
                        );
                    }
                }
            }
        }
    }
    CommandOutcome::Continue
}
cmd!(
    RunCommand,
    "run",
    CommandCategory::Debug,
    "Show or export a run",
    cmd_run_show
);

// ── Register ───────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(TraceCommand));
    registry.register(Arc::new(SelfReviewCommand));
    registry.register(Arc::new(ImproveCommand));
    registry.register(Arc::new(RunsCommand));
    registry.register(Arc::new(RunCommand));
}
