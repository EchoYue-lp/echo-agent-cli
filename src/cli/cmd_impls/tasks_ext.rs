//! Extended task management commands — progress tracking, dependency visualization.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

fn parse_budget(value: &str, label: &str) -> Result<Option<u64>, String> {
    if matches!(value, "none" | "unbounded") {
        return Ok(None);
    }
    let budget = value
        .parse::<u64>()
        .map_err(|error| format!("invalid {label} budget: {error}"))?;
    if budget == 0 {
        return Err(format!("{label} budget must be positive or 'none'"));
    }
    Ok(Some(budget))
}

fn current_task_run(
    ctx: &CommandContext,
    requested_run_id: Option<&str>,
) -> Result<
    (
        Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>,
        echo_agent_app_core::tasks::task_runtime::RunStateSnapshot,
    ),
    String,
> {
    let state = ctx
        .app_state
        .as_ref()
        .ok_or_else(|| "application state is unavailable".to_string())?;
    let store = state
        .tasks
        .runtime
        .clone()
        .ok_or_else(|| "TaskRuntime store is unavailable".to_string())?;
    let conversation_id = ctx
        .conversation_id
        .as_deref()
        .ok_or_else(|| "REPL conversation identity is unavailable".to_string())?;
    let run = match requested_run_id.filter(|run_id| !run_id.trim().is_empty()) {
        Some(run_id) => store.get_run(run_id).map_err(|error| error.to_string())?,
        None => store
            .latest_run_for_conversation(conversation_id)
            .map_err(|error| error.to_string())?,
    }
    .ok_or_else(|| "no TaskRun was found for this conversation".to_string())?;
    if run.conversation_id != conversation_id {
        return Err(format!(
            "TaskRun {} belongs to another conversation",
            run.run_id
        ));
    }
    let snapshot = store
        .get_run_state(&run.run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("TaskRun {} has no event projection", run.run_id))?;
    Ok((store, snapshot))
}

fn print_task_run_status(snapshot: &echo_agent_app_core::tasks::task_runtime::RunStateSnapshot) {
    println!("\n--- TaskRun {} ---", snapshot.run.run_id);
    println!("  Status: {}", snapshot.run.status.as_str());
    println!(
        "  Goal r{}: {}",
        snapshot.run.goal_revision, snapshot.run.goal
    );
    if let Some(continuation) = snapshot.continuation.as_ref() {
        let token_budget = continuation
            .token_budget
            .map(|budget| budget.to_string())
            .unwrap_or_else(|| "unbounded".to_string());
        let tokens_remaining = continuation
            .token_budget
            .map(|budget| budget.saturating_sub(continuation.tokens_used).to_string())
            .unwrap_or_else(|| "unbounded".to_string());
        let time_budget = continuation
            .time_budget_seconds
            .map(|budget| budget.to_string())
            .unwrap_or_else(|| "unbounded".to_string());
        let time_remaining = continuation
            .time_budget_seconds
            .map(|budget| {
                budget
                    .saturating_sub(continuation.time_used_seconds)
                    .to_string()
            })
            .unwrap_or_else(|| "unbounded".to_string());
        println!(
            "  Turn: {}  compactions: {}  deferred: {}",
            continuation
                .active_turn
                .as_ref()
                .or(continuation.last_turn.as_ref())
                .map(|turn| turn.ordinal.to_string())
                .unwrap_or_else(|| "none".to_string()),
            continuation.compaction_count,
            continuation.deferred
        );
        println!(
            "  Tokens: used {}, budget {}, remaining {}",
            continuation.tokens_used, token_budget, tokens_remaining
        );
        println!(
            "  Time seconds: used {}, budget {}, remaining {}",
            continuation.time_used_seconds, time_budget, time_remaining
        );
        if let Some(pause) = continuation.pause.as_ref() {
            println!(
                "  Pause: {}{}",
                pause.reason.as_str(),
                pause
                    .detail
                    .as_ref()
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            );
        }
    }
}

async fn cmd_task_run(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let (action, requested_run_id) = match args.first().copied() {
        Some(action @ ("status" | "pause" | "resume" | "cancel" | "budget")) => {
            let run_id_index = if action == "budget" { 3 } else { 1 };
            (action, args.get(run_id_index).copied())
        }
        Some(run_id) => ("status", Some(run_id)),
        None => ("status", None),
    };
    let (store, snapshot) = match current_task_run(ctx, requested_run_id) {
        Ok(value) => value,
        Err(error) => {
            println!("\n  TaskRun error: {error}");
            return CommandOutcome::Continue;
        }
    };
    match action {
        "status" => print_task_run_status(&snapshot),
        "pause" => match store.request_pause(&snapshot.run.run_id) {
            Ok(true) => println!("\n  TaskRun {} paused.", snapshot.run.run_id),
            Ok(false) => println!(
                "\n  TaskRun {} is not actively pausable.",
                snapshot.run.run_id
            ),
            Err(error) => println!("\n  Unable to pause TaskRun: {error}"),
        },
        "cancel" => match store.request_cancel(&snapshot.run.run_id) {
            Ok(true) => println!("\n  TaskRun {} cancelled.", snapshot.run.run_id),
            Ok(false) => println!("\n  TaskRun {} is already terminal.", snapshot.run.run_id),
            Err(error) => println!("\n  Unable to cancel TaskRun: {error}"),
        },
        "resume" => {
            if snapshot.run.status
                != echo_agent_app_core::tasks::task_runtime::TaskRunStatus::Paused
            {
                println!(
                    "\n  TaskRun {} is {}; resume requires paused.",
                    snapshot.run.run_id,
                    snapshot.run.status.as_str()
                );
            } else {
                *ctx.interaction_mode.write().await =
                    echo_agent_app_core::tasks::task_runtime::InteractionMode::Task;
                return CommandOutcome::ResumeTaskRun {
                    message: format!(
                        "Resume the existing TaskRun {} toward its unchanged Goal. Reload the authoritative TaskRuntime projection and continue the next useful work.",
                        snapshot.run.run_id
                    ),
                    run_id: snapshot.run.run_id.clone(),
                    root_message_id: snapshot.run.root_message_id.clone(),
                };
            }
        }
        "budget" => {
            let Some(token_value) = args.get(1).copied() else {
                println!("\n  Usage: /task-run budget <tokens|none> <seconds|none> [run-id]");
                return CommandOutcome::Continue;
            };
            let Some(time_value) = args.get(2).copied() else {
                println!("\n  Usage: /task-run budget <tokens|none> <seconds|none> [run-id]");
                return CommandOutcome::Continue;
            };
            let budgets = parse_budget(token_value, "token")
                .and_then(|tokens| parse_budget(time_value, "time").map(|time| (tokens, time)));
            match budgets.and_then(|(tokens, time)| {
                store
                    .update_run_continuation_budgets(&snapshot.run.run_id, tokens, time)
                    .map_err(|error| error.to_string())
            }) {
                Ok(updated) => {
                    println!("\n  TaskRun {} budgets updated.", snapshot.run.run_id);
                    let mut updated_snapshot = snapshot;
                    updated_snapshot.continuation = Some(updated);
                    print_task_run_status(&updated_snapshot);
                }
                Err(error) => println!("\n  Unable to update TaskRun budgets: {error}"),
            }
        }
        _ => {}
    }
    CommandOutcome::Continue
}
cmd!(
    TaskRunCommand,
    "task-run",
    ["tr"],
    CommandCategory::Advanced,
    "Inspect, budget, or control the current long-horizon TaskRun",
    cmd_task_run
);

async fn cmd_task_goal(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let parsed = match crate::task_run_control::parse_run_goal_update_args(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            println!("\n  {error}");
            return CommandOutcome::Continue;
        }
    };
    let (store, snapshot) = match current_task_run(ctx, parsed.requested_run_id.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            println!("\n  TaskRun error: {error}");
            return CommandOutcome::Continue;
        }
    };
    match store.update_run_goal(
        &snapshot.run.run_id,
        parsed.expected_goal_revision,
        &parsed.new_goal,
        &parsed.reason,
        echo_agent_app_core::tasks::task_runtime::RunGoalActorSource::Cli,
    ) {
        Ok(run) => println!(
            "\n  TaskRun {} Goal updated to revision {}; submit task_update before resuming.",
            run.run_id, run.goal_revision
        ),
        Err(error) => println!("\n  Unable to update TaskRun Goal: {error}"),
    }
    CommandOutcome::Continue
}
cmd!(
    TaskGoalCommand,
    "task-goal",
    CommandCategory::Advanced,
    "Update the paused TaskRun Goal with optimistic concurrency",
    cmd_task_goal
);

fn print_completion_gate(report: &echo_agent_app_core::tasks::task_runtime::CompletionGateReport) {
    println!(
        "\n--- Completion gate: Goal r{}, Plan r{} ({}) ---",
        report.goal_revision,
        report.plan_revision,
        if report.ready { "ready" } else { "blocked" }
    );
    for item in &report.requirements {
        println!(
            "  [{}] {}  {} ({})",
            item.status.as_str(),
            item.requirement.requirement_id,
            item.requirement.title,
            item.requirement.task_id
        );
    }
    for blocker in &report.blockers {
        println!("  BLOCK {:?}: {}", blocker.code, blocker.detail);
    }
}

async fn cmd_task_requirements(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let requested_run_id = args.first().copied();
    let (store, snapshot) = match current_task_run(ctx, requested_run_id) {
        Ok(value) => value,
        Err(error) => {
            println!("\n  TaskRun error: {error}");
            return CommandOutcome::Continue;
        }
    };
    match store.completion_gate_report(&snapshot.run.run_id) {
        Ok(report) => print_completion_gate(&report),
        Err(error) => println!("\n  Unable to read completion gate: {error}"),
    }
    CommandOutcome::Continue
}

async fn cmd_task_requirement_skip(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let parsed = match crate::task_run_control::parse_requirement_skip_args(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            println!("\n  {error}");
            return CommandOutcome::Continue;
        }
    };
    let (store, snapshot) = match current_task_run(ctx, parsed.requested_run_id.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            println!("\n  TaskRun error: {error}");
            return CommandOutcome::Continue;
        }
    };
    match store.skip_goal_requirement(
        &snapshot.run.run_id,
        parsed.expected_goal_revision,
        &parsed.requirement_id,
        &parsed.reason,
        echo_agent_app_core::tasks::task_runtime::RunGoalActorSource::Cli,
    ) {
        Ok(report) => print_completion_gate(&report),
        Err(error) => println!("\n  Unable to confirm requirement Skip: {error}"),
    }
    CommandOutcome::Continue
}

cmd!(
    TaskRequirementsCommand,
    "task-requirements",
    CommandCategory::Advanced,
    "Show the current Goal Requirement/Evidence completion gate",
    cmd_task_requirements
);
cmd!(
    TaskRequirementSkipCommand,
    "task-requirement-skip",
    CommandCategory::Advanced,
    "Confirm a Skip for one exact current-Goal requirement",
    cmd_task_requirement_skip
);

#[derive(Clone, Copy)]
enum SubagentControlAction {
    Message,
    Followup,
    Interrupt,
}

async fn cmd_subagent_control(
    ctx: &CommandContext,
    args: &[&str],
    action: SubagentControlAction,
) -> CommandOutcome {
    let (usage, instruction_required) = match action {
        SubagentControlAction::Message => (crate::task_run_control::SUBAGENT_MESSAGE_USAGE, true),
        SubagentControlAction::Followup => (crate::task_run_control::SUBAGENT_FOLLOWUP_USAGE, true),
        SubagentControlAction::Interrupt => {
            (crate::task_run_control::SUBAGENT_INTERRUPT_USAGE, false)
        }
    };
    let parsed = match crate::task_run_control::parse_subagent_control_args(
        args,
        usage,
        instruction_required,
    ) {
        Ok(parsed) => parsed,
        Err(error) => {
            println!("\n  {error}");
            return CommandOutcome::Continue;
        }
    };
    let (store, _) = match current_task_run(ctx, Some(&parsed.identity.run_id)) {
        Ok(value) => value,
        Err(error) => {
            println!("\n  Subagent control error: {error}");
            return CommandOutcome::Continue;
        }
    };
    let service = echo_agent_app_core::tasks::task_runtime::SubagentControlService::new(store);
    let result = match action {
        SubagentControlAction::Message => {
            let Some(instruction) = parsed.instruction.as_deref() else {
                println!("\n  Usage: {usage}");
                return CommandOutcome::Continue;
            };
            service
                .send_message(
                    parsed.identity,
                    instruction,
                    echo_agent_app_core::tasks::task_runtime::SubagentControlActorSource::Cli,
                )
                .await
        }
        SubagentControlAction::Followup => {
            let Some(instruction) = parsed.instruction.as_deref() else {
                println!("\n  Usage: {usage}");
                return CommandOutcome::Continue;
            };
            service.queue_guidance(
                parsed.identity,
                instruction,
                echo_agent_app_core::tasks::task_runtime::SubagentControlActorSource::Cli,
            )
        }
        SubagentControlAction::Interrupt => {
            service
                .interrupt_subagent(
                    parsed.identity,
                    echo_agent_app_core::tasks::task_runtime::SubagentControlActorSource::Cli,
                )
                .await
        }
    };
    match result {
        Ok(receipt) => println!(
            "\n  Subagent command {} is {}{}.",
            receipt.identity.command_id,
            receipt.status.as_str(),
            receipt
                .detail
                .as_deref()
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default()
        ),
        Err(error) => println!("\n  Subagent control failed: {error}"),
    }
    CommandOutcome::Continue
}

async fn cmd_subagent_message(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    cmd_subagent_control(ctx, args, SubagentControlAction::Message).await
}

async fn cmd_subagent_followup(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    cmd_subagent_control(ctx, args, SubagentControlAction::Followup).await
}

async fn cmd_subagent_interrupt(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    cmd_subagent_control(ctx, args, SubagentControlAction::Interrupt).await
}

cmd!(
    SubagentMessageCommand,
    "subagent-message",
    CommandCategory::Advanced,
    "Send guidance to one exact active Subagent attempt",
    cmd_subagent_message
);
cmd!(
    SubagentFollowupCommand,
    "subagent-followup",
    CommandCategory::Advanced,
    "Queue guidance for one exact future Subagent attempt",
    cmd_subagent_followup
);
cmd!(
    SubagentInterruptCommand,
    "subagent-interrupt",
    CommandCategory::Advanced,
    "Interrupt one exact Subagent attempt",
    cmd_subagent_interrupt
);

// ── TaskProgressCommand ──────────────────────────────────────────────

async fn cmd_task_progress(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let handle = ctx.agent.clone();
    handle
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                let msg_count = ctx.messages().len();
                let tokens = ctx.token_estimate();

                println!("\n--- Task Progress ---");
                println!("  Messages: {}", msg_count);
                println!("  Est. tokens: ~{}", tokens);
                println!(
                    "  Plan mode: {}",
                    if a.is_plan_mode() { "ON" } else { "OFF" }
                );
                println!("\n  Use /tasks to manage coding tasks.");
                println!("  Use /plan to toggle plan mode.");
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    TaskProgressCommand,
    "task-progress",
    ["tp"],
    CommandCategory::Advanced,
    "Show current task progress",
    cmd_task_progress
);

// ── TaskTreeCommand ─────────────────────────────────────────────────

async fn cmd_task_tree(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let handle = ctx.agent.clone();
    handle
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                let messages = ctx.messages();

                println!("\n--- Conversation Flow ---");
                println!("  Total messages: {}", messages.len());

                // Show message flow with roles
                let mut entries: Vec<(usize, String, String)> = Vec::new();
                for (i, msg) in messages.iter().enumerate() {
                    let role = msg.role.as_str().to_string();
                    let preview = msg
                        .content
                        .as_text_ref()
                        .map(|s| {
                            let truncated: String = s.chars().take(80).collect();
                            if s.chars().count() > 80 {
                                format!("{}...", truncated)
                            } else {
                                truncated
                            }
                        })
                        .unwrap_or_else(|| "[non-text]".to_string());
                    entries.push((i, role, preview));
                }

                if entries.is_empty() {
                    println!("  No messages recorded.");
                } else {
                    println!("  Message flow:\n");
                    for (idx, role, preview) in entries.iter().take(40) {
                        let role_tag = match role.as_str() {
                            "user" => "USR",
                            "assistant" => "AST",
                            "system" => "SYS",
                            "tool" | "ToolResult" => "TLR",
                            _ => role,
                        };
                        println!("    [{:>3}] {:>3}: {}", idx, role_tag, preview);
                    }
                    if entries.len() > 40 {
                        println!("    ... and {} more", entries.len() - 40);
                    }
                }
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    TaskTreeCommand,
    "task-tree",
    ["tt"],
    CommandCategory::Advanced,
    "Show conversation/task flow tree",
    cmd_task_tree
);

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(TaskRunCommand));
    registry.register(Arc::new(TaskGoalCommand));
    registry.register(Arc::new(TaskRequirementsCommand));
    registry.register(Arc::new(TaskRequirementSkipCommand));
    registry.register(Arc::new(SubagentMessageCommand));
    registry.register(Arc::new(SubagentFollowupCommand));
    registry.register(Arc::new(SubagentInterruptCommand));
    registry.register(Arc::new(TaskProgressCommand));
    registry.register(Arc::new(TaskTreeCommand));
}

#[cfg(test)]
mod tests {
    use super::parse_budget;

    #[test]
    fn continuation_budget_parser_accepts_positive_or_unbounded() {
        assert_eq!(parse_budget("42", "token"), Ok(Some(42)));
        assert_eq!(parse_budget("none", "token"), Ok(None));
        assert!(parse_budget("0", "time").is_err());
        assert!(parse_budget("1.5", "time").is_err());
    }
}
