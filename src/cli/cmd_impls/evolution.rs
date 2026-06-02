//! Evolution & trajectory slash commands — self-improvement and trajectory management.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

// ── TrajectoriesCommand ─────────────────────────────────────────────

async fn cmd_trajectories(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("list");

    match sub {
        "stats" => match echo_agent::improve::TrajectorySaver::default_dir() {
            Ok(saver) => match saver.stats().await {
                Ok(stats) => {
                    println!("\n--- Trajectory Stats ---");
                    println!("  Total:       {}", stats.total);
                    println!("  Completed:   {}", stats.completed);
                    println!("  Failed:      {}", stats.failed);
                    println!("  Total tokens: {}", stats.total_tokens);
                    println!("  Tool calls:  {}", stats.total_tool_calls);
                    println!("  Avg duration: {}ms", stats.avg_duration_ms);
                }
                Err(e) => println!("Error reading stats: {e}"),
            },
            Err(e) => println!("Error initializing trajectory saver: {e}"),
        },
        "list" | _ => {
            let date_filter = args.get(1).copied();
            match echo_agent::improve::TrajectorySaver::default_dir() {
                Ok(saver) => match saver.list(date_filter).await {
                    Ok(entries) if !entries.is_empty() => {
                        println!("\n--- Trajectories ---");
                        for entry in entries.iter().take(20) {
                            let status = if entry.completed { "✅" } else { "❌" };
                            let id_short = &entry.id[..12.min(entry.id.len())];
                            let preview: String = entry
                                .conversations
                                .first()
                                .map(|m| m.value.chars().take(60).collect())
                                .unwrap_or_default();
                            println!(
                                "  {status} {id_short}  [{}]  tokens={}  tools={}  {preview}",
                                entry.model, entry.token_usage, entry.tool_call_count,
                            );
                        }
                        if entries.len() > 20 {
                            println!("  ... and {} more", entries.len() - 20);
                        }
                    }
                    Ok(_) => println!("No trajectories saved yet. Run some conversations first."),
                    Err(e) => println!("Error listing trajectories: {e}"),
                },
                Err(e) => println!("Error initializing trajectory saver: {e}"),
            }
        }
    }
    CommandOutcome::Continue
}
cmd!(
    TrajectoriesCommand,
    "trajectories",
    ["traj"],
    CommandCategory::Advanced,
    "List or inspect saved trajectories",
    cmd_trajectories
);

// ── ReviewCommand ───────────────────────────────────────────────────

async fn cmd_review(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    // Get the run store and LLM client from the agent
    let (run_store, llm_client, memory_store) = ctx
        .agent
        .read(|a| {
            (
                a.run_store.clone(),
                a.llm_client().cloned(),
                a.store().cloned(),
            )
        })
        .await;

    let run_store = match run_store {
        Some(s) => s,
        None => {
            println!("No run store configured. Enable run tracing first.");
            return CommandOutcome::Continue;
        }
    };

    let llm_client = match llm_client {
        Some(c) => c,
        None => {
            println!("No LLM client available.");
            return CommandOutcome::Continue;
        }
    };

    // Get the latest run
    let runs = match run_store.list_all(1).await {
        Ok(r) => r,
        Err(e) => {
            println!("Error listing runs: {e}");
            return CommandOutcome::Continue;
        }
    };

    let run_summary = match runs.first() {
        Some(r) => r,
        None => {
            println!("No runs to review. Run a conversation first.");
            return CommandOutcome::Continue;
        }
    };

    let run = match run_store.load(&run_summary.run_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            println!("Run {} not found.", run_summary.run_id);
            return CommandOutcome::Continue;
        }
        Err(e) => {
            println!("Error loading run: {e}");
            return CommandOutcome::Continue;
        }
    };

    println!(
        "Reviewing run {}...",
        &run.run_id[..12.min(run.run_id.len())]
    );

    let reviewer = echo_agent::improve::BackgroundReviewer::new(
        echo_agent::improve::BackgroundReviewConfig::default(),
        llm_client,
        memory_store,
        Some(run_store),
    );

    match reviewer.review(&run) {
        Ok(handle) => match handle.await {
            Ok(outcome) => {
                if outcome.nothing_to_save {
                    println!("Nothing to save.");
                } else {
                    println!("Review actions:");
                    for action in &outcome.actions {
                        println!("  - {action}");
                    }
                }
                if let Some(ref err) = outcome.error {
                    println!("Warning: {err}");
                }
            }
            Err(e) => println!("Review task panicked: {e}"),
        },
        Err(e) => println!("Review failed: {e}"),
    }

    CommandOutcome::Continue
}
cmd!(
    ReviewCommand,
    "review",
    CommandCategory::Advanced,
    "Run background review on last run",
    cmd_review
);

// ── CuratorCommand ─────────────────────────────────────────────────

async fn cmd_curator(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("status");
    let curator =
        echo_agent::improve::Curator::default_path(echo_agent::improve::CuratorConfig::default());

    match sub {
        "status" => {
            let status = curator.status();
            println!("\n--- Curator Status ---");
            println!("  Total skills: {}", status.total);
            println!("  Active:       {}", status.active);
            println!("  Stale:        {}", status.stale);
            println!("  Archived:     {}", status.archived);
            println!("  Pinned:       {}", status.pinned);
            if let Some(last) = status.last_run_at {
                println!("  Last run:     {}", last.format("%Y-%m-%d %H:%M:%S"));
            }
        }
        "run" => match curator.apply_transitions() {
            Ok(transitions) if !transitions.is_empty() => {
                println!("Applied {} transition(s):", transitions.len());
                for (name, from, to) in &transitions {
                    println!("  {name}: {from:?} → {to:?}");
                }
            }
            Ok(_) => println!("No transitions needed."),
            Err(e) => println!("Error applying transitions: {e}"),
        },
        "pin" => {
            if let Some(name) = args.get(1) {
                match curator.pin_skill(name) {
                    Ok(()) => println!("Pinned skill: {name}"),
                    Err(e) => println!("Error: {e}"),
                }
            } else {
                println!("Usage: /curator pin <skill-name>");
            }
        }
        "unpin" => {
            if let Some(name) = args.get(1) {
                match curator.unpin_skill(name) {
                    Ok(()) => println!("Unpinned skill: {name}"),
                    Err(e) => println!("Error: {e}"),
                }
            } else {
                println!("Usage: /curator unpin <skill-name>");
            }
        }
        _ => {
            println!("Usage: /curator status|run|pin <name>|unpin <name>");
        }
    }
    CommandOutcome::Continue
}
cmd!(
    CuratorCommand,
    "curator",
    CommandCategory::Advanced,
    "Manage skill lifecycle (status/run/pin/unpin)",
    cmd_curator
);

// ── CritiquesCommand ────────────────────────────────────────────────

async fn cmd_critiques(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("list");

    match sub {
        "list" | "ls" | "" => {
            let handle = ctx.agent.clone();
            handle.read_async(|a| Box::pin(async move {
                if let Some(ref run_store) = a.run_store {
                    match run_store.list_all(20).await {
                        Ok(runs) => {
                            println!("\n--- Critiques from Recent Runs ---");
                            if runs.is_empty() {
                                println!("  No runs recorded yet.");
                                println!("  Run /review to generate a critique on the latest run.");
                            } else {
                                println!("  {} run(s) available for review.", runs.len());
                                for run in runs.iter().take(10) {
                                    println!("  • {} ({:?})", &run.run_id[..12.min(run.run_id.len())], run.status);
                                }
                                println!("\n  Run /review to generate critiques on the latest run.");
                            }
                        }
                        Err(e) => println!("  Error loading runs: {e}"),
                    }
                } else {
                    println!("  Run store not available. Critiques require run tracking.");
                }
            })).await;
        }
        "clear" => {
            println!("Critiques cleared.");
        }
        _ => {
            println!("Usage: /critiques [list|clear]");
        }
    }
    CommandOutcome::Continue
}
cmd!(
    CritiquesCommand,
    "critiques",
    ["cq"],
    CommandCategory::Advanced,
    "View critiques from background reviews",
    cmd_critiques
);

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(TrajectoriesCommand));
    registry.register(Arc::new(ReviewCommand));
    registry.register(Arc::new(CuratorCommand));
    registry.register(Arc::new(CritiquesCommand));
}
