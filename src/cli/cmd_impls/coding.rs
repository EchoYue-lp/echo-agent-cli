//! Coding mode slash commands — plan, tasks, test, code-review, fix, agents, agent.
//!
//! Git commands moved to `/git` subcommand group (see `git.rs`).

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use crate::project::test_runner;
use echo_agent_app_core::tasks::task_runtime::{RecoveryDecision, TaskRetryPreparation};
use echo_agent_app_core::tasks::{BackgroundTaskKind, ResearchOutputFormat};
use std::sync::Arc;

// ── PlanCommand ──────────────────────────────────────────────────────

async fn cmd_plan(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("");
    match sub {
        "on" => {
            ctx.agent.write(|a| a.set_plan_mode(true)).await;
            println!("Plan mode enabled — write operations blocked.");
        }
        "off" => {
            ctx.agent.write(|a| a.set_plan_mode(false)).await;
            println!("Plan mode disabled.");
        }
        _ => {
            if sub.is_empty() {
                let is_plan = ctx.agent.read(|a| a.is_plan_mode()).await;
                println!("Plan mode: {}", if is_plan { "ON" } else { "OFF" });
                println!("Usage: /plan [on|off]");
            } else {
                let task = args.join(" ");
                *ctx.interaction_mode.write().await =
                    echo_agent_app_core::tasks::task_runtime::InteractionMode::Task;
                println!("Interaction mode set to Task for this request.");
                return CommandOutcome::Chat(task);
            }
        }
    }
    CommandOutcome::Continue
}
cmd!(
    PlanCommand,
    "plan",
    CommandCategory::Coding,
    "Toggle plan mode (read-only enforcement)",
    cmd_plan
);

// ── TasksCommand ─────────────────────────────────────────────────────

async fn cmd_tasks(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let service = match &ctx.task_service {
        Some(svc) => svc.clone(),
        None => {
            println!("Background task service not configured.");
            println!(
                "Use the --tasks flag when starting the agent to enable background task support."
            );
            return CommandOutcome::Continue;
        }
    };

    let sub = args.first().copied().unwrap_or("");
    match sub {
        "list" | "" => {
            let tasks = service.list_unified(None);
            if tasks.is_empty() {
                println!("No background tasks.");
            } else {
                println!("\nBackground Tasks:");
                println!("{:-<90}", "");
                for t in &tasks {
                    let deps = if t.dependencies.is_empty() {
                        String::new()
                    } else {
                        format!(" deps:[{}]", t.dependencies.join(","))
                    };
                    println!(
                        "  [{:>12}] P{:<2} {} (id: {}{})",
                        t.status, t.priority, t.description, t.id, deps
                    );
                }
            }
        }
        "status" => {
            let id = args.get(1).copied().unwrap_or("");
            if id.is_empty() {
                println!("Usage: /tasks status <id>");
                return CommandOutcome::Continue;
            }
            match service.get_unified(id) {
                Some(task) => {
                    println!("\nTask: {}", task.id);
                    println!("  Description: {}", task.description);
                    println!("  Status: {}", task.status);
                    println!("  Priority: {}", task.priority);
                    if !task.dependencies.is_empty() {
                        println!("  Dependencies: {}", task.dependencies.join(", "));
                    }
                    println!("  Created At: {}", task.created_at);
                    if let Some(ref result) = task.result {
                        println!("  Result: {}", result);
                    }
                    if let Some(ref kind) = task.kind {
                        println!("  Type: {}", kind);
                    }
                    // Show real-time progress from cache
                    if let Some(p) = service.get_progress(id) {
                        println!("  Live Progress: {:.1}%", p.percentage);
                        println!("  Phase: {}", p.current_phase);
                        if let Some(ref msg) = p.message {
                            println!("  Message: {}", msg);
                        }
                        if let Some(eta) = p.eta_secs {
                            println!("  ETA: {}s", eta);
                        }
                    }
                    match service.recovery_blockers(id) {
                        Ok(blockers) if !blockers.is_empty() => {
                            println!("  Recovery blockers:");
                            for blocker in blockers {
                                println!("    {}: {}", blocker.task_id, blocker.reason);
                            }
                        }
                        Ok(_) => {}
                        Err(error) => println!("  Recovery status unavailable: {error}"),
                    }
                }
                None => {
                    println!("Task not found: {}", id);
                }
            }
        }
        "cancel" => {
            let id = args.get(1).copied().unwrap_or("");
            if id.is_empty() {
                println!("Usage: /tasks cancel <id>");
                return CommandOutcome::Continue;
            }
            if service.cancel(id).await {
                println!("Task cancelled: {}", id);
            } else {
                println!("Failed to cancel task (not found or not running): {}", id);
            }
        }
        "pause" => {
            let id = args.get(1).copied().unwrap_or("");
            if id.is_empty() {
                println!("Usage: /tasks pause <id>");
                return CommandOutcome::Continue;
            }
            match service.pause(id) {
                Ok(true) => println!("Task paused: {id}"),
                Ok(false) => println!("Failed to pause task (not running): {id}"),
                Err(error) => println!("Failed to pause task: {error}"),
            }
        }
        "resume" => {
            let id = args.get(1).copied().unwrap_or("");
            if id.is_empty() {
                println!("Usage: /tasks resume <id>");
                return CommandOutcome::Continue;
            }
            match service.resume(id) {
                Ok(()) => println!("Task resumed: {id}"),
                Err(error) => println!("Failed to resume task: {error}"),
            }
        }
        "recovery" => {
            let id = args.get(1).copied().unwrap_or("");
            if id.is_empty() {
                println!("Usage: /tasks recovery <id>");
                return CommandOutcome::Continue;
            }
            match service.recovery_blockers(id) {
                Ok(blockers) if blockers.is_empty() => println!("No recovery blockers: {id}"),
                Ok(blockers) => {
                    println!("Recovery blockers for {id}:");
                    for blocker in blockers {
                        println!("  {}: {}", blocker.task_id, blocker.reason);
                    }
                    println!(
                        "Resolve with /tasks retry <id> <task-id> or /tasks skip <id> <task-id>"
                    );
                }
                Err(error) => println!("Failed to read recovery blockers: {error}"),
            }
        }
        "retry" | "skip" => {
            let id = args.get(1).copied().unwrap_or("");
            let task_id = args.get(2).copied().unwrap_or("");
            if id.is_empty() || task_id.is_empty() {
                println!("Usage: /tasks {sub} <id> <task-id>");
                return CommandOutcome::Continue;
            }
            if sub == "retry" {
                match service.retry_blocked_task(id, task_id) {
                    Ok(TaskRetryPreparation::Acceptance { next_attempt }) => {
                        println!("Task {task_id} retried as attempt {next_attempt} on run {id}.");
                    }
                    Ok(TaskRetryPreparation::Recovery) => {
                        println!("Recovery decision recorded: {id}/{task_id} -> retry");
                    }
                    Err(error) => println!("Failed to retry task: {error}"),
                }
            } else {
                match service.resolve_recovery_task(id, task_id, RecoveryDecision::Skip) {
                    Ok(()) => println!("Recovery decision recorded: {id}/{task_id} -> skip"),
                    Err(error) => println!("Failed to resolve recovery task: {error}"),
                }
            }
        }
        "research" => {
            let topic = args.get(1..).map(|s| s.join(" ")).unwrap_or_default();
            if topic.is_empty() {
                println!("Usage: /tasks research <topic>");
                return CommandOutcome::Continue;
            }
            match service
                .submit(
                    BackgroundTaskKind::Research {
                        topic: topic.clone(),
                        max_papers: 20,
                        output_format: ResearchOutputFormat::Markdown,
                    },
                    &format!("Research: {}", topic),
                    Some("cli".to_string()),
                )
                .await
            {
                Ok(task_id) => {
                    println!("Research task submitted: {} (id: {})", topic, task_id)
                }
                Err(e) => println!("Failed to submit research task: {}", e),
            }
        }
        "dag" => {
            let tasks = service.list_unified(None);
            if tasks.is_empty() {
                println!("No tasks to visualize.");
            } else {
                println!("\nTask Dependency Graph (Mermaid format):");
                println!("graph TD");
                for task in &tasks {
                    println!(
                        "    {}[\"{}\"]",
                        task.id,
                        task.description.replace('"', "'")
                    );
                    for dependency in &task.dependencies {
                        println!("    {} --> {}", dependency, task.id);
                    }
                }
                println!("\nTask Details:");
                for task in &tasks {
                    let deps = if task.dependencies.is_empty() {
                        "none".to_string()
                    } else {
                        task.dependencies.join(", ")
                    };
                    println!(
                        "  {} [P{}] - {} (deps: {})",
                        task.id, task.priority, task.description, deps
                    );
                }
            }
        }
        _ => {
            println!(
                "Usage: /tasks [list|status <id>|pause <id>|resume <id>|cancel <id>|recovery <id>|retry <id> <task-id>|skip <id> <task-id>|research <topic>|dag]"
            );
        }
    }
    CommandOutcome::Continue
}

cmd!(
    TasksCommand,
    "tasks",
    CommandCategory::Coding,
    "List, add, or update tasks",
    cmd_tasks
);

// ── TestCommand ──────────────────────────────────────────────────────

async fn cmd_test(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let (cmd, root) = {
            let g = cl.lock().await;
            (g.test_command().to_string(), g.project_root.clone())
        };
        if cmd.is_empty() {
            println!("No test command configured.");
            return CommandOutcome::Continue;
        }
        println!("\nRunning: {cmd}");
        match test_runner::run_test_command(&cmd, &root).await {
            Ok(r) => {
                if r.passed {
                    println!("\nAll tests passed.");
                } else {
                    println!("\n{} failure(s)", r.failures.len());
                    for f in &r.failures {
                        println!("  FAIL: {}", f.test_name);
                    }
                }
            }
            Err(e) => println!("Error: {e}"),
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}
cmd!(
    TestCommand,
    "test",
    CommandCategory::Coding,
    "Run project test command",
    cmd_test
);

// ── ReviewCommand ────────────────────────────────────────────────────

async fn cmd_review(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        if !root.join(".git").exists() {
            println!("Not a git repo.");
            return CommandOutcome::Continue;
        }

        // Get the actual diff
        let diff_output = tokio::process::Command::new("git")
            .args(["diff", "HEAD"])
            .current_dir(&root)
            .output()
            .await;

        let diff_text = match diff_output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).to_string()
            }
            Ok(output) => {
                println!(
                    "Failed to inspect changes: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
                return CommandOutcome::Continue;
            }
            Err(error) => {
                println!("Failed to inspect changes: {error}");
                return CommandOutcome::Continue;
            }
        };

        if diff_text.trim().is_empty() {
            println!("No changes to review.");
            return CommandOutcome::Continue;
        }

        // Count changed lines for risk assessment
        let added = diff_text
            .lines()
            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
            .count();
        let removed = diff_text
            .lines()
            .filter(|l| l.starts_with('-') && !l.starts_with("---"))
            .count();
        let changed_files = diff_text
            .lines()
            .filter(|l| l.starts_with("diff --git"))
            .count();

        println!("\n=== Code Review ===\n");
        println!(
            "Changes: +{} -{} across {} file(s)",
            added, removed, changed_files
        );

        // Risk assessment
        let risk = if added + removed > 500 {
            "High"
        } else if added + removed > 100 {
            "Medium"
        } else if added + removed > 0 {
            "Low"
        } else {
            "None"
        };
        println!("Risk: {}\n", risk);

        // Show diff (truncated if too long)
        let max_lines = 50;
        let diff_lines: Vec<&str> = diff_text.lines().collect();
        if diff_lines.len() > max_lines {
            for line in diff_lines.iter().take(max_lines) {
                println!("{}", line);
            }
            println!(
                "\n... ({} more lines, use 'git diff' to see all)",
                diff_lines.len() - max_lines
            );
        } else {
            println!("{}", diff_text);
        }

        // Suggest agent review for larger changes
        if added + removed > 50 {
            println!("\nTip: Ask the agent to review these changes:");
            println!("  'Review the recent code changes and suggest improvements'");
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}
cmd!(
    CodeReviewCommand,
    "code-review",
    ["cr"],
    CommandCategory::Coding,
    "Review accumulated changes",
    cmd_review
);

// ── AgentsCommand ────────────────────────────────────────────────────

async fn cmd_agents(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    println!("\nAvailable SubAgents:");

    // Query the agent's actual tool registry for agent-like tool names
    let agent_like_names: Vec<String> = ctx
        .agent
        .read(|a| {
            a.tool_names()
                .into_iter()
                .filter(|name| name.contains("agent"))
                .collect()
        })
        .await;

    if !agent_like_names.is_empty() {
        println!("\n  Agent Dispatch Tools (from registry):");
        for name in &agent_like_names {
            println!("    - {}", name);
        }
    } else {
        println!("\n  (No agent dispatch tools currently registered)");
    }

    // Show common pre-registered subagents as a reference
    println!("\n  Common SubAgents (pre-registered):");
    for (n, d) in &[
        ("code-explorer", "Explore codebases"),
        ("test-runner", "Run tests"),
        ("security-reviewer", "Find vulnerabilities"),
        ("build-fixer", "Fix compile errors"),
        ("doc-writer", "Write docs"),
        ("refactor-planner", "Plan refactoring"),
        ("performance-profiler", "Profile performance"),
        ("release-engineer", "Manage releases"),
    ] {
        println!("    {} — {}", n, d);
    }

    println!();
    println!("  Use /agent run <name> <task> to dispatch to a subagent.");
    println!("  Full subagent registry listing planned for future release.");

    CommandOutcome::Continue
}
cmd!(
    AgentsCommand,
    "agents",
    CommandCategory::Coding,
    "List available subagents",
    cmd_agents
);

// ── AgentCommand ─────────────────────────────────────────────────────

async fn cmd_agent(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.len() < 3 {
        println!("Usage: /agent run <name> <task>");
        println!();
        println!("Examples:");
        println!("  /agent run code-explorer 'Explain the architecture of src/'");
        println!("  /agent run doc-writer 'Write documentation for the API'");
        println!("  /agent run build-fixer 'Fix the current build errors'");
        return CommandOutcome::Continue;
    }

    let subcommand = args[0];
    if subcommand != "run" {
        println!(
            "Unknown subcommand '{}'. Usage: /agent run <name> <task>",
            subcommand
        );
        return CommandOutcome::Continue;
    }

    let agent_name = args[1];
    let task = args[2..].join(" ");

    // If task service is available, submit as a background Run (Phase 3.5:
    // AgentChat variant deleted; use submit_run directly).
    if let Some(ref service) = ctx.task_service {
        let prompt = format!("Run as subagent '{}': {}", agent_name, task);
        let description = format!("Agent run: {} — {}", agent_name, task);
        match service
            .submit_run(&prompt, &description, "background", "cli")
            .await
        {
            Ok(task_id) => {
                println!("Agent task submitted as background task.");
                println!("  Agent: {}", agent_name);
                println!("  Task description: {}", task);
                println!("  Task ID: {}", task_id);
                println!("  Use /tasks status {} to check progress.", task_id);
            }
            Err(e) => {
                println!("Failed to submit agent task: {}", e);
            }
        }
    } else {
        println!("Agent dispatch requested:");
        println!("  Agent: {}", agent_name);
        println!("  Task: {}", task);
        println!();
        println!("Background task service not configured. Try submitting via chat:");
        println!("  'Use the {} subagent to: {}'", agent_name, task);
    }

    CommandOutcome::Continue
}
cmd!(
    AgentCommand,
    "agent",
    CommandCategory::Coding,
    "Run a subagent",
    cmd_agent
);

// ── FixCommand ───────────────────────────────────────────────────────

async fn cmd_fix(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let (cmd, root) = {
            let g = cl.lock().await;
            (g.test_command().to_string(), g.project_root.clone())
        };

        if cmd.is_empty() {
            println!("No test command configured.");
            return CommandOutcome::Continue;
        }

        println!("Running: {}", cmd);

        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let (program, cmd_args) = if parts.is_empty() {
            ("echo", vec!["no test command"])
        } else {
            (parts[0], parts[1..].to_vec())
        };

        let result = tokio::process::Command::new(program)
            .args(&cmd_args)
            .current_dir(&root)
            .output()
            .await;

        match result {
            Ok(output) if output.status.success() => {
                println!("✅ All tests passed!");
                CommandOutcome::Continue
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let combined = format!("{}\n{}", stdout, stderr);

                println!("❌ Tests FAILED");
                println!("\nError output:");
                // Truncate long output for display
                let max_display = 2000;
                if combined.chars().count() > max_display {
                    let truncated: String = combined.chars().take(max_display).collect();
                    println!("{truncated}...");
                    println!(
                        "\n(truncated, {} more chars)",
                        combined.chars().count() - max_display
                    );
                } else {
                    println!("{}", combined);
                }

                println!("\nAsking agent to analyze and fix the errors...");

                let fix_prompt = format!(
                    "The test command '{}' failed with the following output:\n\n{}\n\n\
                     Please analyze the errors and fix the code. \
                     Focus on the root cause, not just symptoms. \
                     After fixing, the user can run /fix again to verify.",
                    cmd, combined
                );

                CommandOutcome::Chat(fix_prompt)
            }
            Err(e) => {
                println!("❌ Failed to run test command: {}", e);
                CommandOutcome::Continue
            }
        }
    } else {
        println!("Coding mode not active.");
        CommandOutcome::Continue
    }
}
cmd!(
    FixCommand,
    "fix",
    CommandCategory::Coding,
    "Run test/fix loop",
    cmd_fix
);

// ── PermissionCommand ─────────────────────────────────────────────────

async fn cmd_permission(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let mode = args.first().copied().unwrap_or("");

    if mode.is_empty() {
        // Show current permission mode
        let current = match ctx.app_state.as_ref() {
            Some(state) => echo_agent_app_core::permission::permission_mode_id(
                *state.config.permission_mode.read().await,
            ),
            None => {
                ctx.agent
                    .read(|agent| {
                        echo_agent_app_core::permission::permission_mode_id(
                            agent.get_permission_mode(),
                        )
                    })
                    .await
            }
        };
        println!("Current permission mode: {}", current);
        println!();
        println!("Available modes:");
        println!("  default    — Ask before dangerous operations (file writes, shell commands)");
        println!(
            "  auto-edit  — Read and file edit operations are auto-approved; shell still requires confirmation"
        );
        println!("  full-auto  — All operations auto-approved (bypass permissions)");
        println!("  strict     — Ask before writes, shell, network, and sensitive operations");
        println!();
        println!("Usage: /permission <mode>");
        return CommandOutcome::Continue;
    }

    let framework_mode = match echo_agent_app_core::permission::parse_permission_mode(mode) {
        Ok(mode) => mode,
        Err(error) => {
            println!("{error}");
            return CommandOutcome::Continue;
        }
    };
    let normalized = echo_agent_app_core::permission::permission_mode_id(framework_mode);

    match ctx.app_state.as_ref() {
        Some(state) => {
            *state.config.permission_mode.write().await = framework_mode;
            state.apply_permission_mode_to_agents(framework_mode).await;
        }
        None => {
            ctx.agent
                .write(|agent| agent.set_permission_mode(framework_mode))
                .await
        }
    }

    match normalized {
        "auto-edit" => {
            println!("Permission mode: auto-edit — reads and file edits are auto-approved.")
        }
        "full-auto" => {
            println!("Permission mode: full-auto — all operations auto-approved. Use with caution.")
        }
        "strict" => println!(
            "Permission mode: strict — writes, shell, network, and sensitive operations require confirmation."
        ),
        _ => println!("Permission mode: default — standard approval flow."),
    }

    CommandOutcome::Continue
}
cmd!(
    PermissionCommand,
    "permission",
    ["perm"],
    CommandCategory::Config,
    "View or change the agent permission mode",
    cmd_permission
);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(PlanCommand));
    registry.register(Arc::new(TasksCommand));
    registry.register(Arc::new(TestCommand));
    registry.register(Arc::new(CodeReviewCommand));
    registry.register(Arc::new(AgentsCommand));
    registry.register(Arc::new(AgentCommand));
    registry.register(Arc::new(FixCommand));
    registry.register(Arc::new(PermissionCommand));
}
