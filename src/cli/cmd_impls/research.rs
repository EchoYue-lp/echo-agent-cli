//! Research paper commands — search, fetch, manage academic papers.
//!
//! Submits research tasks to the BackgroundTaskService for execution
//! via the research pipeline Graph workflow.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent_app_core::analysis::workspace_root_for_agent;
use echo_agent_app_core::research::{
    CreateReviewRequest, CreateSourceRequest, ReviewDomain, create_review, create_source,
    get_review, get_source, list_evidence, list_reviews, list_sources,
};
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
    println!("{}", execute_papers_command(&ctx.agent, args).await);
    CommandOutcome::Continue
}

pub async fn execute_papers_command(
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    args: &[&str],
) -> String {
    const USAGE: &str = "Usage: /papers list | show <source-id> | evidence [source-id] | reviews | review <review-id> | add-source <title> | create-review <academic|medical> <title>";
    let root = workspace_root_for_agent(agent).await;
    match args.first().copied().unwrap_or("list") {
        "list" | "ls" => match list_sources(&root, None, None) {
            Ok(sources) if sources.is_empty() => "No sources in the research library.".to_string(),
            Ok(sources) => sources
                .iter()
                .map(|source| {
                    format!(
                        "{}  {}{}",
                        source.id,
                        source.title,
                        source
                            .year
                            .map(|year| format!(" ({year})"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Err(error) => format!("Unable to list sources: {error}"),
        },
        "show" => match args.get(1) {
            Some(source_id) => match get_source(&root, source_id) {
                Ok(source) => pretty_json(&source),
                Err(error) => format!("Unable to load source: {error}"),
            },
            None => USAGE.to_string(),
        },
        "evidence" => match list_evidence(&root, args.get(1).copied(), None) {
            Ok(records) if records.is_empty() => "No evidence records found.".to_string(),
            Ok(records) => pretty_json(&records),
            Err(error) => format!("Unable to list evidence: {error}"),
        },
        "reviews" => match list_reviews(&root) {
            Ok(reviews) if reviews.is_empty() => "No systematic reviews found.".to_string(),
            Ok(reviews) => pretty_json(&reviews),
            Err(error) => format!("Unable to list reviews: {error}"),
        },
        "review" => match args.get(1) {
            Some(review_id) => match get_review(&root, review_id) {
                Ok(review) => pretty_json(&review),
                Err(error) => format!("Unable to load review: {error}"),
            },
            None => USAGE.to_string(),
        },
        "add-source" => {
            let title = args.get(1..).unwrap_or(&[]).join(" ");
            if title.trim().is_empty() {
                return USAGE.to_string();
            }
            match create_source(
                &root,
                CreateSourceRequest {
                    title,
                    ..CreateSourceRequest::default()
                },
            ) {
                Ok(source) => pretty_json(&source),
                Err(error) => format!("Unable to create source: {error}"),
            }
        }
        "create-review" => {
            let domain = match args.get(1).copied() {
                Some("academic") => ReviewDomain::Academic,
                Some("medical") => ReviewDomain::Medical,
                _ => return USAGE.to_string(),
            };
            let title = args.get(2..).unwrap_or(&[]).join(" ");
            if title.trim().is_empty() {
                return USAGE.to_string();
            }
            match create_review(
                &root,
                CreateReviewRequest {
                    question: title.clone(),
                    title,
                    domain,
                },
            ) {
                Ok(review) => pretty_json(&review),
                Err(error) => format!("Unable to create review: {error}"),
            }
        }
        _ => USAGE.to_string(),
    }
}

fn pretty_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|error| format!("Unable to format research record: {error}"))
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
