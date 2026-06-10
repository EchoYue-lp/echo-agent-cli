//! Evolution & trajectory slash commands — self-improvement and trajectory management.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

use echo_agent::workspace::state::profiles::{AgentProfile, ProfileStore, UserProfile};
use echo_agent::workspace::state::skill_telemetry::SkillTelemetryStore;

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

// ── SkillReviewCommand ───────────────────────────────────────────

async fn cmd_skill_review(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let skill_name = args.first().copied();

    let store = ctx.agent.read(|a| a.store().cloned()).await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured. Cannot load telemetry.");
            return CommandOutcome::Continue;
        }
    };

    let telemetry_store = SkillTelemetryStore::new(store);

    match skill_name {
        Some(name) => {
            // Review a specific skill
            match telemetry_store.get_telemetry(name).await {
                Ok(Some(t)) => print_skill_review(&t),
                Ok(None) => {
                    println!("No telemetry data for skill '{}'.", name);
                    println!("Use the skill a few times first, then review.");
                }
                Err(e) => println!("Error loading telemetry: {e}"),
            }
        }
        None => {
            // List all skills with telemetry
            match telemetry_store.list_all().await {
                Ok(telemetry) if !telemetry.is_empty() => {
                    println!("\n=== Skill Telemetry Overview ===");
                    println!(
                        "{:<25} {:>8} {:>8} {:>10} {:>10}",
                        "Skill", "Uses", "Success", "Avg(ms)", "Rate"
                    );
                    println!("{}", "-".repeat(65));
                    for t in &telemetry {
                        println!(
                            "{:<25} {:>8} {:>8} {:>10} {:>9.0}%",
                            t.skill_name,
                            t.activation_count,
                            t.success_count,
                            t.avg_duration_ms(),
                            t.success_rate() * 100.0,
                        );
                    }
                    println!("\nRun /skill-review <name> for detailed analysis.");
                }
                Ok(_) => println!("No telemetry data yet. Use skills first, then review."),
                Err(e) => println!("Error loading telemetry: {e}"),
            }
        }
    }
    CommandOutcome::Continue
}

fn print_skill_review(t: &echo_agent::workspace::state::skill_telemetry::SkillTelemetry) {
    println!("\n=== Skill Review: {} ===", t.skill_name);
    println!(
        "Activations: {}  |  Success: {:.0}%  |  Avg Duration: {}ms",
        t.activation_count,
        t.success_rate() * 100.0,
        t.avg_duration_ms(),
    );

    if !t.common_tools.is_empty() {
        println!("\n🔧 Tools Used:");
        let mut tools: Vec<_> = t.common_tools.iter().collect();
        tools.sort_by(|a, b| b.1.cmp(a.1));
        for (tool, count) in tools.iter().take(10) {
            println!("  - {}: {} uses", tool, count);
        }
    }

    if !t.common_failures.is_empty() {
        println!("\n❌ Common Failures:");
        for f in &t.common_failures {
            let snippet = if f.error_snippet.len() > 80 {
                format!("{}...", &f.error_snippet[..80])
            } else {
                f.error_snippet.clone()
            };
            println!("  - [{}x] {}", f.count, snippet);
        }
    }

    if t.activation_count >= 3 {
        println!("\n📝 Analysis:");
        if t.success_rate() >= 0.9 {
            println!("  ✓ High success rate — skill instructions are effective.");
        } else if t.success_rate() >= 0.7 {
            println!("  ⚠ Moderate success rate — review failure patterns above.");
        } else {
            println!("  ✗ Low success rate — skill needs significant improvement.");
            println!("    Consider updating the SKILL.md instructions or sandbox policy.");
        }
        if t.common_tools.len() < 2 && t.activation_count > 5 {
            println!("  💡 Skill uses few tools — consider expanding tool coverage.");
        }
    }
}

cmd!(
    SkillReviewCommand,
    "skill-review",
    ["sr"],
    CommandCategory::Advanced,
    "Review skill telemetry and suggest improvements",
    cmd_skill_review
);

// ── ProfileCommand ───────────────────────────────────────────────

async fn cmd_profile(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("view");

    let store = ctx.agent.read(|a| a.store().cloned()).await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured.");
            return CommandOutcome::Continue;
        }
    };

    let profile_store = ProfileStore::new(store);

    match sub {
        "view" | "" => {
            // Show agent profile
            match profile_store.load_agent_profile().await {
                Ok(Some(profile)) => {
                    println!("\n=== Agent Profile ===");
                    let caps = profile.top_capabilities(10);
                    if !caps.is_empty() {
                        println!("Capabilities (Top {}):", caps.len());
                        for cap in &caps {
                            println!(
                                "  - {}: {:.0}% proficiency ({} uses, {:.0}% success)",
                                cap.skill_name,
                                cap.proficiency * 100.0,
                                cap.usage_count,
                                cap.success_rate * 100.0,
                            );
                        }
                    } else {
                        println!("  No capability data yet.");
                    }

                    let tools = profile.top_tools(10);
                    if !tools.is_empty() {
                        println!("\nTool Expertise (Top {}):", tools.len());
                        for tool in &tools {
                            let name = profile
                                .tool_usage
                                .iter()
                                .find(|(_, v)| std::ptr::eq(*v, *tool))
                                .map(|(k, _)| k.as_str())
                                .unwrap_or("?");
                            println!(
                                "  - {}: {} uses ({})",
                                name,
                                tool.usage_count,
                                if tool.common_skills.is_empty() {
                                    "general".to_string()
                                } else {
                                    tool.common_skills.join(", ")
                                },
                            );
                        }
                    }
                }
                Ok(None) => println!("No agent profile yet. Use /profile refresh to generate."),
                Err(e) => println!("Error loading agent profile: {e}"),
            }

            // Show user profile
            match profile_store.load_user_profile().await {
                Ok(Some(profile)) => {
                    println!("\n=== User Profile ===");
                    if !profile.preferences.is_empty() {
                        println!("Preferences:");
                        for (k, v) in &profile.preferences {
                            println!("  - {}: {}", k, v);
                        }
                    }
                    if !profile.expertise_areas.is_empty() {
                        println!("Expertise: {}", profile.expertise_areas.join(", "));
                    }
                    let tasks = profile.top_tasks(10);
                    if !tasks.is_empty() {
                        println!("Common Tasks:");
                        for t in &tasks {
                            println!("  - {} ({}x)", t.task_type, t.frequency);
                        }
                    }
                    if profile.preferences.is_empty()
                        && profile.expertise_areas.is_empty()
                        && profile.common_tasks.is_empty()
                    {
                        println!("  No user profile data yet.");
                        println!("  Use /profile set <key> <value> to add preferences.");
                    }
                }
                Ok(None) => println!("\nNo user profile yet."),
                Err(e) => println!("Error loading user profile: {e}"),
            }
        }
        "refresh" => {
            // Refresh agent profile from telemetry
            let telemetry_store = SkillTelemetryStore::new(
                ctx.agent
                    .read(|a| a.store().cloned())
                    .await
                    .expect("store should exist"),
            );
            match telemetry_store.list_all().await {
                Ok(telemetry) if !telemetry.is_empty() => {
                    let mut profile = profile_store
                        .load_agent_profile()
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    profile.update_from_telemetry(&telemetry);
                    match profile_store.save_agent_profile(&profile).await {
                        Ok(()) => {
                            println!("Agent profile refreshed from {} skill(s).", telemetry.len())
                        }
                        Err(e) => println!("Error saving profile: {e}"),
                    }
                }
                Ok(_) => println!("No telemetry data to build profile from."),
                Err(e) => println!("Error loading telemetry: {e}"),
            }
        }
        "set" => {
            if args.len() < 4 {
                println!("Usage: /profile set <key> <value>");
                return CommandOutcome::Continue;
            }
            let key = args[1];
            let value = args[2..].join(" ");
            let mut profile = profile_store
                .load_user_profile()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            profile.set_preference(key, &value);
            match profile_store.save_user_profile(&profile).await {
                Ok(()) => println!("Set preference: {} = {}", key, value),
                Err(e) => println!("Error saving preference: {e}"),
            }
        }
        "reset" => {
            match profile_store.save_agent_profile(&AgentProfile::new()).await {
                Ok(()) => {}
                Err(e) => println!("Error resetting agent profile: {e}"),
            }
            match profile_store.save_user_profile(&UserProfile::new()).await {
                Ok(()) => println!("All profiles reset."),
                Err(e) => println!("Error resetting user profile: {e}"),
            }
        }
        _ => {
            println!("Usage: /profile [view|refresh|set <key> <value>|reset]");
        }
    }
    CommandOutcome::Continue
}

cmd!(
    ProfileCommand,
    "profile",
    CommandCategory::Advanced,
    "View or manage agent/user profiles",
    cmd_profile
);

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(TrajectoriesCommand));
    registry.register(Arc::new(ReviewCommand));
    registry.register(Arc::new(CuratorCommand));
    registry.register(Arc::new(CritiquesCommand));
    registry.register(Arc::new(SkillReviewCommand));
    registry.register(Arc::new(ProfileCommand));
}
