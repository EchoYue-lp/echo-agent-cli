//! Runtime observability slash commands for traces, prompt budgets, and runs.

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
                            RunEvent::BudgetDecision {
                                decision,
                                reason,
                                iteration,
                                reported_model_tokens,
                                usage_complete,
                            } => println!(
                                "  Budget: {} ({}, iteration {}, {} reported tokens, usage {})",
                                decision,
                                reason,
                                iteration,
                                reported_model_tokens,
                                if *usage_complete {
                                    "complete"
                                } else {
                                    "partial"
                                }
                            ),
                            RunEvent::LlmCall {
                                prompt_tokens,
                                completion_tokens,
                                cached_prompt_tokens,
                                cache_creation_prompt_tokens,
                                usage_reported,
                                estimated_context_tokens,
                                protected_context_tokens,
                                protected_message_count,
                                ..
                            } => {
                                println!(
                                    "  LLM Call: {}->{} tokens, cached {}, cache write {}, context ~{}, protected ~{} ({} messages), usage {}",
                                    prompt_tokens,
                                    completion_tokens,
                                    cached_prompt_tokens,
                                    cache_creation_prompt_tokens,
                                    estimated_context_tokens,
                                    protected_context_tokens,
                                    protected_message_count,
                                    if *usage_reported {
                                        "reported"
                                    } else {
                                        "missing"
                                    },
                                );
                            }
                            RunEvent::ToolCall { name, .. } => println!("  Tool Call: {}", name),
                            RunEvent::ToolResult {
                                name,
                                output_truncated,
                                original_bytes,
                                returned_bytes,
                                estimated_tokens,
                                output_handling,
                                artifact,
                                ..
                            } => {
                                println!(
                                    "  Tool Result: {} {} [{}; {} -> {} bytes; ~{} tokens]",
                                    name,
                                    if *output_truncated { "(truncated)" } else { "" },
                                    output_handling.as_deref().unwrap_or("unknown"),
                                    original_bytes,
                                    returned_bytes,
                                    estimated_tokens,
                                );
                                if let Some(artifact) = artifact {
                                    println!(
                                        "    Artifact: {} ({} bytes, sha256 {}, retention {})",
                                        artifact.path,
                                        artifact.bytes,
                                        artifact.sha256,
                                        artifact.retention,
                                    );
                                }
                            }
                            RunEvent::ToolError { name, message, .. } => {
                                println!("  Tool Error: {} - {}", name, message)
                            }
                            RunEvent::Error { message } => println!("  Error: {}", message),
                            RunEvent::Checkpoint { id } => println!("  Checkpoint: {}", id),
                            RunEvent::CheckpointResumed {
                                conversation_id,
                                completed_tool_call_ids,
                                checkpoint_timestamp,
                            } => println!(
                                "  Resumed: {} at {} ({} completed tools: {})",
                                conversation_id,
                                checkpoint_timestamp,
                                completed_tool_call_ids.len(),
                                completed_tool_call_ids.join(", ")
                            ),
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

async fn cmd_prompt_diagnostics(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let context = ctx.agent.read(|agent| agent.context().clone()).await;
    let (message_count, estimated_tokens, protected_message_count, protected_tokens) = {
        let context = context.lock().await;
        (
            context.messages().len(),
            context.token_estimate(),
            context.protected_message_count(),
            context.protected_token_estimate(),
        )
    };

    println!("\nPrompt diagnostics (local estimates):");
    if let Some(assembly) = ctx.prompt_assembly.as_ref() {
        println!("  Static prompt: ~{} tokens", assembly.estimated_tokens);
        for module in &assembly.modules {
            let status = if !module.included {
                "omitted"
            } else if module.truncated {
                "truncated"
            } else {
                "full"
            };
            println!(
                "    {:<24} ~{:>6} tokens  {}",
                module.name, module.estimated_tokens, status
            );
        }
    } else {
        println!("  Static prompt report: unavailable");
    }
    println!(
        "  Current context: ~{} tokens across {} messages",
        estimated_tokens, message_count
    );
    println!(
        "  Protected context: ~{} tokens across {} messages",
        protected_tokens, protected_message_count
    );
    CommandOutcome::Continue
}
cmd!(
    PromptDiagnosticsCommand,
    "prompt-diagnostics",
    CommandCategory::Debug,
    "Show prompt modules and current protected-context estimates",
    cmd_prompt_diagnostics
);

// ── RunsCommand ───────────────────────────────────────────────────────

async fn cmd_runs(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.run_store.clone()).await;
    if let Some(ref s) = store {
        match s.list_all(10).await {
            Ok(runs) => {
                println!("\n--- Recent Runs ---");
                for r in &runs {
                    let short_id: String = r.run_id.chars().take(12).collect();
                    println!("  {:?} {} — {}", r.status, short_id, r.input_preview);
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
    let Some(sub) = args.first().copied() else {
        println!("Usage: /run show <id> | /run export <id>");
        return CommandOutcome::Continue;
    };
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
    registry.register(Arc::new(PromptDiagnosticsCommand));
    registry.register(Arc::new(RunsCommand));
    registry.register(Arc::new(RunCommand));
}
