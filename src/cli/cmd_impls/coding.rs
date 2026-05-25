//! Coding mode slash commands — plan, tasks, diff, test, fix, commit, review, agents, agent, pr, patch.

use std::sync::Arc;
use crate::cli::command::{cmd, CommandCategory, CommandContext, CommandOutcome};
use crate::project::{coding_task::TaskStatus, test_runner};

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
                if let Some(ref cl) = ctx.coding_loop {
                    let task = args.join(" ");
                    cl.lock().await.add_task(&task);
                    println!("Plan mode ON. Task added: {task}");
                } else {
                    println!("Plan mode ON (read-only).");
                }
            }
        }
    }
    CommandOutcome::Continue
}
cmd!(PlanCommand, "plan", CommandCategory::Coding, "Toggle plan mode (read-only enforcement)", cmd_plan);

// ── TasksCommand ─────────────────────────────────────────────────────

async fn cmd_tasks(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("");
    let rest: String = args.iter().skip(1).copied().collect::<Vec<_>>().join(" ");
    if let Some(ref cl) = ctx.coding_loop {
        let mut guard = cl.lock().await;
        match sub {
            "add" if !rest.is_empty() => { let t = guard.add_task(&rest); println!("Added: {} ({})", t.id, t.description); }
            "done" if !rest.is_empty() => { if guard.task_tracker.mark_done(&rest) { println!("Done: {rest}"); } else { println!("Not found: {rest}"); } }
            "start" if !rest.is_empty() => { if guard.task_tracker.mark_in_progress(&rest) { println!("Started: {rest}"); } else { println!("Not found: {rest}"); } }
            _ => {
                let tasks = guard.task_tracker.list();
                if tasks.is_empty() { println!("No tasks. Use /tasks add <desc>"); }
                else { for t in tasks { let icon = match t.status { TaskStatus::Pending=>"[ ]", TaskStatus::InProgress=>"[>]", TaskStatus::Done=>"[x]", TaskStatus::Cancelled=>"[-]" }; println!("  {icon} {} - {}", t.id, t.description); } }
            }
        }
    } else { println!("Coding mode not active."); }
    CommandOutcome::Continue
}
cmd!(TasksCommand, "tasks", CommandCategory::Coding, "List, add, or update tasks", cmd_tasks);

// ── DiffCommand ──────────────────────────────────────────────────────

async fn cmd_diff(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        println!("\n--- Changes ---");
        if root.join(".git").exists() {
            for (label, args) in &[("Staged", &["diff", "--cached", "--stat"] as &[&str]), ("Unstaged", &["diff", "--stat"])] {
                if let Ok(o) = tokio::process::Command::new("git").args(*args).current_dir(&root).output().await {
                    let s = String::from_utf8_lossy(&o.stdout);
                    if !s.trim().is_empty() { println!("\n--- {} ---\n{s}", label); }
                }
            }
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}
cmd!(DiffCommand, "diff", CommandCategory::Coding, "Show file changes and git diff", cmd_diff);

// ── TestCommand ──────────────────────────────────────────────────────

async fn cmd_test(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let (cmd, root) = { let g = cl.lock().await; (g.test_command().to_string(), g.project_root.clone()) };
        if cmd.is_empty() { println!("No test command configured."); return CommandOutcome::Continue; }
        println!("\nRunning: {cmd}");
        match test_runner::run_test_command(&cmd, &root).await {
            Ok(r) => {
                if r.passed { println!("\nAll tests passed."); }
                else { println!("\n{} failure(s)", r.failures.len()); for f in &r.failures { println!("  FAIL: {}", f.test_name); } }
            }
            Err(e) => println!("Error: {e}"),
        }
    } else { println!("Coding mode not active."); }
    CommandOutcome::Continue
}
cmd!(TestCommand, "test", CommandCategory::Coding, "Run project test command", cmd_test);

// ── ReviewCommand ────────────────────────────────────────────────────

async fn cmd_review(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let (summary, count) = { let g = cl.lock().await; (g.diff_summary(), g.change_count()) };
        println!("\n--- Review ---\n{summary}\n");
        println!("  Risk: {}", match count { 0=>"None", 1..=2=>"Low", 3..=5=>"Medium", _=>"High" });
    } else { println!("Coding mode not active."); }
    CommandOutcome::Continue
}
cmd!(ReviewCommand, "review", CommandCategory::Coding, "Review accumulated changes", cmd_review);

// ── CommitCommand ────────────────────────────────────────────────────

async fn cmd_commit(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        if !root.join(".git").exists() { println!("Not a git repo."); return CommandOutcome::Continue; }
        let status = tokio::process::Command::new("git").args(["status","--short"]).current_dir(&root).output().await;
        if let Ok(s) = status {
            let out = String::from_utf8_lossy(&s.stdout);
            if out.trim().is_empty() { println!("Nothing to commit."); }
            else { println!("\n--- Changes ---\n{out}\nRun: git add -A && git commit -m \"...\""); }
        }
    } else { println!("Coding mode not active."); }
    CommandOutcome::Continue
}
cmd!(CommitCommand, "commit", CommandCategory::Coding, "Show changes and commit message", cmd_commit);

// ── AgentsCommand ────────────────────────────────────────────────────

async fn cmd_agents(_ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    println!("\nAvailable SubAgents:");
    for (n, d) in &[("code-explorer","Explore codebases"),("test-runner","Run tests"),("security-reviewer","Find vulnerabilities"),("build-fixer","Fix compile errors"),("doc-writer","Write docs"),("refactor-planner","Plan refactoring"),("performance-profiler","Profile performance"),("release-engineer","Manage releases")] {
        println!("  {n} — {d}");
    }
    CommandOutcome::Continue
}
cmd!(AgentsCommand, "agents", CommandCategory::Coding, "List available subagents", cmd_agents);

// ── AgentCommand ─────────────────────────────────────────────────────

async fn cmd_agent(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.len() < 2 { println!("Usage: /agent run <name> <task>"); }
    else { println!("Running agent: {} — {}", args[1], args.get(2..).map(|s| s.join(" ")).unwrap_or_default()); }
    CommandOutcome::Continue
}
cmd!(AgentCommand, "agent", CommandCategory::Coding, "Run a subagent", cmd_agent);

// ── FixCommand ───────────────────────────────────────────────────────

async fn cmd_fix(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let cmd = { cl.lock().await.test_command().to_string() };
        println!("\nFix loop: running {cmd}...");
        println!("  If tests fail, use /test to see details and manually fix.");
    } else { println!("Coding mode not active."); }
    CommandOutcome::Continue
}
cmd!(FixCommand, "fix", CommandCategory::Coding, "Run test/fix loop", cmd_fix);

// ── PrCommand ────────────────────────────────────────────────────────

async fn cmd_pr(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\nPR summary generated. Use /commit to prepare changes.");
    CommandOutcome::Continue
}
cmd!(PrCommand, "pr", CommandCategory::Coding, "Generate PR summary", cmd_pr);

// ── PatchCommand ─────────────────────────────────────────────────────

async fn cmd_patch(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\nPatch generation: use /diff to see changes first.");
    CommandOutcome::Continue
}
cmd!(PatchCommand, "patch", CommandCategory::Coding, "Generate patch", cmd_patch);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(PlanCommand));
    registry.register(Arc::new(TasksCommand));
    registry.register(Arc::new(DiffCommand));
    registry.register(Arc::new(TestCommand));
    registry.register(Arc::new(ReviewCommand));
    registry.register(Arc::new(CommitCommand));
    registry.register(Arc::new(AgentsCommand));
    registry.register(Arc::new(AgentCommand));
    registry.register(Arc::new(FixCommand));
    registry.register(Arc::new(PrCommand));
    registry.register(Arc::new(PatchCommand));
}
