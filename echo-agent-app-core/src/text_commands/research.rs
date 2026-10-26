//! Existing file-backed research text commands.
use super::ManagementCommandError;
use crate::product_data_io::ScopedProductData;
use crate::research::{
    CreateReviewRequest, CreateSourceRequest, ResearchResult, ReviewDomain, ReviewExportFormat,
    ReviewRecord, UpsertEvidenceRequest, add_source_tags, audit_review, create_review,
    create_source, delete_evidence, delete_review, delete_source, export_all_review_formats,
    export_review, get_review, get_source, list_evidence, list_reviews, list_sources, save_review,
    update_source_notes, upsert_evidence,
};
use crate::research_connectors::{
    ResearchProvider, ScholarlySearchRequest, ZoteroSyncRequest, enrich_from_europe_pmc_scoped,
    export_zotero_scoped, import_zotero_scoped, search_and_ingest_scoped,
};
use echo_agent::tools::research::ZoteroLibraryKind;

async fn research_io<T, F>(
    product_data: &ScopedProductData,
    operation: &'static str,
    may_mutate: bool,
    function: F,
) -> Result<T, ManagementCommandError>
where
    T: Send + 'static,
    F: FnOnce(&std::path::Path) -> ResearchResult<T> + Send + 'static,
{
    product_data
        .data(operation, function)
        .await
        .map_err(|error| ManagementCommandError::product_io(error, may_mutate))?
        .map_err(|error| ManagementCommandError::native(error, may_mutate))
}

pub async fn execute_papers_command(product_data: &ScopedProductData, args: &[&str]) -> String {
    execute_papers_command_result(product_data, args)
        .await
        .unwrap_or_else(|error| error.to_string())
}

/// One command path for ordinary text and observed controls; never re-execute for rendering.
pub(crate) async fn execute_papers_command_result(
    product_data: &ScopedProductData,
    args: &[&str],
) -> Result<String, ManagementCommandError> {
    const USAGE: &str = "Usage: /papers list | show <source-id> | add-source <title> | update-notes <source-id> <notes> | add-tags <source-id> <tags...> | delete-source <source-id> | evidence [source-id] | upsert-evidence <json> | delete-evidence <id> | reviews | review <review-id> | create-review <academic|medical> <title> | save-review <expected-revision> <json> | delete-review <review-id> | search <openalex|crossref|europe-pmc> <query> | enrich <source-id> | audit <review-id> | export <review-id> <markdown|pdf|docx|json|csv|bibtex|ris|all> | zotero-import <user|group> <library-id> | zotero-export <user|group> <library-id> <source-id,...>";
    Ok(match args.first().copied().unwrap_or("list") {
        "list" | "ls" => {
            match research_io(product_data, "list research sources", false, move |root| {
                list_sources(root, None, None)
            })
            .await
            {
                Ok(sources) if sources.is_empty() => {
                    "No sources in the research library.".to_string()
                }
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
                Err(error) => return Err(error.context("Unable to list sources: ")),
            }
        }
        "show" => match args.get(1) {
            Some(source_id) => {
                let source_id = source_id.to_string();
                match research_io(product_data, "load research source", false, move |root| {
                    get_source(root, &source_id)
                })
                .await
                {
                    Ok(source) => pretty_json(&source),
                    Err(error) => return Err(error.context("Unable to load source: ")),
                }
            }
            None => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
        },
        "evidence" => {
            let source_id = args.get(1).map(|value| (*value).to_string());
            match research_io(product_data, "list research evidence", false, move |root| {
                list_evidence(root, source_id.as_deref(), None)
            })
            .await
            {
                Ok(records) if records.is_empty() => "No evidence records found.".to_string(),
                Ok(records) => pretty_json(&records),
                Err(error) => return Err(error.context("Unable to list evidence: ")),
            }
        }
        "reviews" => {
            match research_io(product_data, "list systematic reviews", false, list_reviews).await {
                Ok(reviews) if reviews.is_empty() => "No systematic reviews found.".to_string(),
                Ok(reviews) => pretty_json(&reviews),
                Err(error) => return Err(error.context("Unable to list reviews: ")),
            }
        }
        "review" => match args.get(1) {
            Some(review_id) => {
                let review_id = review_id.to_string();
                match research_io(product_data, "load systematic review", false, move |root| {
                    get_review(root, &review_id)
                })
                .await
                {
                    Ok(review) => pretty_json(&review),
                    Err(error) => return Err(error.context("Unable to load review: ")),
                }
            }
            None => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
        },
        "add-source" => {
            let title = args.get(1..).unwrap_or(&[]).join(" ");
            if title.trim().is_empty() {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            }
            match research_io(product_data, "create research source", true, move |root| {
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
                Err(error) => return Err(error.context("Unable to create source: ")),
            }
        }
        "update-notes" => {
            let (Some(source_id), Some(notes)) = (args.get(1), args.get(2..)) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let source_id = source_id.to_string();
            let notes = notes.join(" ");
            match research_io(product_data, "update research notes", true, move |root| {
                update_source_notes(root, &source_id, notes)
            })
            .await
            {
                Ok(source) => pretty_json(&source),
                Err(error) => return Err(error.context("Unable to update source notes: ")),
            }
        }
        "add-tags" => {
            let (Some(source_id), Some(tags)) = (args.get(1), args.get(2..)) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            if tags.is_empty() {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            }
            let source_id = source_id.to_string();
            let tags = tags.iter().map(|tag| (*tag).to_string()).collect();
            match research_io(product_data, "add research tags", true, move |root| {
                add_source_tags(root, &source_id, tags)
            })
            .await
            {
                Ok(source) => pretty_json(&source),
                Err(error) => return Err(error.context("Unable to add source tags: ")),
            }
        }
        "delete-source" => {
            let Some(source_id) = args.get(1) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let source_id = source_id.to_string();
            match research_io(product_data, "delete research source", true, move |root| {
                delete_source(root, &source_id)
            })
            .await
            {
                Ok(()) => "Research source deleted.".to_string(),
                Err(error) => return Err(error.context("Unable to delete source: ")),
            }
        }
        "create-review" => {
            let domain = match args.get(1).copied() {
                Some("academic") => ReviewDomain::Academic,
                Some("medical") => ReviewDomain::Medical,
                _ => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
            };
            let title = args.get(2..).unwrap_or(&[]).join(" ");
            if title.trim().is_empty() {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            }
            match research_io(
                product_data,
                "create systematic review",
                true,
                move |root| {
                    create_review(
                        root,
                        CreateReviewRequest {
                            question: title.clone(),
                            title,
                            domain,
                        },
                    )
                },
            )
            .await
            {
                Ok(review) => pretty_json(&review),
                Err(error) => return Err(error.context("Unable to create review: ")),
            }
        }
        "upsert-evidence" => {
            let json = args.get(1..).unwrap_or(&[]).join(" ");
            let request: UpsertEvidenceRequest = match serde_json::from_str(&json) {
                Ok(request) => request,
                Err(error) => {
                    return Err(ManagementCommandError::InvalidInput(format!(
                        "Invalid evidence JSON: {error}"
                    )));
                }
            };
            match research_io(
                product_data,
                "upsert research evidence",
                true,
                move |root| upsert_evidence(root, request),
            )
            .await
            {
                Ok(record) => pretty_json(&record),
                Err(error) => return Err(error.context("Unable to upsert evidence: ")),
            }
        }
        "delete-evidence" => {
            let Some(evidence_id) = args.get(1) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let evidence_id = evidence_id.to_string();
            match research_io(
                product_data,
                "delete research evidence",
                true,
                move |root| delete_evidence(root, &evidence_id),
            )
            .await
            {
                Ok(()) => "Evidence record deleted.".to_string(),
                Err(error) => return Err(error.context("Unable to delete evidence: ")),
            }
        }
        "save-review" => {
            let (Some(expected_revision), Some(json_parts)) = (args.get(1), args.get(2..)) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let expected_revision = expected_revision.to_string();
            let record: ReviewRecord = match serde_json::from_str(&json_parts.join(" ")) {
                Ok(record) => record,
                Err(error) => {
                    return Err(ManagementCommandError::InvalidInput(format!(
                        "Invalid review JSON: {error}"
                    )));
                }
            };
            let review_id = record.id.clone();
            match research_io(product_data, "save systematic review", true, move |root| {
                save_review(root, &review_id, record, &expected_revision)
            })
            .await
            {
                Ok(document) => pretty_json(&document),
                Err(error) => return Err(error.context("Unable to save review: ")),
            }
        }
        "delete-review" => {
            let Some(review_id) = args.get(1) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let review_id = review_id.to_string();
            match research_io(
                product_data,
                "delete systematic review",
                true,
                move |root| delete_review(root, &review_id),
            )
            .await
            {
                Ok(()) => "Systematic review deleted.".to_string(),
                Err(error) => return Err(error.context("Unable to delete review: ")),
            }
        }
        "search" => {
            let provider = match args.get(1).copied() {
                Some("openalex") => ResearchProvider::Openalex,
                Some("crossref") => ResearchProvider::Crossref,
                Some("europe-pmc") => ResearchProvider::EuropePmc,
                _ => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
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
                Err(error) => {
                    return Err(ManagementCommandError::native(error, true)
                        .context("Unable to search sources: "));
                }
            }
        }
        "enrich" => match args.get(1) {
            Some(source_id) => match enrich_from_europe_pmc_scoped(product_data, source_id).await {
                Ok(result) => pretty_json(&result),
                Err(error) => {
                    return Err(ManagementCommandError::native(error, true)
                        .context("Unable to enrich source: "));
                }
            },
            None => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
        },
        "audit" => match args.get(1) {
            Some(review_id) => {
                let review_id = review_id.to_string();
                match research_io(
                    product_data,
                    "audit systematic review",
                    false,
                    move |root| audit_review(root, &review_id),
                )
                .await
                {
                    Ok(report) => pretty_json(&report),
                    Err(error) => return Err(error.context("Unable to audit review: ")),
                }
            }
            None => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
        },
        "export" => match (args.get(1), args.get(2).copied()) {
            (Some(review_id), Some("all")) => {
                let review_id = review_id.to_string();
                match research_io(
                    product_data,
                    "export all review formats",
                    true,
                    move |root| export_all_review_formats(root, &review_id),
                )
                .await
                {
                    Ok(artifacts) => pretty_json(&artifacts),
                    Err(error) => return Err(error.context("Unable to export review: ")),
                }
            }
            (Some(review_id), Some(format)) => match parse_export_format(format) {
                Some(format) => {
                    let review_id = review_id.to_string();
                    match research_io(
                        product_data,
                        "export systematic review",
                        true,
                        move |root| export_review(root, &review_id, format),
                    )
                    .await
                    {
                        Ok(artifact) => pretty_json(&artifact),
                        Err(error) => return Err(error.context("Unable to export review: ")),
                    }
                }
                None => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
            },
            _ => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
        },
        "zotero-import" => match zotero_request(args, false) {
            Some(request) => match import_zotero_scoped(product_data, request).await {
                Ok(result) => pretty_json(&result),
                Err(error) => {
                    return Err(ManagementCommandError::native(error, true)
                        .context("Unable to import Zotero library: "));
                }
            },
            None => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
        },
        "zotero-export" => match zotero_request(args, true) {
            Some(request) => match export_zotero_scoped(product_data, request).await {
                Ok(result) => pretty_json(&result),
                Err(error) => {
                    return Err(ManagementCommandError::native(error, true)
                        .context("Unable to export to Zotero: "));
                }
            },
            None => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
        },
        _ => return Err(ManagementCommandError::InvalidInput(USAGE.to_string())),
    })
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
