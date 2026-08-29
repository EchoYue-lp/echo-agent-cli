//! Pipeline 子命令组
//!
//! `/pipeline` 统一管理所有后台任务管道，替代旧的 `/analyze`、`/write` 等平铺命令。

use crate::cli::command::{
    CommandCategory, CommandContext, CommandOutcome, SlashCommand, SubCommandDef,
};
use echo_agent_app_core::api::tasks::BackgroundTaskKind;
use std::future::Future;
use std::pin::Pin;

pub struct PipelineCommand;

impl SlashCommand for PipelineCommand {
    fn name(&self) -> &'static str {
        "pipeline"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["pl"]
    }
    fn description(&self) -> &'static str {
        "Pipeline tasks (research/writing/data)"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Advanced
    }

    fn subcommands(&self) -> Vec<SubCommandDef> {
        vec![
            SubCommandDef {
                name: "research",
                aliases: &["r"],
                description: "Run research pipeline",
            },
            SubCommandDef {
                name: "writing",
                aliases: &["w"],
                description: "Run writing pipeline",
            },
            SubCommandDef {
                name: "data",
                aliases: &["d"],
                description: "Run data analysis pipeline",
            },
            SubCommandDef {
                name: "list",
                aliases: &["ls"],
                description: "List running pipelines",
            },
        ]
    }

    fn run<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: &'a [&'a str],
    ) -> Pin<Box<dyn Future<Output = CommandOutcome> + Send + 'a>> {
        Box::pin(async move {
            let subcommand = args.first().copied().unwrap_or("help");
            let sub_args = args.get(1..).unwrap_or(&[]);

            match subcommand {
                "research" | "r" => pipeline_research(ctx, sub_args).await,
                "writing" | "w" => pipeline_writing(ctx, sub_args).await,
                "data" | "d" => pipeline_data(ctx, sub_args).await,
                "list" | "ls" => pipeline_list(ctx, sub_args).await,
                "help" | "--help" | "-h" => {
                    print_pipeline_help();
                    CommandOutcome::Continue
                }
                _ => {
                    println!("Unknown pipeline subcommand: {subcommand}");
                    print_pipeline_help();
                    CommandOutcome::Continue
                }
            }
        })
    }
}

// ── Help ──────────────────────────────────────────────────────────────

fn print_pipeline_help() {
    println!("\n=== Pipeline Subcommands ===\n");
    println!("  /pipeline research [r] <topic>   Run research pipeline (search + synthesize)");
    println!("  /pipeline writing [w] <topic>    Run writing pipeline (outline + draft + review)");
    println!("  /pipeline data [d] <dataset>     Run data analysis pipeline");
    println!("  /pipeline list [ls]              List running pipelines");
    println!("  /pipeline help                   Show this help");
}

// ── research ──────────────────────────────────────────────────────────

async fn pipeline_research(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let query = args.join(" ");
    if query.is_empty() {
        println!("Usage: /pipeline research <topic>");
        println!("  Example: /pipeline research transformer attention mechanism");
        println!("  This will search arxiv and Semantic Scholar for matching papers.");
        return CommandOutcome::Continue;
    }

    let service = match &ctx.task_service {
        Some(s) => s.clone(),
        None => {
            println!("  Background task service not available (start in web or both mode).");
            return CommandOutcome::Continue;
        }
    };

    println!("\n=== Research Pipeline: '{}' ===\n", query);

    let kind = BackgroundTaskKind::Research {
        topic: query.clone(),
        max_papers: 20,
        output_format: echo_agent_app_core::api::tasks::ResearchOutputFormat::Markdown,
    };

    match service
        .submit(
            kind,
            &format!("Research: {}", query),
            Some("cli".to_string()),
        )
        .await
    {
        Ok(task_id) => {
            println!("  Submitted research task: {}", task_id);
            println!("  Monitor progress: /tasks status {}", task_id);
            println!("  Cancel: /tasks cancel {}", task_id);
        }
        Err(e) => {
            println!("  Failed to submit research task: {}", e);
        }
    }

    CommandOutcome::Continue
}

// ── writing ───────────────────────────────────────────────────────────

async fn pipeline_writing(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let topic = args.join(" ");
    if topic.is_empty() {
        println!("Usage: /pipeline writing <topic>");
        println!("  Example: /pipeline writing introduction to machine learning");
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

    println!("\n=== Writing Pipeline: '{}' ===\n", topic);

    let kind = BackgroundTaskKind::WritingPipeline {
        topic: topic.clone(),
        audience: "general audience".to_string(),
        format: "markdown".to_string(),
        max_revisions: 3,
        quality_threshold: 70,
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

// ── data ──────────────────────────────────────────────────────────────

async fn pipeline_data(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let dataset_path = args.join(" ");
    if dataset_path.is_empty() {
        println!("Usage: /pipeline data <dataset-path>");
        println!("  Example: /pipeline data data/sales_2024.csv");
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

// ── list ──────────────────────────────────────────────────────────────

async fn pipeline_list(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref service) = ctx.task_service {
        let tasks = service.list_unified(None).await;
        let pipeline_tasks: Vec<_> = tasks
            .iter()
            .filter(|t| {
                matches!(
                    t.kind.as_deref(),
                    Some(
                        "bg:kind:research"
                            | "bg:kind:research_to_writing"
                            | "bg:kind:data_pipeline"
                            | "bg:kind:writing_pipeline"
                    )
                )
            })
            .collect();

        if pipeline_tasks.is_empty() {
            println!("\n--- Pipelines ---");
            println!("  No running pipelines.");
            println!("  Use /pipeline research|writing|data to start a pipeline.");
        } else {
            println!("\n--- Pipelines ({}) ---", pipeline_tasks.len());
            for task in pipeline_tasks {
                let status_icon = match task.status.as_str() {
                    "completed" => "✓",
                    "failed" => "✗",
                    "in_progress" => "▶",
                    "pending" => "○",
                    "cancelled" => "⊘",
                    _ => "?",
                };
                println!(
                    "  {} {} — {} ({})",
                    status_icon, task.id, task.description, task.status
                );
            }
        }
    } else {
        println!("  Background task service not available (start in web or both mode).");
    }
    CommandOutcome::Continue
}

// ── Register ──────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(std::sync::Arc::new(PipelineCommand));
}
