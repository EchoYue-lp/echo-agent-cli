//! Data analysis and writing pipeline commands.
//!
//! Submits data/writing tasks to the BackgroundTaskService for execution
//! through the shared TaskRuntime and domain-aware Subagents.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent_app_core::tasks::BackgroundTaskKind;

// ── AnalyzeCommand ──────────────────────────────────────────────────

async fn cmd_analyze(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let dataset_path = args.join(" ");
    if dataset_path.is_empty() {
        println!("Usage: /analyze <dataset-path>");
        println!("  Example: /analyze data/sales_2024.csv");
        println!(
            "  This creates a reviewable analysis script, executes it, and records lineage and artifacts"
        );
        return CommandOutcome::Continue;
    }

    let service = match &ctx.task_service {
        Some(s) => s.clone(),
        None => {
            println!("  Background task service not available (start in web or both mode).");
            return CommandOutcome::Continue;
        }
    };

    println!("\n=== Data Analysis: '{}' ===\n", dataset_path);

    let kind = BackgroundTaskKind::DataPipeline {
        dataset_path: dataset_path.clone(),
        objective: None,
        max_charts: 3,
    };

    match service
        .submit(
            kind,
            &format!("Data analysis: {}", dataset_path),
            Some("cli".to_string()),
        )
        .await
    {
        Ok(task_id) => {
            println!("  Submitted data analysis task: {}", task_id);
            println!("  Monitor progress: /tasks status {}", task_id);
            println!("  Cancel: /tasks cancel {}", task_id);
        }
        Err(e) => {
            println!("  Failed to submit data analysis task: {}", e);
        }
    }

    CommandOutcome::Continue
}
cmd!(
    AnalyzeCommand,
    "analyze",
    ["da"],
    CommandCategory::Advanced,
    "Run data analysis pipeline on a dataset",
    cmd_analyze
);

// ── WriteCommand ────────────────────────────────────────────────────

async fn cmd_write(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let topic = args.join(" ");
    if topic.is_empty() {
        println!("Usage: /write <topic>");
        println!("  Example: /write introduction to machine learning");
        println!(
            "  This will run a writing pipeline: outline -> draft -> review-revise loop -> finalize"
        );
        return CommandOutcome::Continue;
    }

    let service = match &ctx.task_service {
        Some(s) => s.clone(),
        None => {
            println!("  Background task service not available (start in web or both mode).");
            return CommandOutcome::Continue;
        }
    };

    println!("\n=== Writing: '{}' ===\n", topic);

    let kind = BackgroundTaskKind::WritingPipeline {
        topic: topic.clone(),
        audience: "general audience".to_string(),
        format: "markdown".to_string(),
        max_revisions: 3,
        quality_threshold: 7,
    };

    match service
        .submit(kind, &format!("Write: {}", topic), Some("cli".to_string()))
        .await
    {
        Ok(task_id) => {
            println!("  Submitted writing task: {}", task_id);
            println!("  Monitor progress: /tasks status {}", task_id);
            println!("  Cancel: /tasks cancel {}", task_id);
        }
        Err(e) => {
            println!("  Failed to submit writing task: {}", e);
        }
    }

    CommandOutcome::Continue
}
cmd!(
    WriteCommand,
    "write",
    ["wp"],
    CommandCategory::Advanced,
    "Run writing pipeline for a document topic",
    cmd_write
);

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    // Commands moved to /pipeline subcommand group (see pipeline.rs)
    let _ = registry;
}
