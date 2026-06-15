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

    let reviewer = echo_agent::evolution::BackgroundReviewer::new(
        echo_agent::evolution::BackgroundReviewConfig::default(),
        llm_client,
        memory_store.clone(),
        Some(run_store),
    );
    let reviewer = if let Some(store) = memory_store {
        let review_integration = Arc::new(echo_agent_app_core::evolution::ReviewIntegration::new(
            echo_agent::evolution::ReviewConfig::default(),
            echo_agent_app_core::evolution::discover_echo_agent_dir(),
            store,
        ));
        reviewer.with_layer_manager(Arc::new(
            review_integration
                .create_layer_manager()
                .with_write_observer(review_integration),
        ))
    } else {
        reviewer
    };

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
            let store = ctx.agent.read(|a| a.store().cloned()).await;
            let store = match store {
                Some(s) => s,
                None => {
                    println!("No memory store configured. Cannot refresh profile.");
                    return CommandOutcome::Continue;
                }
            };
            let telemetry_store = SkillTelemetryStore::new(store);
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

// ── MemoryReviewCommand ─────────────────────────────────────────────

async fn cmd_memory_review(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    // Get the store from the agent — needed to create ReviewIntegration
    let store = ctx.agent.read(|a| a.store().cloned()).await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured. Cannot run memory review.");
            return CommandOutcome::Continue;
        }
    };

    let echo_agent_dir = echo_agent_app_core::evolution::discover_echo_agent_dir();
    let review_integration = echo_agent_app_core::evolution::ReviewIntegration::new(
        echo_agent::evolution::ReviewConfig::default(),
        echo_agent_dir,
        store,
    );

    println!("\n📋 Running memory review...");

    match review_integration.run_review().await {
        Ok(report) => {
            let formatted = echo_agent_app_core::evolution::format_review_report(&report);
            println!("{formatted}");
        }
        Err(e) => {
            println!("Memory review failed: {e}");
        }
    }

    CommandOutcome::Continue
}
cmd!(
    MemoryReviewCommand,
    "memory-review",
    ["mr"],
    CommandCategory::Advanced,
    "Review and clean up accumulated memories",
    cmd_memory_review
);

// ── SkillCandidatesCommand ──────────────────────────────────────────

async fn cmd_skill_candidates(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("list");

    // Load curator state to find candidates and drafts.
    let curator =
        echo_agent::improve::Curator::default_path(echo_agent::improve::CuratorConfig::default());
    let state = curator.load_state();

    let candidates_and_drafts: Vec<_> = state
        .skills
        .iter()
        .filter(|(_, meta)| {
            matches!(
                meta.lifecycle,
                echo_agent::improve::SkillLifecycle::Candidate
                    | echo_agent::improve::SkillLifecycle::Draft
            )
        })
        .collect();

    match sub {
        "detail" | "d" => {
            if candidates_and_drafts.is_empty() {
                println!("No skill candidates or drafts found.");
                println!(
                    "Candidates are created automatically when repeated patterns are detected."
                );
                println!("Run /memory-review to trigger detection.");
            } else {
                println!("\n=== Skill Candidates & Drafts (Detail) ===");
                for (name, meta) in &candidates_and_drafts {
                    let lifecycle = format!("{:?}", meta.lifecycle);
                    let created = meta.created_at.format("%Y-%m-%d %H:%M");
                    let last_used = meta.last_used_at.format("%Y-%m-%d %H:%M");
                    let auto = if meta.agent_created { "🤖" } else { "👤" };
                    println!(
                        "  {} {} [{}]  created: {}  last-used: {}",
                        auto, name, lifecycle, created, last_used
                    );
                }
            }
        }
        "list" | _ => {
            if candidates_and_drafts.is_empty() {
                println!("No skill candidates or drafts found.");
                println!(
                    "Candidates are created automatically when repeated patterns are detected."
                );
            } else {
                println!("\n=== Skill Candidates & Drafts ===");
                let candidate_count = candidates_and_drafts
                    .iter()
                    .filter(|(_, m)| {
                        matches!(m.lifecycle, echo_agent::improve::SkillLifecycle::Candidate)
                    })
                    .count();
                let draft_count = candidates_and_drafts
                    .iter()
                    .filter(|(_, m)| {
                        matches!(m.lifecycle, echo_agent::improve::SkillLifecycle::Draft)
                    })
                    .count();
                println!("  Candidates: {}  Drafts: {}", candidate_count, draft_count);
                for (name, meta) in &candidates_and_drafts {
                    let icon = match meta.lifecycle {
                        echo_agent::improve::SkillLifecycle::Candidate => "🎯",
                        echo_agent::improve::SkillLifecycle::Draft => "📝",
                        _ => "  ",
                    };
                    println!("  {} {} [{:?}]", icon, name, meta.lifecycle);
                }
                println!("\n  Run /skill-candidates detail for more info.");
                println!("  Run /skill-create <name> to generate a draft from a candidate.");
                println!("  Run /skill-promote <name> to promote a draft to Active.");
            }
        }
    }

    CommandOutcome::Continue
}
cmd!(
    SkillCandidatesCommand,
    "skill-candidates",
    ["sc"],
    CommandCategory::Advanced,
    "List skill candidates and drafts",
    cmd_skill_candidates
);

// ── SkillPromoteCommand ────────────────────────────────────────────

async fn cmd_skill_promote(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = match args.first() {
        Some(n) => *n,
        None => {
            println!("Usage: /skill-promote <name>");
            println!("Promotes a Draft skill to Active status.");
            return CommandOutcome::Continue;
        }
    };

    let curator =
        echo_agent::improve::Curator::default_path(echo_agent::improve::CuratorConfig::default());

    // Check current lifecycle state.
    let state = curator.load_state();
    match state.skills.get(name) {
        Some(meta) => match meta.lifecycle {
            echo_agent::improve::SkillLifecycle::Draft => match curator.promote_to_active(name) {
                Ok(true) => println!("✓ Skill '{}' promoted from Draft to Active.", name),
                Ok(false) => println!("Skill '{}' was not in Draft state.", name),
                Err(e) => println!("Error promoting skill: {e}"),
            },
            echo_agent::improve::SkillLifecycle::Candidate => {
                println!("Skill '{}' is a Candidate, not a Draft.", name);
                println!(
                    "Run /skill-create {} first to generate a draft SKILL.md.",
                    name
                );
            }
            echo_agent::improve::SkillLifecycle::Active => {
                println!("Skill '{}' is already Active.", name);
            }
            other => println!(
                "Skill '{}' is in {:?} state and cannot be promoted.",
                name, other
            ),
        },
        None => {
            println!("Skill '{}' not found in curator state.", name);
            println!("Run /skill-candidates to see available candidates and drafts.");
        }
    }

    CommandOutcome::Continue
}
cmd!(
    SkillPromoteCommand,
    "skill-promote",
    CommandCategory::Advanced,
    "Promote a Draft skill to Active",
    cmd_skill_promote
);

// ── SkillCreateCommand ─────────────────────────────────────────────

async fn cmd_skill_create(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = args.first().copied();

    let store = ctx.agent.read(|a| a.store().cloned()).await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured. Cannot create skill drafts.");
            return CommandOutcome::Continue;
        }
    };

    let echo_agent_dir = echo_agent_app_core::evolution::discover_echo_agent_dir();

    // If no name given, list candidates.
    let name = match name {
        Some(n) => n.to_string(),
        None => {
            // List available candidates.
            let curator = echo_agent::improve::Curator::default_path(
                echo_agent::improve::CuratorConfig::default(),
            );
            let state = curator.load_state();
            let candidates: Vec<_> = state
                .skills
                .iter()
                .filter(|(_, m)| {
                    matches!(m.lifecycle, echo_agent::improve::SkillLifecycle::Candidate)
                })
                .collect();
            if candidates.is_empty() {
                println!("No candidates available. Run /memory-review to detect patterns.");
            } else {
                println!("\nAvailable candidates:");
                for (name, _meta) in &candidates {
                    println!("  🎯 {}", name);
                }
                println!("\nRun /skill-create <name> to generate a draft.");
            }
            return CommandOutcome::Continue;
        }
    };

    // Generate draft from candidate.
    let typed_store = echo_agent::memory::TypedMemoryStore::new(store);
    let log_path = echo_agent_dir.join("evolution").join("change-log.jsonl");
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let change_log = echo_agent::evolution::JsonlChangeLog::new(log_path);
    let generator = echo_agent::evolution::SkillDraftGenerator::new(
        echo_agent_dir,
        &change_log as &dyn echo_agent::evolution::ChangeLog,
    );

    match generator.generate(&name, &typed_store).await {
        Ok(result) => {
            if result.created {
                println!("✓ Draft SKILL.md created for '{}' at:", result.name);
            } else {
                println!("✓ Draft SKILL.md updated for '{}' at:", result.name);
            }
            println!("  {}", result.skill_md_path.display());
            println!(
                "\nReview the draft, then run /skill-promote {} to activate it.",
                result.name
            );
        }
        Err(e) => {
            println!("Error creating skill draft: {e}");
            println!(
                "Make sure '{}' is a valid candidate. Run /skill-candidates to check.",
                name
            );
        }
    }

    CommandOutcome::Continue
}
cmd!(
    SkillCreateCommand,
    "skill-create",
    CommandCategory::Advanced,
    "Create a draft SKILL.md from a candidate",
    cmd_skill_create
);

// ── SkillMergeCommand ─────────────────────────────────────────────

async fn cmd_skill_merge(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.store().cloned()).await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured.");
            return CommandOutcome::Continue;
        }
    };

    // If no args, run similarity detection and show proposals
    if args.is_empty() {
        println!("Scanning skills for similarity...");
        let detector = echo_agent::evolution::SkillSimilarityDetector::new(store.clone());

        // Load all skill descriptors from registry
        let descriptors: Vec<_> = ctx
            .agent
            .read(|a| {
                a.skill_registry()
                    .list_descriptors()
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .await;

        let echo_agent_dir = echo_agent_app_core::evolution::discover_echo_agent_dir();
        let log_path = echo_agent_dir.join("evolution").join("change-log.jsonl");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let change_log = echo_agent::evolution::JsonlChangeLog::new(log_path);

        match detector.scan_and_propose(&descriptors, &change_log).await {
            Ok(proposals) if !proposals.is_empty() => {
                println!("\n=== Skill Merge Proposals ===");
                for (i, proposal) in proposals.iter().enumerate() {
                    println!(
                        "{}. {} ↔ {} (similarity: {:.2})",
                        i + 1,
                        proposal.skill_a,
                        proposal.skill_b,
                        proposal.similarity_score
                    );
                    println!(
                        "   Primary: {} | Deprecate: {}",
                        proposal.primary_skill, proposal.deprecated_skill
                    );
                    println!(
                        "   Trigger overlap: {:.2} | Path overlap: {:.2}",
                        proposal.breakdown.trigger_overlap, proposal.breakdown.path_overlap
                    );
                    println!(
                        "   Tool overlap: {:.2} | Description similarity: {:.2}",
                        proposal.breakdown.tool_overlap, proposal.breakdown.description_similarity
                    );
                    println!();
                }
                println!("Run /skill-merge <skill-a> <skill-b> to execute a merge.");
            }
            Ok(_) => {
                println!("No similar skill pairs found.");
            }
            Err(e) => {
                println!("Error scanning skills: {e}");
            }
        }

        return CommandOutcome::Continue;
    }

    // Execute merge if two skill names provided
    if args.len() == 2 {
        let skill_a = args[0];
        let skill_b = args[1];

        println!("Executing merge: {} ↔ {}...", skill_a, skill_b);

        // Load proposals from storage
        let proposal_key = format!("merge:{}:{}", skill_a, skill_b);

        match store
            .get(&["evolution", "merge_proposals"], &proposal_key)
            .await
        {
            Ok(Some(proposal_item)) => {
                let proposal: echo_agent::evolution::SkillMergeProposal =
                    match serde_json::from_value(proposal_item.value) {
                        Ok(p) => p,
                        Err(e) => {
                            println!("Error parsing proposal: {e}");
                            return CommandOutcome::Continue;
                        }
                    };

                // Get primary descriptor
                let primary_desc = ctx
                    .agent
                    .read(|a| {
                        a.skill_registry()
                            .get_descriptor(&proposal.primary_skill)
                            .cloned()
                    })
                    .await;

                let primary_desc = match primary_desc {
                    Some(d) => d,
                    None => {
                        println!(
                            "Primary skill '{}' not found in registry.",
                            proposal.primary_skill
                        );
                        return CommandOutcome::Continue;
                    }
                };

                // Get deprecated descriptor
                let deprecated_desc = ctx
                    .agent
                    .read(|a| {
                        a.skill_registry()
                            .get_descriptor(&proposal.deprecated_skill)
                            .cloned()
                    })
                    .await;

                let echo_agent_dir = echo_agent_app_core::evolution::discover_echo_agent_dir();
                let log_path = echo_agent_dir.join("evolution").join("change-log.jsonl");
                if let Some(parent) = log_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let change_log = echo_agent::evolution::JsonlChangeLog::new(log_path);

                let curator_config = echo_agent::improve::CuratorConfig::default();
                let curator = echo_agent::improve::Curator::default_path(curator_config);
                let merger = echo_agent::evolution::SkillMerger::new(store.clone(), curator);

                let mut primary_desc_mut = primary_desc;
                match merger
                    .execute_merge(
                        &proposal,
                        &mut primary_desc_mut,
                        deprecated_desc.as_ref(),
                        &change_log,
                    )
                    .await
                {
                    Ok(_) => {
                        println!("✓ Merge completed successfully.");
                        println!(
                            "  Primary skill '{}' has been updated.",
                            proposal.primary_skill
                        );
                        println!(
                            "  Secondary skill '{}' has been deprecated.",
                            proposal.deprecated_skill
                        );
                        println!(
                            "\nNote: The updated skill descriptor needs to be written back to the registry."
                        );
                        println!(
                            "This is a manual step - update the SKILL.md file for '{}' with the merged content.",
                            proposal.primary_skill
                        );
                    }
                    Err(e) => {
                        println!("Error executing merge: {e}");
                    }
                }
            }
            Ok(None) => {
                println!("No merge proposal found for {} ↔ {}.", skill_a, skill_b);
                println!("Run /skill-merge without arguments to scan for similar skills.");
            }
            Err(e) => {
                println!("Error loading proposal: {e}");
            }
        }

        return CommandOutcome::Continue;
    }

    println!("Usage:");
    println!("  /skill-merge              Scan skills and show merge proposals");
    println!("  /skill-merge <a> <b>      Execute merge of two skills");

    CommandOutcome::Continue
}
cmd!(
    SkillMergeCommand,
    "skill-merge",
    CommandCategory::Advanced,
    "Detect similar skills and propose/execute merges",
    cmd_skill_merge
);

// ── SkillHealthCommand ─────────────────────────────────────────────

async fn cmd_skill_health(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.store().cloned()).await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured.");
            return CommandOutcome::Continue;
        }
    };

    let monitor = echo_agent::evolution::SkillHealthMonitor::new(store);

    if args.is_empty() {
        // Show health overview for all skills
        println!("Analyzing skill health...");
        match monitor.analyze_all_skills().await {
            Ok(reports) if !reports.is_empty() => {
                println!("\n=== Skill Health Overview ===");

                let healthy: Vec<_> = reports
                    .iter()
                    .filter(|r| r.status == echo_agent::evolution::HealthStatus::Healthy)
                    .collect();
                let needs_attention: Vec<_> = reports
                    .iter()
                    .filter(|r| r.status == echo_agent::evolution::HealthStatus::NeedsAttention)
                    .collect();
                let unhealthy: Vec<_> = reports
                    .iter()
                    .filter(|r| r.status == echo_agent::evolution::HealthStatus::Unhealthy)
                    .collect();

                println!("  ✓ Healthy: {} skills", healthy.len());
                println!("  ⚠ Needs attention: {} skills", needs_attention.len());
                println!("  ✗ Unhealthy: {} skills", unhealthy.len());
                println!();

                if !unhealthy.is_empty() {
                    println!("Unhealthy skills:");
                    for report in unhealthy.iter().take(5) {
                        println!(
                            "  • {} (score: {:.2})",
                            report.skill_name, report.health_score
                        );
                    }
                    if unhealthy.len() > 5 {
                        println!("  ... and {} more", unhealthy.len() - 5);
                    }
                    println!();
                }

                if !needs_attention.is_empty() {
                    println!("Skills needing attention:");
                    for report in needs_attention.iter().take(5) {
                        println!(
                            "  • {} (score: {:.2})",
                            report.skill_name, report.health_score
                        );
                    }
                    if needs_attention.len() > 5 {
                        println!("  ... and {} more", needs_attention.len() - 5);
                    }
                }

                println!("\nRun /skill-health <name> for detailed analysis of a specific skill.");
            }
            Ok(_) => {
                println!("No skills with telemetry data found.");
            }
            Err(e) => {
                println!("Error analyzing skills: {e}");
            }
        }
    } else {
        // Show detailed health for specific skill
        let skill_name = args[0];
        println!("Analyzing health of '{}'...", skill_name);

        match monitor.analyze_skill(skill_name).await {
            Ok(Some(report)) => {
                println!("\n=== Skill Health: {} ===", report.skill_name);
                println!(
                    "Status: {} (score: {:.2})",
                    match report.status {
                        echo_agent::evolution::HealthStatus::Healthy => "✓ Healthy",
                        echo_agent::evolution::HealthStatus::NeedsAttention => "⚠ Needs attention",
                        echo_agent::evolution::HealthStatus::Unhealthy => "✗ Unhealthy",
                    },
                    report.health_score
                );
                println!("\nBreakdown:");
                println!("  Success rate: {:.2}", report.breakdown.success_rate);
                println!(
                    "  Recent success: {:.2}",
                    report.breakdown.recent_success_rate
                );
                println!("  Usage frequency: {:.2}", report.breakdown.usage_frequency);
                println!("  Freshness: {:.2}", report.breakdown.freshness);
                println!("  User approval: {:.2}", report.breakdown.user_approval);
                println!(
                    "  Command validity: {:.2}",
                    report.breakdown.command_validity
                );

                if !report.recommendations.is_empty() {
                    println!("\nRecommendations:");
                    for (i, rec) in report.recommendations.iter().enumerate() {
                        println!("  {}. {}", i + 1, rec);
                    }
                }
            }
            Ok(None) => {
                println!("No telemetry data found for skill '{}'.", skill_name);
            }
            Err(e) => {
                println!("Error analyzing skill: {e}");
            }
        }
    }

    CommandOutcome::Continue
}
cmd!(
    SkillHealthCommand,
    "skill-health",
    CommandCategory::Advanced,
    "Monitor skill health and get recommendations",
    cmd_skill_health
);

// ── SkillPatchCommand ─────────────────────────────────────────────

async fn cmd_skill_patch(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.store().cloned()).await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured.");
            return CommandOutcome::Continue;
        }
    };

    let patcher = echo_agent::evolution::SkillPatcher::new(store);

    if args.is_empty() {
        // Show all patch proposals
        println!("Analyzing skills for patch opportunities...");
        match patcher.analyze_all_skills().await {
            Ok(patches) if !patches.is_empty() => {
                println!("\n=== Skill Patch Proposals ===");
                for (i, patch) in patches.iter().enumerate() {
                    println!(
                        "{}. {} ({})",
                        i + 1,
                        patch.skill_name,
                        match &patch.patch_type {
                            echo_agent::evolution::PatchType::ErrorHandling { .. } =>
                                "Error handling",
                            echo_agent::evolution::PatchType::PrerequisiteCheck { .. } =>
                                "Prerequisite check",
                            echo_agent::evolution::PatchType::FallbackStrategy { .. } =>
                                "Fallback strategy",
                            echo_agent::evolution::PatchType::InstructionEnhancement { .. } =>
                                "Instruction enhancement",
                        }
                    );
                    println!(
                        "   Confidence: {:.2} | Priority: {}",
                        patch.confidence, patch.priority
                    );
                    println!("   {}\n", patch.rationale);
                }
                println!("Run /skill-patch <name> to see patches for a specific skill.");
            }
            Ok(_) => {
                println!("No patch opportunities found. All skills are performing well.");
            }
            Err(e) => {
                println!("Error analyzing skills: {e}");
            }
        }
    } else {
        // Show patches for specific skill
        let skill_name = args[0];
        println!("Analyzing '{}' for patch opportunities...", skill_name);

        match patcher.analyze_and_propose(skill_name).await {
            Ok(patches) if !patches.is_empty() => {
                println!("\n=== Patches for {} ===", skill_name);
                for (i, patch) in patches.iter().enumerate() {
                    println!("\n{}. {}", i + 1, patch.summary());
                }
                println!(
                    "\nNote: These are proposals. To apply them, manually update the SKILL.md file."
                );
                println!("Future versions may support automatic patch application.");
            }
            Ok(_) => {
                println!("No patch opportunities found for '{}'.", skill_name);
            }
            Err(e) => {
                println!("Error analyzing skill: {e}");
            }
        }
    }

    CommandOutcome::Continue
}
cmd!(
    SkillPatchCommand,
    "skill-patch",
    CommandCategory::Advanced,
    "Generate patch proposals to improve skills",
    cmd_skill_patch
);

// ── RulePromoteCommand ────────────────────────────────────────────

async fn cmd_rule_promote(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.store().cloned()).await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured.");
            return CommandOutcome::Continue;
        }
    };

    let promoter = echo_agent_app_core::evolution::RulePromoter::new(store);

    match args.first().copied() {
        Some("scan") | None => {
            println!("Scanning memories for rule promotion candidates...");
            let proposals = promoter.scan_for_proposals().await;

            if proposals.is_empty() {
                println!("\nNo memories meet the promotion criteria.");
                println!(
                    "Criteria: confidence >= {:.2}, age >= {} days",
                    promoter.criteria().min_confidence,
                    promoter.criteria().min_age_days
                );
            } else {
                println!("\n=== Rule Promotion Candidates ===\n");
                for (i, proposal) in proposals.iter().enumerate() {
                    println!(
                        "{}. [{}] (confidence: {:.2})",
                        i + 1,
                        proposal.memory_key,
                        proposal.confidence
                    );
                    println!("   Type: {:?}", proposal.memory_type);
                    println!("   Reason: {}", proposal.reason);
                    println!("   Rule: {}", proposal.rule_text);
                    println!();
                }

                println!("To promote a specific rule, use: /rule-promote <memory_key>");
            }
        }
        Some(memory_key) => {
            println!("Promoting memory '{}' to rule...", memory_key);

            let proposals = promoter.scan_for_proposals().await;
            let proposal = proposals.iter().find(|p| p.memory_key == memory_key);

            match proposal {
                Some(prop) => {
                    let change_log = echo_agent::evolution::JsonlChangeLog::new(
                        echo_agent_app_core::evolution::discover_echo_agent_dir()
                            .join("evolution")
                            .join("changelog.jsonl"),
                    );

                    match promoter.promote_rule(prop, &change_log).await {
                        Ok(()) => {
                            println!(
                                "✓ Successfully promoted memory '{}' to AGENTS.md",
                                memory_key
                            );
                        }
                        Err(e) => {
                            println!("✗ Failed to promote rule: {}", e);
                        }
                    }
                }
                None => {
                    println!(
                        "Memory '{}' not found or does not meet promotion criteria.",
                        memory_key
                    );
                    println!("Run /rule-promote scan to see available candidates.");
                }
            }
        }
    }

    CommandOutcome::Continue
}
cmd!(
    RulePromoteCommand,
    "rule-promote",
    CommandCategory::Advanced,
    "Promote high-confidence memories to agent rules in AGENTS.md",
    cmd_rule_promote
);

// ── EvolutionDashboardCommand ─────────────────────────────────────

async fn cmd_evolution_dashboard(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    let store = ctx.agent.read(|a| a.store().cloned()).await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured.");
            return CommandOutcome::Continue;
        }
    };

    let change_log = echo_agent::evolution::JsonlChangeLog::new(
        echo_agent_app_core::evolution::discover_echo_agent_dir()
            .join("evolution")
            .join("changelog.jsonl"),
    );

    let dashboard = echo_agent_app_core::evolution::Dashboard::new(store, change_log);

    println!("Generating evolution dashboard...\n");

    let metrics = dashboard.generate_metrics().await;
    let output = echo_agent_app_core::evolution::Dashboard::format_metrics(&metrics);

    println!("{}", output);

    CommandOutcome::Continue
}
cmd!(
    EvolutionDashboardCommand,
    "evolution-dashboard",
    CommandCategory::Advanced,
    "Display evolution system metrics and status overview",
    cmd_evolution_dashboard
);

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(TrajectoriesCommand));
    registry.register(Arc::new(ReviewCommand));
    registry.register(Arc::new(CuratorCommand));
    registry.register(Arc::new(CritiquesCommand));
    registry.register(Arc::new(SkillReviewCommand));
    registry.register(Arc::new(ProfileCommand));
    registry.register(Arc::new(MemoryReviewCommand));
    registry.register(Arc::new(SkillCandidatesCommand));
    registry.register(Arc::new(SkillPromoteCommand));
    registry.register(Arc::new(SkillCreateCommand));
    registry.register(Arc::new(SkillMergeCommand));
    registry.register(Arc::new(SkillHealthCommand));
    registry.register(Arc::new(SkillPatchCommand));
    registry.register(Arc::new(RulePromoteCommand));
    registry.register(Arc::new(EvolutionDashboardCommand));
}
