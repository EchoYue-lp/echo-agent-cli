//! Research paper commands — search, fetch, manage academic papers.
//!
//! Submits research tasks to the BackgroundTaskService for execution
//! via the research pipeline Graph workflow.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent::tools::research::ZoteroLibraryKind;
use echo_agent_app_core::api::product_data_io::ScopedProductData;
use echo_agent_app_core::api::research::{
    CreateReviewRequest, CreateSourceRequest, ResearchResult, ReviewDomain, ReviewExportFormat,
    ReviewRecord, UpsertEvidenceRequest, add_source_tags, audit_review, create_review,
    create_source, delete_evidence, delete_review, delete_source, export_all_review_formats,
    export_review, get_review, get_source, list_evidence, list_reviews, list_sources, save_review,
    update_source_notes, upsert_evidence,
};
use echo_agent_app_core::api::research_connectors::{
    ResearchProvider, ScholarlySearchRequest, ZoteroSyncRequest, enrich_from_europe_pmc_scoped,
    export_zotero_scoped, import_zotero_scoped, search_and_ingest_scoped,
};
use echo_agent_app_core::api::tasks::{BackgroundTaskKind, ResearchOutputFormat};
use std::sync::Arc;

async fn research_io<T, F>(
    product_data: &ScopedProductData,
    operation: &'static str,
    function: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&std::path::Path) -> ResearchResult<T> + Send + 'static,
{
    product_data
        .data(operation, function)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

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

pub async fn execute_papers_command(product_data: &ScopedProductData, args: &[&str]) -> String {
    const USAGE: &str = "Usage: /papers list | show <source-id> | add-source <title> | update-notes <source-id> <notes> | add-tags <source-id> <tags...> | delete-source <source-id> | evidence [source-id] | upsert-evidence <json> | delete-evidence <id> | reviews | review <review-id> | create-review <academic|medical> <title> | save-review <expected-revision> <json> | delete-review <review-id> | search <openalex|crossref|europe-pmc> <query> | enrich <source-id> | audit <review-id> | export <review-id> <markdown|pdf|docx|json|csv|bibtex|ris|all> | zotero-import <user|group> <library-id> | zotero-export <user|group> <library-id> <source-id,...>";
    match args.first().copied().unwrap_or("list") {
        "list" | "ls" => match research_io(product_data, "list research sources", move |root| {
            list_sources(root, None, None)
        })
        .await
        {
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
            Some(source_id) => {
                let source_id = source_id.to_string();
                match research_io(product_data, "load research source", move |root| {
                    get_source(root, &source_id)
                })
                .await
                {
                    Ok(source) => pretty_json(&source),
                    Err(error) => format!("Unable to load source: {error}"),
                }
            }
            None => USAGE.to_string(),
        },
        "evidence" => {
            let source_id = args.get(1).map(|value| (*value).to_string());
            match research_io(product_data, "list research evidence", move |root| {
                list_evidence(root, source_id.as_deref(), None)
            })
            .await
            {
                Ok(records) if records.is_empty() => "No evidence records found.".to_string(),
                Ok(records) => pretty_json(&records),
                Err(error) => format!("Unable to list evidence: {error}"),
            }
        }
        "reviews" => match research_io(product_data, "list systematic reviews", list_reviews).await
        {
            Ok(reviews) if reviews.is_empty() => "No systematic reviews found.".to_string(),
            Ok(reviews) => pretty_json(&reviews),
            Err(error) => format!("Unable to list reviews: {error}"),
        },
        "review" => match args.get(1) {
            Some(review_id) => {
                let review_id = review_id.to_string();
                match research_io(product_data, "load systematic review", move |root| {
                    get_review(root, &review_id)
                })
                .await
                {
                    Ok(review) => pretty_json(&review),
                    Err(error) => format!("Unable to load review: {error}"),
                }
            }
            None => USAGE.to_string(),
        },
        "add-source" => {
            let title = args.get(1..).unwrap_or(&[]).join(" ");
            if title.trim().is_empty() {
                return USAGE.to_string();
            }
            match research_io(product_data, "create research source", move |root| {
                create_source(
                    root,
                    CreateSourceRequest {
                        title,
                        ..CreateSourceRequest::default()
                    },
                )
            })
            .await
            {
                Ok(source) => pretty_json(&source),
                Err(error) => format!("Unable to create source: {error}"),
            }
        }
        "update-notes" => {
            let (Some(source_id), Some(notes)) = (args.get(1), args.get(2..)) else {
                return USAGE.to_string();
            };
            let source_id = source_id.to_string();
            let notes = notes.join(" ");
            match research_io(product_data, "update research notes", move |root| {
                update_source_notes(root, &source_id, notes)
            })
            .await
            {
                Ok(source) => pretty_json(&source),
                Err(error) => format!("Unable to update source notes: {error}"),
            }
        }
        "add-tags" => {
            let (Some(source_id), Some(tags)) = (args.get(1), args.get(2..)) else {
                return USAGE.to_string();
            };
            if tags.is_empty() {
                return USAGE.to_string();
            }
            let source_id = source_id.to_string();
            let tags = tags.iter().map(|tag| (*tag).to_string()).collect();
            match research_io(product_data, "add research tags", move |root| {
                add_source_tags(root, &source_id, tags)
            })
            .await
            {
                Ok(source) => pretty_json(&source),
                Err(error) => format!("Unable to add source tags: {error}"),
            }
        }
        "delete-source" => {
            let Some(source_id) = args.get(1) else {
                return USAGE.to_string();
            };
            let source_id = source_id.to_string();
            match research_io(product_data, "delete research source", move |root| {
                delete_source(root, &source_id)
            })
            .await
            {
                Ok(()) => "Research source deleted.".to_string(),
                Err(error) => format!("Unable to delete source: {error}"),
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
            match research_io(product_data, "create systematic review", move |root| {
                create_review(
                    root,
                    CreateReviewRequest {
                        question: title.clone(),
                        title,
                        domain,
                    },
                )
            })
            .await
            {
                Ok(review) => pretty_json(&review),
                Err(error) => format!("Unable to create review: {error}"),
            }
        }
        "upsert-evidence" => {
            let json = args.get(1..).unwrap_or(&[]).join(" ");
            let request: UpsertEvidenceRequest = match serde_json::from_str(&json) {
                Ok(request) => request,
                Err(error) => return format!("Invalid evidence JSON: {error}"),
            };
            match research_io(product_data, "upsert research evidence", move |root| {
                upsert_evidence(root, request)
            })
            .await
            {
                Ok(record) => pretty_json(&record),
                Err(error) => format!("Unable to upsert evidence: {error}"),
            }
        }
        "delete-evidence" => {
            let Some(evidence_id) = args.get(1) else {
                return USAGE.to_string();
            };
            let evidence_id = evidence_id.to_string();
            match research_io(product_data, "delete research evidence", move |root| {
                delete_evidence(root, &evidence_id)
            })
            .await
            {
                Ok(()) => "Evidence record deleted.".to_string(),
                Err(error) => format!("Unable to delete evidence: {error}"),
            }
        }
        "save-review" => {
            let (Some(expected_revision), Some(json_parts)) = (args.get(1), args.get(2..)) else {
                return USAGE.to_string();
            };
            let expected_revision = expected_revision.to_string();
            let record: ReviewRecord = match serde_json::from_str(&json_parts.join(" ")) {
                Ok(record) => record,
                Err(error) => return format!("Invalid review JSON: {error}"),
            };
            let review_id = record.id.clone();
            match research_io(product_data, "save systematic review", move |root| {
                save_review(root, &review_id, record, &expected_revision)
            })
            .await
            {
                Ok(document) => pretty_json(&document),
                Err(error) => format!("Unable to save review: {error}"),
            }
        }
        "delete-review" => {
            let Some(review_id) = args.get(1) else {
                return USAGE.to_string();
            };
            let review_id = review_id.to_string();
            match research_io(product_data, "delete systematic review", move |root| {
                delete_review(root, &review_id)
            })
            .await
            {
                Ok(()) => "Systematic review deleted.".to_string(),
                Err(error) => format!("Unable to delete review: {error}"),
            }
        }
        "search" => {
            let provider = match args.get(1).copied() {
                Some("openalex") => ResearchProvider::Openalex,
                Some("crossref") => ResearchProvider::Crossref,
                Some("europe-pmc") => ResearchProvider::EuropePmc,
                _ => return USAGE.to_string(),
            };
            let query = args.get(2..).unwrap_or(&[]).join(" ");
            match search_and_ingest_scoped(
                product_data,
                ScholarlySearchRequest {
                    provider,
                    query,
                    limit: Some(20),
                    mailto: std::env::var("EKO_RESEARCH_MAILTO").ok(),
                },
            )
            .await
            {
                Ok(result) => pretty_json(&result),
                Err(error) => format!("Unable to search sources: {error}"),
            }
        }
        "enrich" => match args.get(1) {
            Some(source_id) => match enrich_from_europe_pmc_scoped(product_data, source_id).await {
                Ok(result) => pretty_json(&result),
                Err(error) => format!("Unable to enrich source: {error}"),
            },
            None => USAGE.to_string(),
        },
        "audit" => match args.get(1) {
            Some(review_id) => {
                let review_id = review_id.to_string();
                match research_io(product_data, "audit systematic review", move |root| {
                    audit_review(root, &review_id)
                })
                .await
                {
                    Ok(report) => pretty_json(&report),
                    Err(error) => format!("Unable to audit review: {error}"),
                }
            }
            None => USAGE.to_string(),
        },
        "export" => match (args.get(1), args.get(2).copied()) {
            (Some(review_id), Some("all")) => {
                let review_id = review_id.to_string();
                match research_io(product_data, "export all review formats", move |root| {
                    export_all_review_formats(root, &review_id)
                })
                .await
                {
                    Ok(artifacts) => pretty_json(&artifacts),
                    Err(error) => format!("Unable to export review: {error}"),
                }
            }
            (Some(review_id), Some(format)) => match parse_export_format(format) {
                Some(format) => {
                    let review_id = review_id.to_string();
                    match research_io(product_data, "export systematic review", move |root| {
                        export_review(root, &review_id, format)
                    })
                    .await
                    {
                        Ok(artifact) => pretty_json(&artifact),
                        Err(error) => format!("Unable to export review: {error}"),
                    }
                }
                None => USAGE.to_string(),
            },
            _ => USAGE.to_string(),
        },
        "zotero-import" => match zotero_request(args, false) {
            Some(request) => match import_zotero_scoped(product_data, request).await {
                Ok(result) => pretty_json(&result),
                Err(error) => format!("Unable to import Zotero library: {error}"),
            },
            None => USAGE.to_string(),
        },
        "zotero-export" => match zotero_request(args, true) {
            Some(request) => match export_zotero_scoped(product_data, request).await {
                Ok(result) => pretty_json(&result),
                Err(error) => format!("Unable to export to Zotero: {error}"),
            },
            None => USAGE.to_string(),
        },
        _ => USAGE.to_string(),
    }
}

fn parse_export_format(value: &str) -> Option<ReviewExportFormat> {
    match value {
        "markdown" => Some(ReviewExportFormat::Markdown),
        "pdf" => Some(ReviewExportFormat::Pdf),
        "docx" => Some(ReviewExportFormat::Docx),
        "json" => Some(ReviewExportFormat::Json),
        "csv" => Some(ReviewExportFormat::Csv),
        "bibtex" => Some(ReviewExportFormat::Bibtex),
        "ris" => Some(ReviewExportFormat::Ris),
        _ => None,
    }
}

fn zotero_request(args: &[&str], export: bool) -> Option<ZoteroSyncRequest> {
    let library_kind = match args.get(1).copied()? {
        "user" => ZoteroLibraryKind::User,
        "group" => ZoteroLibraryKind::Group,
        _ => return None,
    };
    let library_id = args.get(2)?.to_string();
    let source_ids = if export {
        args.get(3)?
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };
    let api_key = std::env::var("ZOTERO_API_KEY").ok()?;
    Some(ZoteroSyncRequest {
        library_kind,
        library_id,
        api_key,
        limit: Some(1_000),
        source_ids,
    })
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
