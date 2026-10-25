//! Research paper commands — search, fetch, manage academic papers.
//!
//! Submits research tasks to the BackgroundTaskService for execution
//! via the research pipeline Graph workflow.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent_app_core::api::tasks::{BackgroundTaskKind, ResearchOutputFormat};
pub use echo_agent_app_core::api::text_commands::research::execute_papers_command;
use std::sync::Arc;

// ── SearchPapersCommand ─────────────────────────────────────────────

async fn cmd_search_papers(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let query = args.join(" ");
    if query.is_empty() {
        println!("Usage: /search-papers <query>");
        println!("  Example: /search-papers transformer attention mechanism");
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

    println!("\n=== Paper Search: '{}' ===\n", query);

    let kind = BackgroundTaskKind::Research {
        topic: query.clone(),
        max_papers: 20,
        output_format: ResearchOutputFormat::Markdown,
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
cmd!(
    SearchPapersCommand,
    "search-papers",
    ["sp"],
    CommandCategory::Advanced,
    "Search arxiv and Semantic Scholar for papers (submits background research task)",
    cmd_search_papers
);

// ── FetchPaperCommand ───────────────────────────────────────────────

async fn cmd_fetch_paper(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let url = args.first().copied().unwrap_or("");
    if url.is_empty() {
        println!("Usage: /fetch-paper <pdf-url>");
        println!("  Example: /fetch-paper https://arxiv.org/pdf/1706.03762");
        return CommandOutcome::Continue;
    }

    // Submit as an agent chat task to fetch and summarize the paper
    let prompt = format!(
        "Download and analyze this paper: {}\n\
         Provide: title, authors, year, key findings, methodology, and a summary.",
        url
    );

    let service = match &ctx.task_service {
        Some(s) => s.clone(),
        None => {
            println!("  Background task service not available. Asking agent directly...");
            return CommandOutcome::Chat(prompt);
        }
    };

    // Phase 3.5: AgentChat variant deleted; submit as a Run directly.
    match service
        .submit_run(
            &prompt,
            &format!("Fetch paper: {}", url),
            "background",
            "cli",
        )
        .await
    {
        Ok(task_id) => {
            println!("  Submitted paper fetch task: {}", task_id);
            println!("  Monitor: /tasks status {}", task_id);
        }
        Err(e) => {
            println!("  Failed to submit: {}. Falling back to direct chat.", e);
            return CommandOutcome::Chat(prompt);
        }
    }

    CommandOutcome::Continue
}
cmd!(
    FetchPaperCommand,
    "fetch-paper",
    ["fp"],
    CommandCategory::Advanced,
    "Download and parse a PDF paper from URL",
    cmd_fetch_paper
);

// ── PapersCommand ───────────────────────────────────────────────────

async fn cmd_papers(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let output = match ctx.app_state.as_ref() {
        Some(state) => match state.current_product_data().await {
            Ok(product_data) => execute_papers_command(&product_data, args).await,
            Err(error) => format!("Research workspace is unavailable: {error}"),
        },
        None => "Research workspace is unavailable.".to_string(),
    };
    println!("{output}");
    CommandOutcome::Continue
}

cmd!(
    PapersCommand,
    "papers",
    CommandCategory::Advanced,
    "Manage the file-backed research library and systematic reviews",
    cmd_papers
);

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(SearchPapersCommand));
    registry.register(Arc::new(FetchPaperCommand));
    registry.register(Arc::new(PapersCommand));
}
