//! Coding mode slash commands — plan, tasks, test, code-review, fix, agents, agent.
//!
//! Git commands moved to `/git` subcommand group (see `git.rs`).

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use crate::project::test_runner;
use echo_agent_app_core::tasks::{BackgroundTaskKind, ResearchOutputFormat, TaskStatus};
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
                ctx.agent.write(|a| a.set_plan_mode(true)).await;
                // TODO(v0.3): submit task via BackgroundTaskService
                let task = args.join(" ");
                println!("Plan mode ON. Task noted: {task}");
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
            let tasks = service.list(None);
            if tasks.is_empty() {
                println!("No background tasks.");
            } else {
                println!("\nBackground Tasks:");
                println!("{:-<90}", "");
                for t in &tasks {
                    let status = task_status_display(&t.status);
                    let deps = if t.dependencies.is_empty() {
                        String::new()
                    } else {
                        format!(" deps:[{}]", t.dependencies.join(","))
                    };
                    println!(
                        "  [{:>12}] P{:<2} {} (id: {}{})",
                        status, t.priority, t.description, t.id, deps
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
            match service.get(id) {
                Some((task, meta)) => {
                    println!("\nTask: {}", task.id);
                    println!("  Description: {}", task.description);
                    println!("  Status: {}", task_status_display(&task.status));
                    println!("  Priority: {}", task.priority);
                    if !task.dependencies.is_empty() {
                        println!("  Dependencies: {}", task.dependencies.join(", "));
                    }
                    println!("  Created At: {}", task.created_at);
                    if let Some(ref result) = task.result {
                        println!("  Result: {}", result);
                    }
                    if let Some(ref meta) = meta {
                        println!("  Type: {}", meta.kind.display_name());
                        println!("  Progress: {}%", meta.progress);
                        if let Some(ref msg) = meta.progress_message {
                            println!("  Progress Message: {}", msg);
                        }
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
        "checkpoints" => {
            let pending = service.pending_checkpoints().await;
            if pending.is_empty() {
                println!("No pending human checkpoints.");
            } else {
                println!("\nPending Human Checkpoints:");
                println!("{:-<80}", "");
                for (task_id, req) in &pending {
                    println!("  Task: {}", task_id);
                    if let Some(ref phase) = req.phase {
                        println!("  Phase: {}", phase);
                    }
                    println!("  Prompt: {}", req.prompt);
                    if let Some(ref options) = req.options {
                        println!("  Options: {}", options.join(", "));
                    }
                    println!();
                }
            }
        }
        "respond" => {
            let task_id = args.get(1).copied().unwrap_or("");
            let selection = args.get(2).copied().unwrap_or("");
            if task_id.is_empty() || selection.is_empty() {
                println!("Usage: /tasks respond <task_id> <selection> [instructions]");
                println!("  Use '/tasks checkpoints' to see pending requests.");
                return CommandOutcome::Continue;
            }
            let instructions = args.get(3..).map(|s| s.join(" "));
            if service
                .respond_to_checkpoint(task_id, selection, instructions)
                .await
            {
                println!("Checkpoint response sent for task {}.", task_id);
            } else {
                println!(
                    "No pending checkpoint found for task {}. Use '/tasks checkpoints' to list pending requests.",
                    task_id
                );
            }
        }
        "dag" => {
            let manager = service.manager();
            let tasks = manager.get_all_tasks();
            if tasks.is_empty() {
                println!("No tasks to visualize.");
            } else {
                println!("\nTask Dependency Graph (Mermaid format):");
                println!("{}", manager.visualize_dependencies());
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
                "Usage: /tasks [list|status <id>|cancel <id>|research <topic>|checkpoints|respond <id> <selection>|dag]"
            );
        }
    }
    CommandOutcome::Continue
}

/// Format a TaskStatus for display.
fn task_status_display(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "Pending",
        TaskStatus::InProgress => "InProgress",
        TaskStatus::Completed => "Completed",
        TaskStatus::Cancelled => "Cancelled",
        TaskStatus::Failed(_) => "Failed",
        TaskStatus::Blocked(_) => "Blocked",
        TaskStatus::TimedOut { .. } => "TimedOut",
        TaskStatus::Retrying { .. } => "Retrying",
    }
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
            _ => {
                // Fallback to accumulated changes summary
                let g = cl.lock().await;
                g.diff_summary()
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

    // If task service is available, submit as a background AgentChat task
    if let Some(ref service) = ctx.task_service {
        match service
            .submit(
                BackgroundTaskKind::AgentChat {
                    prompt: format!("Run as subagent '{}': {}", agent_name, task),
                    session_id: None,
                },
                &format!("Agent run: {} — {}", agent_name, task),
                Some("cli".to_string()),
            )
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

async fn cmd_fix(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let (cmd, root) = {
            let g = cl.lock().await;
            (g.test_command().to_string(), g.project_root.clone())
        };

        let max_rounds: u32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(3);

        println!("Starting test-fix loop (max {} rounds)", max_rounds);

        for round in 1..=max_rounds {
            println!("\n--- Round {}/{} ---", round, max_rounds);
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
                    println!("✅ Tests PASSED on round {}!", round);
                    return CommandOutcome::Continue;
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let combined = format!("{}\n{}", stdout, stderr);

                    println!("❌ Tests FAILED on round {}", round);
                    println!("\nError output:");
                    println!("{}", combined);

                    if round < max_rounds {
                        println!("\nAsking agent to analyze and fix the errors...");

                        let fix_prompt = format!(
                            "The test command '{}' failed with the following output:\n\n{}\n\n\
                             Please analyze the errors and fix the code. \
                             Focus on the root cause, not just symptoms.",
                            cmd, combined
                        );

                        // Return to REPL with the fix prompt - the agent will process it
                        return CommandOutcome::Chat(fix_prompt);
                    } else {
                        println!("\n⚠️  Max rounds reached. Tests still failing.");
                        println!("Consider reviewing the errors manually.");
                    }
                }
                Err(e) => {
                    println!("❌ Failed to run test command: {}", e);
                    return CommandOutcome::Continue;
                }
            }
        }

        println!("\n⚠️  Test-fix loop completed after {} rounds", max_rounds);
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
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
        let current = ctx
            .agent
            .read(|a| a.get_permission_mode().to_string())
            .await;
        println!("Current permission mode: {}", current);
        println!();
        println!("Available modes:");
        println!("  default    — Ask before dangerous operations (file writes, shell commands)");
        println!("  plan       — Read-only; all write operations are blocked");
        println!("  auto-edit  — File edits are auto-approved; shell still requires confirmation");
        println!("  full-auto  — All operations auto-approved (bypass permissions)");
        println!("  auto       — AI classifier decides (when available)");
        println!("  dontask    — Silently reject operations not matching an allow rule");
        println!();
        println!("Usage: /permission <mode>");
        return CommandOutcome::Continue;
    }

    // Validate the mode
    let normalized = match mode {
        "default" => "default",
        "plan" => "plan",
        "auto-edit" | "autoedit" | "accept-edits" => "auto-edit",
        "full-auto" | "fullauto" | "bypass" => "full-auto",
        "auto" => "auto",
        "dontask" | "dont-ask" => "dontask",
        _ => {
            println!("Unknown permission mode: '{}'", mode);
            println!("Valid modes: default, plan, auto-edit, full-auto, auto, dontask");
            return CommandOutcome::Continue;
        }
    };

    ctx.agent.write(|a| a.set_permission_mode(normalized)).await;

    match normalized {
        "plan" => println!("Permission mode: plan — write operations are now BLOCKED."),
        "auto-edit" => println!("Permission mode: auto-edit — file edits are auto-approved."),
        "full-auto" => {
            println!("Permission mode: full-auto — all operations auto-approved. Use with caution.")
        }
        "dontask" => {
            println!("Permission mode: dontask — silent rejection for disallowed operations.")
        }
        "auto" => println!("Permission mode: auto — AI classifier decides."),
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
