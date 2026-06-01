//! Research paper commands — search, fetch, manage academic papers.
//!
//! Submits research tasks to the BackgroundTaskService for execution
//! via the research pipeline Graph workflow.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent_app_core::tasks::{BackgroundTaskKind, ResearchOutputFormat};
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

    let kind = BackgroundTaskKind::AgentChat {
        prompt: prompt.clone(),
        session_id: None,
    };

    match service
        .submit(
            kind,
            &format!("Fetch paper: {}", url),
            Some("cli".to_string()),
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
    let sub = args.first().copied().unwrap_or("help");

    match sub {
        "list" | "ls" | "" => {
            // List research tasks from the background service
            if let Some(ref service) = ctx.task_service {
                let tasks = service.list(None);
                let research_tasks: Vec<_> = tasks
                    .iter()
                    .filter(|t| t.tags.iter().any(|tag| tag == "bg:kind:research"))
                    .collect();

                if research_tasks.is_empty() {
                    println!("\n--- Research Tasks ---");
                    println!("  No research tasks found.");
                    println!("  Use /search-papers <topic> to start a new research task.");
                } else {
                    println!("\n--- Research Tasks ({}) ---", research_tasks.len());
                    for task in research_tasks {
                        let status_icon = match &task.status {
                            echo_agent_app_core::tasks::TaskStatus::Completed => "✓",
                            echo_agent_app_core::tasks::TaskStatus::Failed(_) => "✗",
                            echo_agent_app_core::tasks::TaskStatus::InProgress => "▶",
                            echo_agent_app_core::tasks::TaskStatus::Pending => "○",
                            echo_agent_app_core::tasks::TaskStatus::Cancelled => "⊘",
                            _ => "?",
                        };
                        println!(
                            "  {} {} — {} ({:?})",
                            status_icon, task.id, task.description, task.status
                        );
                        if let Some(ref result) = task.result {
                            let preview: String = result.chars().take(100).collect();
                            println!("    Result: {}...", preview);
                        }
                    }
                }
            } else {
                println!("\n--- Paper Workspace ---");
                println!("  Use /search-papers <topic> to find papers");
                println!("  Use /fetch-paper <url> to download and read a paper");
                println!("  Use /papers tools to list available research tools");
            }
        }
        "tools" => {
            println!("\n--- Research Tools ---");
            println!("  arxiv_search              Search ArXiv (keyword, category, sort)");
            println!("  semantic_scholar_search   Search Semantic Scholar (citations, fields)");
            println!("  pdf_fetch                 Download + parse PDF from URL");
            println!("  bibtex_generate           Generate BibTeX entries from paper metadata");
            println!();
            println!("  These tools are available when the agent is in research mode.");
            println!("  Switch with: /mode research");
        }
        "bib" => {
            println!("\n--- BibTeX Generation ---");
            println!("  Ask the agent to generate BibTeX:");
            println!("    \"Generate BibTeX for these papers: [list paper titles]\"");
            println!();
            println!("  Or use bibtex_generate tool with paper metadata from search results.");
        }
        "workflow" => {
            println!("\n--- Paper Writing Workflow ---");
            println!(
                "  1. /search-papers <topic>    — Submit research task (search + synthesize + write)"
            );
            println!("  2. /fetch-paper <url>        — Download and read a specific paper");
            println!("  3. /papers list              — Check research task status");
            println!("  4. /tasks status <id>        — View detailed task progress");
            println!("  5. /papers bib               — Generate BibTeX for references");
            println!();
            println!("  The research pipeline automatically:");
            println!("    - Searches arxiv + Semantic Scholar in parallel");
            println!("    - Fetches and analyzes top papers");
            println!("    - Synthesizes a literature review");
            println!("    - Generates a paper draft with citations");
        }
        _ => {
            println!("Usage: /papers [list|tools|bib|workflow]");
        }
    }
    CommandOutcome::Continue
}
cmd!(
    PapersCommand,
    "papers",
    CommandCategory::Advanced,
    "Manage research papers (list/tools/bib/workflow)",
    cmd_papers
);

// ── Register ────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(SearchPapersCommand));
    registry.register(Arc::new(FetchPaperCommand));
    registry.register(Arc::new(PapersCommand));
}
