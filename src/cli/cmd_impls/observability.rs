//! Runtime observability slash commands for traces, prompt budgets, and runs.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

// ── TraceCommand ───────────────────────────────────────────────────────

async fn cmd_trace(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.run_store.clone()).await;
    let Some(store) = store else {
        println!("Run store not configured.");
        return CommandOutcome::Continue;
    };
    let diagnostic_id = match args.first().copied().filter(|value| !value.is_empty()) {
        Some(value) => value.to_string(),
        None => {
            match echo_agent_app_core::api::observability::list_diagnostic_runs(store.as_ref())
                .await
            {
                Ok(runs) => match runs.first() {
                    Some(run) => run.diagnostic_id.clone(),
                    None => {
                        println!("No durable run diagnostics available.");
                        return CommandOutcome::Continue;
                    }
                },
                Err(error) => {
                    println!("Unable to list run diagnostics: {error}");
                    return CommandOutcome::Continue;
                }
            }
        }
    };
    match echo_agent_app_core::api::observability::load_run_diagnostics(
        store.as_ref(),
        &diagnostic_id,
        ctx.prompt_assembly.clone(),
    )
    .await
    {
        Ok(Some(diagnostics)) => println!(
            "\n{}",
            echo_agent_app_core::api::observability::format_run_diagnostics(&diagnostics)
        ),
        Ok(None) => println!("Run diagnostics not found: {diagnostic_id}"),
        Err(error) => println!("Unable to load run diagnostics: {error}"),
    }
    CommandOutcome::Continue
}
cmd!(
    TraceCommand,
    "trace",
    CommandCategory::Debug,
    "Show durable usage, cache, context, and compression diagnostics",
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
