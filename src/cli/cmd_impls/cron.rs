//! Cron scheduling slash commands — create, list, delete, pause, resume, run.
//!
//! Provides CLI access to the `SchedulerRunner` for managing recurring
//! background tasks via standard 5-field cron expressions:
//! `minute hour day-of-month month day-of-week`
//!
//! # Usage
//!
//! ```text
//! /cron create <expr> <name> <prompt...>
//! /cron list
//! /cron delete <id>
//! /cron pause <id>
//! /cron resume <id>
//! /cron run <id>
//! /cron reload
//! /cron help
//! ```

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, SubCommandDef, cmd};
use echo_agent_app_core::scheduler::{CronTask, CronTaskStatus};
use std::sync::Arc;

// ── CronCommand (subcommand dispatcher) ────────────────────────────

async fn cmd_cron(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("help");
    match sub {
        "create" | "add" | "new" => cmd_cron_create(ctx, &args[1..]).await,
        "list" | "ls" => cmd_cron_list(ctx, &args[1..]).await,
        "delete" | "rm" | "remove" => cmd_cron_delete(ctx, &args[1..]).await,
        "pause" | "disable" => cmd_cron_pause(ctx, &args[1..]).await,
        "resume" | "enable" => cmd_cron_resume(ctx, &args[1..]).await,
        "run" | "trigger" => cmd_cron_run(ctx, &args[1..]).await,
        "reload" => cmd_cron_reload(ctx, &args[1..]).await,
        _ => {
            print_cron_help();
            CommandOutcome::Continue
        }
    }
}

/// CronCommand uses the `cmd!` macro for basic trait impl, then we override
/// `subcommands()` via a wrapper to enable subcommand-aware dispatch.
pub struct CronCommand;

impl crate::cli::command::SlashCommand for CronCommand {
    fn name(&self) -> &'static str {
        "cron"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["schedule", "sched"]
    }

    fn description(&self) -> &'static str {
        "Manage recurring cron-scheduled tasks"
    }

    fn category(&self) -> CommandCategory {
        CommandCategory::Advanced
    }

    fn subcommands(&self) -> Vec<SubCommandDef> {
        vec![
            SubCommandDef {
                name: "create",
                aliases: &["add", "new"],
                description: "Create a new scheduled task",
            },
            SubCommandDef {
                name: "list",
                aliases: &["ls"],
                description: "List all scheduled tasks",
            },
            SubCommandDef {
                name: "delete",
                aliases: &["rm", "remove"],
                description: "Delete a scheduled task",
            },
            SubCommandDef {
                name: "pause",
                aliases: &["disable"],
                description: "Pause (disable) a scheduled task",
            },
            SubCommandDef {
                name: "resume",
                aliases: &["enable"],
                description: "Resume (enable) a paused task",
            },
            SubCommandDef {
                name: "run",
                aliases: &["trigger"],
                description: "Manually trigger a task now",
            },
            SubCommandDef {
                name: "reload",
                aliases: &[],
                description: "Reload tasks from disk",
            },
        ]
    }

    fn run<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: &'a [&'a str],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandOutcome> + Send + 'a>> {
        Box::pin(async move { cmd_cron(ctx, args).await })
    }
}

// ── Subcommand implementations ─────────────────────────────────────

/// `/cron create <cron_expr> <name> <prompt...>`
///
/// Cron expression is 5 fields: `minute hour day-of-month month day-of-week`
/// Examples:
///   `*/5 * * * *` — every 5 minutes
///   `0 9 * * 1-5` — 9am weekdays
///   `30 18 * * *` — 6:30pm daily
async fn cmd_cron_create(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let runner = match ctx.scheduler {
        Some(ref r) => r,
        None => {
            println!(
                "  Scheduler not available. Start with web mode or ensure scheduler is initialized."
            );
            return CommandOutcome::Continue;
        }
    };

    if args.len() < 3 {
        println!("  Usage: /cron create <cron_expr> <name> <prompt...>");
        println!();
        println!("  Cron expression: 5 fields — minute hour day-of-month month day-of-week");
        println!();
        println!("  Examples:");
        println!("    /cron create \"*/5 * * * *\" health-check Check server health");
        println!("    /cron create \"0 9 * * 1-5\" daily-report Generate daily report");
        println!("    /cron create \"30 18 * * *\" evening-summary Summarize today's work");
        return CommandOutcome::Continue;
    }

    let cron_expr = args[0];
    let name = args[1];
    let prompt = args[2..].join(" ");

    // Validate the cron expression early
    if let Err(e) = validate_cron_expr(cron_expr) {
        println!("  Invalid cron expression '{}': {}", cron_expr, e);
        println!("  Expected 5 fields: minute hour day-of-month month day-of-week");
        return CommandOutcome::Continue;
    }

    let task = CronTask::new(name, cron_expr, &prompt);
    let task_id = task.id.clone();
    let task_name = task.name.clone();

    match runner.add_task(task).await {
        Ok(()) => {
            println!("  Created cron task '{}' [{}]", task_name, &task_id[..8]);
            println!("  Schedule: {}", cron_expr);
            println!("  Prompt:   {}", prompt);
        }
        Err(e) => {
            println!("  Failed to create cron task: {}", e);
        }
    }

    CommandOutcome::Continue
}

/// `/cron list` — display all scheduled tasks
async fn cmd_cron_list(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    let runner = match ctx.scheduler {
        Some(ref r) => r,
        None => {
            println!("  Scheduler not available.");
            return CommandOutcome::Continue;
        }
    };

    let tasks = runner.list_tasks().await;

    if tasks.is_empty() {
        println!("\n  No scheduled tasks. Use /cron create to add one.");
        return CommandOutcome::Continue;
    }

    println!("\n  Scheduled Tasks ({}):", tasks.len());
    println!(
        "  {:<10} {:<20} {:<18} {:<8} {:<20} {}",
        "ID", "Name", "Schedule", "Status", "Last Run", "Next Run"
    );
    println!("  {}", "-".repeat(96));

    for task in &tasks {
        let id_short = if task.id.len() >= 8 {
            &task.id[..8]
        } else {
            &task.id
        };
        let status_str = match task.status {
            CronTaskStatus::Enabled => "enabled",
            CronTaskStatus::Disabled => "paused",
        };
        let last_run = task
            .last_run_at
            .as_deref()
            .map(|s| format_relative_time(s))
            .unwrap_or_else(|| "never".to_string());
        let next_run = if task.status == CronTaskStatus::Enabled {
            task.next_run()
                .map(|dt| format_next_run(&dt))
                .unwrap_or_else(|_| "error".to_string())
        } else {
            "-".to_string()
        };
        println!(
            "  {:<10} {:<20} {:<18} {:<8} {:<20} {}",
            id_short,
            truncate_str(&task.name, 19),
            task.cron_expr,
            status_str,
            last_run,
            next_run,
        );
    }
    println!();

    CommandOutcome::Continue
}

/// `/cron delete <id>` — remove a task by ID (prefix match)
async fn cmd_cron_delete(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let runner = match ctx.scheduler {
        Some(ref r) => r,
        None => {
            println!("  Scheduler not available.");
            return CommandOutcome::Continue;
        }
    };

    let id_prefix = match args.first() {
        Some(id) => *id,
        None => {
            println!("  Usage: /cron delete <id>");
            println!("  The ID can be a prefix (e.g. first 8 chars).");
            return CommandOutcome::Continue;
        }
    };

    // Find matching task by ID prefix
    let tasks = runner.list_tasks().await;
    let matches: Vec<&CronTask> = tasks
        .iter()
        .filter(|t| t.id.starts_with(id_prefix))
        .collect();

    match matches.len() {
        0 => {
            println!("  No task found matching '{}'.", id_prefix);
        }
        1 => {
            let task = matches[0];
            let full_id = task.id.clone();
            let name = task.name.clone();
            match runner.remove_task(&full_id).await {
                Ok(true) => println!("  Deleted cron task '{}' [{}].", name, &full_id[..8]),
                Ok(false) => println!("  Task not found (race condition?)."),
                Err(e) => println!("  Failed to delete task: {}", e),
            }
        }
        _ => {
            println!(
                "  Ambiguous ID prefix '{}'. Matches {} tasks:",
                id_prefix,
                matches.len()
            );
            for t in &matches {
                println!("    [{}] {}", &t.id[..8], t.name);
            }
            println!("  Use a longer ID prefix to disambiguate.");
        }
    }

    CommandOutcome::Continue
}

/// `/cron pause <id>` — disable a task without deleting
async fn cmd_cron_pause(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    set_task_status(ctx, args, CronTaskStatus::Disabled, "Paused").await
}

/// `/cron resume <id>` — re-enable a paused task
async fn cmd_cron_resume(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    set_task_status(ctx, args, CronTaskStatus::Enabled, "Resumed").await
}

/// Shared implementation for pause/resume.
async fn set_task_status(
    ctx: &CommandContext,
    args: &[&str],
    status: CronTaskStatus,
    action_label: &str,
) -> CommandOutcome {
    let runner = match ctx.scheduler {
        Some(ref r) => r,
        None => {
            println!("  Scheduler not available.");
            return CommandOutcome::Continue;
        }
    };

    let id_prefix = match args.first() {
        Some(id) => *id,
        None => {
            let verb = if status == CronTaskStatus::Disabled {
                "pause"
            } else {
                "resume"
            };
            println!("  Usage: /cron {} <id>", verb);
            return CommandOutcome::Continue;
        }
    };

    let tasks = runner.list_tasks().await;
    let matches: Vec<&CronTask> = tasks
        .iter()
        .filter(|t| t.id.starts_with(id_prefix))
        .collect();

    match matches.len() {
        0 => {
            println!("  No task found matching '{}'.", id_prefix);
        }
        1 => {
            let task = matches[0];
            let full_id = task.id.clone();
            let name = task.name.clone();
            match runner.set_status(&full_id, status).await {
                Ok(true) => println!(
                    "  {} cron task '{}' [{}].",
                    action_label,
                    name,
                    &full_id[..8]
                ),
                Ok(false) => println!("  Task not found (race condition?)."),
                Err(e) => println!("  Failed to update task: {}", e),
            }
        }
        _ => {
            println!(
                "  Ambiguous ID prefix '{}'. Matches {} tasks:",
                id_prefix,
                matches.len()
            );
            for t in &matches {
                println!("    [{}] {}", &t.id[..8], t.name);
            }
        }
    }

    CommandOutcome::Continue
}

/// `/cron run <id>` — manually trigger a task now
async fn cmd_cron_run(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let runner = match ctx.scheduler {
        Some(ref r) => r,
        None => {
            println!("  Scheduler not available.");
            return CommandOutcome::Continue;
        }
    };

    let id_prefix = match args.first() {
        Some(id) => *id,
        None => {
            println!("  Usage: /cron run <id>");
            return CommandOutcome::Continue;
        }
    };

    let tasks = runner.list_tasks().await;
    let matches: Vec<&CronTask> = tasks
        .iter()
        .filter(|t| t.id.starts_with(id_prefix))
        .collect();

    match matches.len() {
        0 => {
            println!("  No task found matching '{}'.", id_prefix);
        }
        1 => {
            let task = matches[0];
            let full_id = task.id.clone();
            let name = task.name.clone();
            println!("  Triggering '{}' [{}]...", name, &full_id[..8]);
            match runner.run_task_once(&full_id).await {
                Ok(result) => {
                    let preview: String = result.chars().take(500).collect();
                    println!("  Result:");
                    for line in preview.lines() {
                        println!("    {}", line);
                    }
                    if result.len() > 500 {
                        println!("    ... ({} chars total)", result.len());
                    }
                }
                Err(e) => {
                    println!("  Task execution failed: {}", e);
                }
            }
        }
        _ => {
            println!(
                "  Ambiguous ID prefix '{}'. Matches {} tasks:",
                id_prefix,
                matches.len()
            );
            for t in &matches {
                println!("    [{}] {}", &t.id[..8], t.name);
            }
        }
    }

    CommandOutcome::Continue
}

/// `/cron reload` — reload tasks from disk
async fn cmd_cron_reload(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    let runner = match ctx.scheduler {
        Some(ref r) => r,
        None => {
            println!("  Scheduler not available.");
            return CommandOutcome::Continue;
        }
    };

    match runner.reload().await {
        Ok(count) => println!("  Reloaded {} task(s) from disk.", count),
        Err(e) => println!("  Failed to reload tasks: {}", e),
    }

    CommandOutcome::Continue
}

// ── Helpers ─────────────────────────────────────────────────────────

fn print_cron_help() {
    println!("\n  /cron — Manage recurring cron-scheduled tasks");
    println!();
    println!("  Subcommands:");
    println!("    /cron create <expr> <name> <prompt...>  Create a scheduled task");
    println!("    /cron list                              List all scheduled tasks");
    println!("    /cron delete <id>                       Delete a task");
    println!("    /cron pause <id>                        Pause (disable) a task");
    println!("    /cron resume <id>                       Resume (enable) a task");
    println!("    /cron run <id>                          Manually trigger a task");
    println!("    /cron reload                            Reload tasks from disk");
    println!();
    println!("  Cron expression (5 fields): minute hour day-of-month month day-of-week");
    println!("    *       — any value");
    println!("    */N     — every N");
    println!("    N-M     — range");
    println!("    N,M,O   — list");
    println!();
    println!("  Examples:");
    println!("    /cron create \"*/5 * * * *\" check Health check every 5 min");
    println!("    /cron create \"0 9 * * 1-5\" report Daily weekday 9am report");
    println!("    /cron create \"0 0 1 * *\" monthly First-of-month summary");
    println!();
}

/// Validate a 5-field cron expression.
fn validate_cron_expr(expr: &str) -> Result<(), String> {
    use cron::Schedule;
    use std::str::FromStr;
    // The `cron` crate requires 7-field (sec min hour dom month dow year)
    // but our CronTask::next_run() already wraps with the cron crate.
    // We accept 5-field and try to parse via Schedule (which needs 7 fields),
    // so we pad with "0" prefix (seconds) and "*" suffix (year).
    let fields: Vec<&str> = expr.trim().split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!("expected 5 fields, got {}", fields.len()));
    }
    // Build a 7-field expression for the cron crate
    let expr7 = format!("0 {} *", expr);
    Schedule::from_str(&expr7).map_err(|e| format!("{}", e))?;
    Ok(())
}

/// Truncate a string to `max` chars, appending ".." if truncated.
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(2)).collect();
        format!("{}..", truncated)
    }
}

/// Format an ISO 8601 timestamp as a relative time string.
fn format_relative_time(iso: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(iso) {
        Ok(dt) => {
            let now = chrono::Utc::now();
            let diff = now.signed_duration_since(dt.with_timezone(&chrono::Utc));
            if diff.num_seconds() < 60 {
                format!("{}s ago", diff.num_seconds())
            } else if diff.num_minutes() < 60 {
                format!("{}m ago", diff.num_minutes())
            } else if diff.num_hours() < 24 {
                format!("{}h ago", diff.num_hours())
            } else {
                format!("{}d ago", diff.num_days())
            }
        }
        Err(_) => iso.chars().take(16).collect(),
    }
}

/// Format a next-run DateTime as a human-friendly string.
fn format_next_run(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let diff = dt.signed_duration_since(now);
    if diff.num_seconds() < 60 {
        "in <1m".to_string()
    } else if diff.num_minutes() < 60 {
        format!("in {}m", diff.num_minutes())
    } else if diff.num_hours() < 24 {
        let hours = diff.num_hours();
        let mins = diff.num_minutes() % 60;
        if mins == 0 {
            format!("in {}h", hours)
        } else {
            format!("in {}h{}m", hours, mins)
        }
    } else {
        let days = diff.num_days();
        format!("in {}d", days)
    }
}

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(CronCommand));
}
