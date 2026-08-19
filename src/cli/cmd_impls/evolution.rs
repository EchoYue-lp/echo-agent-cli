//! Memory, rule, and skill evolution slash commands.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

use echo_agent::workspace::state::profiles::{AgentProfile, ProfileStore, UserProfile};
use echo_agent::workspace::state::skill_telemetry::SkillTelemetryStore;

fn current_echo_agent_dir(ctx: &CommandContext) -> std::path::PathBuf {
    ctx.review_integration
        .as_ref()
        .map(|integration| integration.echo_agent_dir())
        .unwrap_or_else(echo_agent_app_core::evolution::discover_echo_agent_dir)
}

fn current_curator(ctx: &CommandContext) -> echo_agent::evolution::Curator {
    ctx.review_integration
        .as_ref()
        .map(|integration| integration.curator())
        .unwrap_or_else(|| {
            echo_agent_app_core::evolution::workspace_curator(&current_echo_agent_dir(ctx))
        })
}

fn current_evidence_store(ctx: &CommandContext) -> echo_agent_app_core::evolution::EvidenceStore {
    ctx.review_integration
        .as_ref()
        .map(|integration| integration.evidence_store())
        .unwrap_or_else(|| {
            echo_agent_app_core::evolution::EvidenceStore::new(current_echo_agent_dir(ctx))
        })
}

fn evolution_write_lease(
    ctx: &CommandContext,
) -> Result<echo_agent_app_core::evolution::ReviewGenerationLease, String> {
    ctx.review_integration
        .as_ref()
        .ok_or_else(|| "Review integration is not configured".to_string())?
        .lease_generation()
        .map_err(|error| error.to_string())
}

fn evidence_write_binding(
    ctx: &CommandContext,
) -> Result<
    (
        echo_agent_app_core::evolution::EvidenceStore,
        echo_agent_app_core::evolution::ReviewGenerationLease,
    ),
    String,
> {
    let lease = evolution_write_lease(ctx)?;
    let store = lease.evidence_store();
    Ok((store, lease))
}

// ── ReviewCommand ───────────────────────────────────────────────────

async fn cmd_review(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    let Some(review_integration) = ctx.review_integration.as_ref() else {
        println!("Review integration is not configured.");
        return CommandOutcome::Continue;
    };
    let review_lease = match review_integration.lease_generation() {
        Ok(lease) => lease,
        Err(error) => {
            println!("Review unavailable during workspace transition: {error}");
            return CommandOutcome::Continue;
        }
    };

    // Snapshot the run plumbing only after memory generation admission so a
    // workspace transition cannot replace it midway through this review pass.
    let (run_store, llm_client) = ctx
        .agent
        .read(|a| (a.run_store.clone(), a.llm_client().cloned()))
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
        run.run_id.chars().take(12).collect::<String>()
    );

    let reviewer = echo_agent::evolution::BackgroundReviewer::new(
        echo_agent::evolution::BackgroundReviewConfig::default(),
        llm_client,
        Some(review_lease.memory_store()),
        Some(run_store),
    );
    let layer_manager = match review_lease.create_layer_manager() {
        Ok(manager) => Arc::new(manager),
        Err(error) => {
            println!("Review memory initialization failed: {error}");
            return CommandOutcome::Continue;
        }
    };
    let reviewer = reviewer.with_layer_manager(layer_manager);

    let handle = match reviewer.review(&run) {
        Ok(handle) => handle,
        Err(error) => {
            println!("Review failed: {error}");
            return CommandOutcome::Continue;
        }
    };
    let settled = match review_lease.track_background_review(handle).await {
        Ok(mut pass) => pass.settle().await,
        Err(error) => Err(error),
    };
    match settled {
        Ok(settlement) => {
            let outcome = settlement.outcome;
            if outcome.nothing_to_save {
                println!("Nothing to save.");
            } else {
                println!("Review candidates:");
                for action in &outcome.actions {
                    println!("  - {action}");
                }
                if let Some(candidate) = &outcome.candidate {
                    println!("  Evidence: {}", candidate.evidence);
                    println!("  Confidence: {:.2}", candidate.confidence);
                }
            }
            if let Some(candidate) = settlement.evidence_candidate {
                println!(
                    "  Inbox: {} ({:?})",
                    candidate.candidate_id, candidate.status
                );
            }
            if let Some(ref err) = outcome.error {
                println!("Warning: {err}");
            }
        }
        Err(error) => println!("Review task failed: {error}"),
    }

    CommandOutcome::Continue
}
cmd!(
    ReviewCommand,
    "review",
    CommandCategory::Advanced,
    "Propose evidence-linked memory candidates from the last run",
    cmd_review
);

// ── CuratorCommand ─────────────────────────────────────────────────

async fn cmd_curator(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("status");

    match sub {
        "status" => {
            let curator = current_curator(ctx);
            let status = match curator.status() {
                Ok(status) => status,
                Err(error) => {
                    eprintln!("Curator state unavailable: {error}");
                    return CommandOutcome::Continue;
                }
            };
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
        "run" => {
            let generation = match evolution_write_lease(ctx) {
                Ok(generation) => generation,
                Err(error) => {
                    println!("Curator unavailable during workspace transition: {error}");
                    return CommandOutcome::Continue;
                }
            };
            let curator =
                echo_agent_app_core::evolution::workspace_curator(generation.echo_agent_dir());
            match curator.apply_transitions() {
                Ok(transitions) if !transitions.is_empty() => {
                    ctx.agent
                        .write_async(|agent| {
                            Box::pin(async move {
                                agent.reconcile_skill_load_policy().await;
                            })
                        })
                        .await;
                    println!("Applied {} transition(s):", transitions.len());
                    for (name, from, to) in &transitions {
                        println!("  {name}: {from:?} → {to:?}");
                        echo_agent_app_core::evolution::fire_evolution_hook(
                            &ctx.agent,
                            echo_core::hooks::HookEvent::SkillLifecycleTransition,
                            name,
                        )
                        .await;
                    }
                }
                Ok(_) => println!("No transitions needed."),
                Err(e) => println!("Error applying transitions: {e}"),
            }
        }
        "pin" => {
            if let Some(name) = args.get(1) {
                let generation = match evolution_write_lease(ctx) {
                    Ok(generation) => generation,
                    Err(error) => {
                        println!("Curator unavailable during workspace transition: {error}");
                        return CommandOutcome::Continue;
                    }
                };
                let curator =
                    echo_agent_app_core::evolution::workspace_curator(generation.echo_agent_dir());
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
                let generation = match evolution_write_lease(ctx) {
                    Ok(generation) => generation,
                    Err(error) => {
                        println!("Curator unavailable during workspace transition: {error}");
                        return CommandOutcome::Continue;
                    }
                };
                let curator =
                    echo_agent_app_core::evolution::workspace_curator(generation.echo_agent_dir());
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
                                    println!(
                                        "  • {} ({:?})",
                                        run.run_id.chars().take(12).collect::<String>(),
                                        run.status
                                    );
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
            let snippet = if f.error_snippet.chars().count() > 80 {
                format!(
                    "{}...",
                    f.error_snippet.chars().take(80).collect::<String>()
                )
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

    let writes_memory = matches!(sub, "refresh" | "set" | "reset");
    let memory_generation = if writes_memory {
        let Some(integration) = ctx.review_integration.as_ref() else {
            println!("Review integration is not configured.");
            return CommandOutcome::Continue;
        };
        match integration.lease_generation() {
            Ok(generation) => Some(generation),
            Err(error) => {
                println!("Profile update unavailable during workspace transition: {error}");
                return CommandOutcome::Continue;
            }
        }
    } else {
        None
    };
    let store = match memory_generation.as_ref() {
        Some(generation) => Some(generation.memory_store()),
        None => ctx.agent.read(|a| a.store().cloned()).await,
    };
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured.");
            return CommandOutcome::Continue;
        }
    };

    let profile_store = ProfileStore::new(store.clone());

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
            if args.len() < 3 {
                println!("Usage: /profile set <key> <value>");
                return CommandOutcome::Continue;
            }
            let Some(key) = args.get(1).copied() else {
                println!("Usage: /profile set <key> <value>");
                return CommandOutcome::Continue;
            };
            let value = args.get(2..).unwrap_or_default().join(" ");
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
    let review_integration = match ctx.review_integration.as_ref() {
        Some(integration) => integration,
        None => {
            println!("Memory review integration is not configured for this agent.");
            return CommandOutcome::Continue;
        }
    };

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

async fn cmd_skill_candidates(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("list");

    // Load curator state to find candidates and drafts.
    let curator = current_curator(ctx);
    let state = match curator.load_state() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("Curator state unavailable: {error}");
            return CommandOutcome::Continue;
        }
    };

    let candidates_and_drafts: Vec<_> = state
        .skills
        .iter()
        .filter(|(_, meta)| {
            matches!(
                meta.lifecycle,
                echo_agent::evolution::SkillLifecycle::Candidate
                    | echo_agent::evolution::SkillLifecycle::Draft
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
        _ => {
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
                        matches!(
                            m.lifecycle,
                            echo_agent::evolution::SkillLifecycle::Candidate
                        )
                    })
                    .count();
                let draft_count = candidates_and_drafts
                    .iter()
                    .filter(|(_, m)| {
                        matches!(m.lifecycle, echo_agent::evolution::SkillLifecycle::Draft)
                    })
                    .count();
                println!("  Candidates: {}  Drafts: {}", candidate_count, draft_count);
                for (name, meta) in &candidates_and_drafts {
                    let icon = match meta.lifecycle {
                        echo_agent::evolution::SkillLifecycle::Candidate => "🎯",
                        echo_agent::evolution::SkillLifecycle::Draft => "📝",
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

async fn cmd_skill_promote(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = match args.first() {
        Some(n) => *n,
        None => {
            println!("Usage: /skill-promote <name>");
            println!("Promotes a Draft skill to Active status.");
            return CommandOutcome::Continue;
        }
    };

    let generation = match evolution_write_lease(ctx) {
        Ok(generation) => generation,
        Err(error) => {
            println!("Skill promotion unavailable during workspace transition: {error}");
            return CommandOutcome::Continue;
        }
    };
    let echo_agent_dir = generation.echo_agent_dir().to_path_buf();
    let curator = echo_agent_app_core::evolution::workspace_curator(&echo_agent_dir);

    // Check current lifecycle state.
    let state = match curator.load_state() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("Curator state unavailable: {error}");
            return CommandOutcome::Continue;
        }
    };
    match state.skills.get(name) {
        Some(meta) => match meta.lifecycle {
            echo_agent::evolution::SkillLifecycle::Draft => {
                let draft_path = echo_agent_dir
                    .join("skills")
                    .join("_drafts")
                    .join(name)
                    .join("SKILL.md");
                let active_dir = echo_agent_dir.join("skills").join(name);
                let active_path = active_dir.join("SKILL.md");
                let copy_result = std::fs::create_dir_all(&active_dir)
                    .and_then(|_| std::fs::copy(&draft_path, &active_path));
                match copy_result {
                    Ok(_) => match curator.promote_to_active_at(name, Some(&active_path)) {
                        Ok(true) => {
                            let load_root = active_dir
                                .parent()
                                .map(std::path::Path::to_path_buf)
                                .unwrap_or(active_dir.clone());
                            match ctx
                                .agent
                                .write_async(|agent| {
                                    Box::pin(
                                        async move { agent.load_skills_from_dir(load_root).await },
                                    )
                                })
                                .await
                            {
                                Ok(_) => println!(
                                    "✓ Skill '{}' promoted from Draft to Active and loaded.",
                                    name
                                ),
                                Err(error) => println!(
                                    "Skill '{}' is active, but runtime load failed: {error}",
                                    name
                                ),
                            }
                        }
                        Ok(false) => {
                            let _ = std::fs::remove_file(&active_path);
                            println!("Skill '{}' was not in Draft state.", name);
                        }
                        Err(e) => {
                            let _ = std::fs::remove_file(&active_path);
                            println!("Error promoting skill: {e}");
                        }
                    },
                    Err(error) => println!("Failed to activate draft skill: {error}"),
                }
            }
            echo_agent::evolution::SkillLifecycle::Candidate => {
                println!("Skill '{}' is a Candidate, not a Draft.", name);
                println!(
                    "Run /skill-create {} first to generate a draft SKILL.md.",
                    name
                );
            }
            echo_agent::evolution::SkillLifecycle::Active => {
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

    // If no name given, list candidates.
    let name = match name {
        Some(n) => n.to_string(),
        None => {
            // List available candidates.
            let curator = current_curator(ctx);
            let state = match curator.load_state() {
                Ok(state) => state,
                Err(error) => {
                    eprintln!("Curator state unavailable: {error}");
                    return CommandOutcome::Continue;
                }
            };
            let candidates: Vec<_> = state
                .skills
                .iter()
                .filter(|(_, m)| {
                    matches!(
                        m.lifecycle,
                        echo_agent::evolution::SkillLifecycle::Candidate
                    )
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

    let generation = match evolution_write_lease(ctx) {
        Ok(generation) => generation,
        Err(error) => {
            println!("Skill draft generation unavailable during workspace transition: {error}");
            return CommandOutcome::Continue;
        }
    };
    let echo_agent_dir = generation.echo_agent_dir().to_path_buf();
    let store = generation.memory_store();
    let curator = echo_agent_app_core::evolution::workspace_curator(&echo_agent_dir);

    // Generate draft from candidate.
    let typed_store = echo_agent::memory::TypedMemoryStore::new(store);
    let log_path = echo_agent_dir.join("evolution").join("change-log.jsonl");
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let change_log = match echo_agent::evolution::JsonlChangeLog::new(log_path) {
        Ok(change_log) => change_log,
        Err(error) => {
            println!("Failed to open evolution change log: {error}");
            return CommandOutcome::Continue;
        }
    };
    let generator = echo_agent::evolution::SkillDraftGenerator::new(
        echo_agent_dir,
        &change_log as &dyn echo_agent::evolution::ChangeLog,
    )
    .with_curator(curator);

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
    if !args.is_empty() && args.len() != 2 {
        println!("Usage:");
        println!("  /skill-merge              Scan skills and show merge proposals");
        println!("  /skill-merge <a> <b>      Execute merge of two skills");
        return CommandOutcome::Continue;
    }

    // Similarity detection persists proposals, so both scan and execute are
    // mutations and must use one pinned workspace binding through settlement.
    let generation = match evolution_write_lease(ctx) {
        Ok(generation) => generation,
        Err(error) => {
            println!("Skill merge unavailable during workspace transition: {error}");
            return CommandOutcome::Continue;
        }
    };
    let store = generation.memory_store();
    let echo_agent_dir = generation.echo_agent_dir().to_path_buf();
    let curator = echo_agent_app_core::evolution::workspace_curator(&echo_agent_dir);

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

        let log_path = echo_agent_dir.join("evolution").join("change-log.jsonl");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let change_log = match echo_agent::evolution::JsonlChangeLog::new(log_path) {
            Ok(change_log) => change_log,
            Err(error) => {
                println!("Failed to open evolution change log: {error}");
                return CommandOutcome::Continue;
            }
        };

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
        let (Some(skill_a), Some(skill_b)) = (args.first().copied(), args.get(1).copied()) else {
            println!("Usage: /skill-merge <skill-a> <skill-b>");
            return CommandOutcome::Continue;
        };

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

                let log_path = echo_agent_dir.join("evolution").join("change-log.jsonl");
                if let Some(parent) = log_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let change_log = match echo_agent::evolution::JsonlChangeLog::new(log_path) {
                    Ok(change_log) => change_log,
                    Err(error) => {
                        println!("Failed to open evolution change log: {error}");
                        return CommandOutcome::Continue;
                    }
                };

                let merger = echo_agent::evolution::SkillMerger::new(curator);

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
                        // Fire SkillMergeApplied hook so registered hooks
                        // are notified of the skill merge.
                        echo_agent_app_core::evolution::fire_evolution_hook(
                            &ctx.agent,
                            echo_core::hooks::HookEvent::SkillMergeApplied,
                            &proposal.primary_skill,
                        )
                        .await;
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

    let observer = echo_agent_app_core::evolution::evolution_hook_observer(&ctx.agent).await;
    let monitor =
        echo_agent::evolution::SkillHealthMonitor::new(store).with_evolution_observer(observer);

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
        let Some(skill_name) = args.first().copied() else {
            println!("Usage: /skill-health <name>");
            return CommandOutcome::Continue;
        };
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
    let memory_generation = if args.get(1).copied() == Some("apply") {
        match evolution_write_lease(ctx) {
            Ok(generation) => Some(generation),
            Err(error) => {
                println!("Skill patch unavailable during workspace transition: {error}");
                return CommandOutcome::Continue;
            }
        }
    } else {
        None
    };
    let store = match memory_generation.as_ref() {
        Some(generation) => generation.memory_store(),
        None => match ctx.agent.read(|a| a.store().cloned()).await {
            Some(store) => store,
            None => {
                println!("No memory store configured.");
                return CommandOutcome::Continue;
            }
        },
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
                        patch.patch_type.label()
                    );
                    println!(
                        "   Confidence: {:.2} | Priority: {}",
                        patch.confidence, patch.priority
                    );
                    println!("   {}\n", patch.rationale);
                }
                println!("Run /skill-patch <name> to see patches for a specific skill.");
                println!("Run /skill-patch <name> apply <index> to apply a specific patch.");
            }
            Ok(_) => {
                println!("No patch opportunities found. All skills are performing well.");
            }
            Err(e) => {
                println!("Error analyzing skills: {e}");
            }
        }
        return CommandOutcome::Continue;
    }

    let Some(skill_name) = args.first().copied() else {
        return CommandOutcome::Continue;
    };

    // Sub-command: apply <index>
    if args.get(1).copied() == Some("apply") {
        let Some(index_arg) = args.get(2).copied() else {
            println!("Usage: /skill-patch <name> apply <index>");
            return CommandOutcome::Continue;
        };
        let idx: usize = match index_arg.parse() {
            Ok(n) => n,
            Err(_) => {
                println!("Invalid index '{index_arg}'. Must be a number.");
                return CommandOutcome::Continue;
            }
        };
        if idx == 0 {
            println!("Index must be >= 1.");
            return CommandOutcome::Continue;
        }

        // Get patches for the skill.
        let patches = match patcher.analyze_and_propose(skill_name).await {
            Ok(p) => p,
            Err(e) => {
                println!("Error analyzing skill: {e}");
                return CommandOutcome::Continue;
            }
        };
        if patches.is_empty() {
            println!("No patch opportunities found for '{}'.", skill_name);
            return CommandOutcome::Continue;
        }
        if idx > patches.len() {
            println!("Index {} out of range (1-{}).", idx, patches.len());
            return CommandOutcome::Continue;
        }

        let Some(patch) = patches.get(idx.saturating_sub(1)).cloned() else {
            println!("Index {idx} is no longer available.");
            return CommandOutcome::Continue;
        };

        // Get the SkillDescriptor (provides .location = SKILL.md path).
        // Clone the full descriptor — we only need .location for apply_patch.
        let descriptor_opt = ctx
            .agent
            .read(|a| a.skill_registry().get_descriptor(skill_name).cloned())
            .await;

        let Some(descriptor) = descriptor_opt else {
            println!(
                "Skill '{}' not found in registry. Activate it first or check the name.",
                skill_name
            );
            return CommandOutcome::Continue;
        };

        // Create change log.
        let Some(memory_generation) = memory_generation.as_ref() else {
            println!("Skill patch generation admission was lost.");
            return CommandOutcome::Continue;
        };
        let echo_agent_dir = memory_generation.echo_agent_dir();
        let log_path = echo_agent_dir.join("evolution").join("change-log.jsonl");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let change_log = match echo_agent::evolution::JsonlChangeLog::new(log_path) {
            Ok(change_log) => change_log,
            Err(error) => {
                println!("Failed to open evolution change log: {error}");
                return CommandOutcome::Continue;
            }
        };

        let patch = match patch.bind_to_source(&descriptor.location).await {
            Ok(patch) => patch,
            Err(error) => {
                println!("Failed to bind patch to current source: {error}");
                return CommandOutcome::Continue;
            }
        };

        println!(
            "Applying patch #{} ({}) to '{}'...",
            idx,
            patch.patch_type.label(),
            skill_name
        );
        match patcher.apply_patch(&patch, &descriptor, &change_log).await {
            Ok(()) => {
                println!("✓ Patch applied to {}", descriptor.location.display());
                // Fire SkillPatchApplied hook.
                echo_agent_app_core::evolution::fire_evolution_hook(
                    &ctx.agent,
                    echo_core::hooks::HookEvent::SkillPatchApplied,
                    skill_name,
                )
                .await;
            }
            Err(e) => {
                println!("✗ Failed to apply patch: {e}");
            }
        }
        return CommandOutcome::Continue;
    }

    // Default: show patches for a specific skill.
    println!("Analyzing '{}' for patch opportunities...", skill_name);

    match patcher.analyze_and_propose(skill_name).await {
        Ok(patches) if !patches.is_empty() => {
            println!("\n=== Patches for {} ===", skill_name);
            for (i, patch) in patches.iter().enumerate() {
                println!("\n{}. {}", i + 1, patch.summary());
            }
            println!(
                "\nTo apply a patch: /skill-patch {} apply <index>",
                skill_name
            );
        }
        Ok(_) => {
            println!("No patch opportunities found for '{}'.", skill_name);
        }
        Err(e) => {
            println!("Error analyzing skill: {e}");
        }
    }

    CommandOutcome::Continue
}
cmd!(
    SkillPatchCommand,
    "skill-patch",
    CommandCategory::Advanced,
    "Generate and apply patches to improve skills based on telemetry",
    cmd_skill_patch
);

// ── RulePromoteCommand ────────────────────────────────────────────

async fn cmd_rule_promote(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let integration = match ctx.review_integration.as_ref() {
        Some(integration) => integration,
        None => {
            println!("Review integration is not configured.");
            return CommandOutcome::Continue;
        }
    };

    match args.first().copied() {
        Some("scan") | None => {
            println!("Scanning memories for rule promotion candidates...");
            let proposals = match integration.scan_rule_proposals().await {
                Ok(proposals) => proposals,
                Err(error) => {
                    println!("Rule promotion scan failed: {error}");
                    return CommandOutcome::Continue;
                }
            };

            if proposals.is_empty() {
                println!("\nNo memories meet the promotion criteria.");
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

            let proposals = match integration.scan_rule_proposals().await {
                Ok(proposals) => proposals,
                Err(error) => {
                    println!("Rule promotion scan failed: {error}");
                    return CommandOutcome::Continue;
                }
            };
            let proposal = proposals.iter().find(|p| p.memory_key == memory_key);

            match proposal {
                Some(proposal) => match integration.promote_rule(proposal).await {
                    Ok(receipt) => {
                        println!(
                            "Successfully promoted memory '{}' as {}",
                            memory_key, receipt.promotion_id
                        );
                    }
                    Err(error) => println!("Failed to promote rule: {error}"),
                },
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
    let (store, run_store) = ctx
        .agent
        .read(|a| (a.store().cloned(), a.run_store.clone()))
        .await;
    let store = match store {
        Some(s) => s,
        None => {
            println!("No memory store configured.");
            return CommandOutcome::Continue;
        }
    };

    let change_log = match echo_agent::evolution::JsonlChangeLog::new(
        current_echo_agent_dir(ctx)
            .join("evolution")
            .join("change-log.jsonl"),
    ) {
        Ok(change_log) => change_log,
        Err(error) => {
            println!("Failed to open evolution change log: {error}");
            return CommandOutcome::Continue;
        }
    };

    let dashboard =
        echo_agent_app_core::evolution::Dashboard::new(store, change_log).with_run_store(run_store);

    println!("Generating evolution dashboard...\n");

    let metrics = dashboard.generate_metrics().await;
    let output = echo_agent_app_core::evolution::Dashboard::format_metrics(&metrics);

    println!("{}", output);
    if let Some(integration) = ctx.review_integration.as_ref() {
        let delivery = integration.trigger_delivery_status();
        if delivery.pending != 0 || delivery.failures != 0 || delivery.rejected != 0 {
            println!(
                "\nMemory trigger delivery: {} pending, {} failed attempt(s), {} rejected",
                delivery.pending, delivery.failures, delivery.rejected
            );
            if let Some(error) = delivery.last_error {
                println!("  Last delivery error: {error}");
            }
        }
    }

    CommandOutcome::Continue
}
cmd!(
    EvolutionDashboardCommand,
    "evolution-dashboard",
    CommandCategory::Advanced,
    "Display on-demand evolution diagnostics",
    cmd_evolution_dashboard
);

// ── Register ────────────────────────────────────────────────────────

// ── SkillRegisterCommand ──────────────────────────────────────────

/// Register a manually-created SKILL.md as a user-owned skill.
/// The curator will never auto-transition (archive/deprecate) it because
/// `agent_created` is set to `false` (curator.rs:467 skips non-agent skills).
async fn cmd_skill_register(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = match args.first() {
        Some(n) => *n,
        None => {
            println!("Usage: /skill-register <name>");
            println!("Register an existing SKILL.md as user-created.");
            println!("The curator will not auto-archive or deprecate user-created skills.");
            return CommandOutcome::Continue;
        }
    };

    let generation = match evolution_write_lease(ctx) {
        Ok(generation) => generation,
        Err(error) => {
            println!("Skill registration unavailable during workspace transition: {error}");
            return CommandOutcome::Continue;
        }
    };
    let echo_agent_dir = generation.echo_agent_dir();
    let curator = echo_agent_app_core::evolution::workspace_curator(echo_agent_dir);
    let skill_path = echo_agent_dir.join("skills").join(name).join("SKILL.md");
    let path = skill_path.exists().then_some(skill_path.as_path());
    match curator.touch_skill_at(name, path, false) {
        Ok(()) => {
            println!(
                "✓ Skill '{}' registered as user-created (agent_created=false).",
                name
            );
            println!("  The curator will not auto-transition this skill.");
            println!("  Use /skill-pin <name> for additional protection.");
        }
        Err(e) => {
            println!("Error registering skill: {e}");
        }
    }

    CommandOutcome::Continue
}
cmd!(
    SkillRegisterCommand,
    "skill-register",
    CommandCategory::Advanced,
    "Register a manually-created SKILL.md as user-created (curator won't auto-manage)",
    cmd_skill_register
);

// ── SkillPinCommand ───────────────────────────────────────────────

/// Pin a skill so it is exempt from all curator auto-transitions.
async fn cmd_skill_pin(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = match args.first() {
        Some(n) => *n,
        None => {
            println!("Usage: /skill-pin <name>");
            println!("Pin a skill — exempt from auto-archival/deprecation.");
            return CommandOutcome::Continue;
        }
    };

    let generation = match evolution_write_lease(ctx) {
        Ok(generation) => generation,
        Err(error) => {
            println!("Skill pin unavailable during workspace transition: {error}");
            return CommandOutcome::Continue;
        }
    };
    let curator = echo_agent_app_core::evolution::workspace_curator(generation.echo_agent_dir());

    match curator.pin_skill(name) {
        Ok(()) => println!("✓ Skill '{}' pinned — exempt from auto-transitions.", name),
        Err(e) => println!("Error pinning skill: {e}"),
    }

    CommandOutcome::Continue
}
cmd!(
    SkillPinCommand,
    "skill-pin",
    CommandCategory::Advanced,
    "Pin a skill — exempt from curator auto-transitions",
    cmd_skill_pin
);

// ── SkillUnpinCommand ─────────────────────────────────────────────

/// Unpin a skill, restoring curator auto-transition eligibility.
async fn cmd_skill_unpin(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = match args.first() {
        Some(n) => *n,
        None => {
            println!("Usage: /skill-unpin <name>");
            println!("Unpin a skill — restore curator auto-transition eligibility.");
            return CommandOutcome::Continue;
        }
    };

    let generation = match evolution_write_lease(ctx) {
        Ok(generation) => generation,
        Err(error) => {
            println!("Skill unpin unavailable during workspace transition: {error}");
            return CommandOutcome::Continue;
        }
    };
    let curator = echo_agent_app_core::evolution::workspace_curator(generation.echo_agent_dir());

    match curator.unpin_skill(name) {
        Ok(()) => println!(
            "✓ Skill '{}' unpinned — curator may auto-transition it.",
            name
        ),
        Err(e) => println!("Error unpinning skill: {e}"),
    }

    CommandOutcome::Continue
}
cmd!(
    SkillUnpinCommand,
    "skill-unpin",
    CommandCategory::Advanced,
    "Unpin a skill — restore curator auto-transition eligibility",
    cmd_skill_unpin
);

// ── EvidenceInboxCommand ────────────────────────────────────────────

async fn cmd_evidence_inbox(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    use echo_agent_app_core::evolution::EvidenceReviewFilter;

    let store = current_evidence_store(ctx);
    let sub = args.first().copied().unwrap_or("list");
    match sub {
        "list" | "ls" | "pending" | "expired" | "stale" | "applied" | "undoable" => {
            let filter = match sub {
                "expired" | "stale" => EvidenceReviewFilter::Expired,
                "applied" | "undoable" => EvidenceReviewFilter::Undoable,
                _ => EvidenceReviewFilter::Pending,
            };
            match store.review_items() {
                Ok(candidates) => {
                    let visible: Vec<_> = candidates
                        .into_iter()
                        .filter(|candidate| filter.matches(candidate))
                        .collect();
                    if visible.is_empty() {
                        println!("Review Inbox is empty for this filter.");
                    } else {
                        println!("\n--- Review Inbox ({}) ---", visible.len());
                        for item in visible {
                            let candidate = item.candidate;
                            let state = if item.expired {
                                "Expired"
                            } else if matches!(
                                candidate.status,
                                echo_agent_app_core::evolution::EvidenceCandidateStatus::Applied
                            ) {
                                "Undoable"
                            } else {
                                "Ready"
                            };
                            println!(
                                "{} [{} / {:?}] {:.2} {}",
                                candidate.candidate_id,
                                state,
                                candidate.kind,
                                candidate.confidence,
                                candidate.content
                            );
                        }
                    }
                }
                Err(error) => println!("Failed to read Review Inbox: {error}"),
            }
        }
        "show" => {
            let Some(candidate_id) = args.get(1) else {
                println!("Usage: /evidence-inbox show <candidate-id>");
                return CommandOutcome::Continue;
            };
            match store.review_item(candidate_id) {
                Ok(Some(item)) => {
                    let candidate = item.candidate;
                    println!("{}", candidate.content);
                    println!(
                        "Kind: {:?}  Status: {:?}{}  Confidence: {:.2}",
                        candidate.kind,
                        candidate.status,
                        if item.expired { " (expired)" } else { "" },
                        candidate.confidence
                    );
                    println!("Scope: {:?}", candidate.scope);
                    for evidence in candidate.evidence {
                        println!(
                            "Evidence [{:?}/{}]: {}",
                            evidence.source,
                            evidence.source_role.as_deref().unwrap_or("unknown"),
                            evidence.quote
                        );
                    }
                }
                Ok(None) => println!("Candidate '{candidate_id}' not found."),
                Err(error) => println!("Failed to read candidate: {error}"),
            }
        }
        "edit" => {
            let Some(candidate_id) = args.get(1) else {
                println!("Usage: /evidence-inbox edit <candidate-id> <new-content>");
                return CommandOutcome::Continue;
            };
            let content = args
                .get(2..)
                .map(|parts| parts.join(" "))
                .unwrap_or_default();
            let (store, _memory_lease) = match evidence_write_binding(ctx) {
                Ok(binding) => binding,
                Err(error) => {
                    println!("Cannot edit evidence while the workspace is switching: {error}");
                    return CommandOutcome::Continue;
                }
            };
            match store.edit(candidate_id, &content) {
                Ok(candidate) => {
                    println!("Updated {}: {}", candidate.candidate_id, candidate.content)
                }
                Err(error) => println!("Failed to edit candidate: {error}"),
            }
        }
        "reject" => {
            let Some(candidate_id) = args.get(1) else {
                println!("Usage: /evidence-inbox reject <candidate-id>");
                return CommandOutcome::Continue;
            };
            let (store, _memory_lease) = match evidence_write_binding(ctx) {
                Ok(binding) => binding,
                Err(error) => {
                    println!("Cannot reject evidence while the workspace is switching: {error}");
                    return CommandOutcome::Continue;
                }
            };
            match store.reject(candidate_id) {
                Ok(candidate) => println!("Rejected {}.", candidate.candidate_id),
                Err(error) => println!("Failed to reject candidate: {error}"),
            }
        }
        "accept" | "undo" => {
            let Some(candidate_id) = args.get(1) else {
                println!("Usage: /evidence-inbox {sub} <candidate-id>");
                return CommandOutcome::Continue;
            };
            let (store, memory_lease) = match evidence_write_binding(ctx) {
                Ok(binding) => binding,
                Err(error) => {
                    println!("Cannot update evidence while the workspace is switching: {error}");
                    return CommandOutcome::Continue;
                }
            };
            let layer_manager = match memory_lease.create_layer_manager() {
                Ok(manager) => Arc::new(manager),
                Err(error) => {
                    println!("Failed to initialize layered memory: {error}");
                    return CommandOutcome::Continue;
                }
            };
            let result = if sub == "accept" {
                let edited = args
                    .get(2..)
                    .map(|parts| parts.join(" "))
                    .filter(|content| !content.trim().is_empty());
                store
                    .accept(candidate_id, edited.as_deref(), &layer_manager)
                    .await
            } else {
                store.undo(candidate_id, &layer_manager).await
            };
            match result {
                Ok(candidate) => {
                    println!("{} is now {:?}.", candidate.candidate_id, candidate.status)
                }
                Err(error) => println!("Review Inbox action failed: {error}"),
            }
        }
        _ => {
            println!(
                "Usage: /evidence-inbox <pending|expired|undoable|show|edit|accept|reject|undo> [candidate-id] [content]"
            );
        }
    }
    CommandOutcome::Continue
}
cmd!(
    EvidenceInboxCommand,
    "evidence-inbox",
    ["inbox"],
    CommandCategory::Advanced,
    "Review evidence-backed memory candidates",
    cmd_evidence_inbox
);

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
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
    registry.register(Arc::new(SkillRegisterCommand));
    registry.register(Arc::new(SkillPinCommand));
    registry.register(Arc::new(SkillUnpinCommand));
    registry.register(Arc::new(EvidenceInboxCommand));
}
