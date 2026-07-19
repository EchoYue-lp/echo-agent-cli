//! EKO research integrations and automatic search-result ingestion.

use std::path::{Path, PathBuf};

use chrono::Utc;
use echo_agent::agent::ReactAgent;
use echo_agent::tools::research::{
    CrossrefClient, EuropePmcClient, OpenAlexClient, ScholarlySearchPage, ScholarlyWork,
    ZoteroClient, ZoteroLibraryKind, scholarly_work_from_zotero, scholarly_work_to_zotero,
};
use echo_agent::tools::{Tool, ToolParameters, ToolResult, ToolRiskLevel};
use echo_core::tools::ToolContext;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::research::{
    BiomedicalEntity, CreateSourceRequest, EuropePmcSupplement, ResearchError, ResearchResult,
    SourceIngestResult, SourceKind, SourceProvenance, SourceRecord, get_source, ingest_source,
    save_europe_pmc_supplement, write_full_text_xml,
};

const AUTO_INGEST_TOOLS: &[&str] = &[
    "arxiv_search",
    "semantic_scholar_search",
    "pubmed_search",
    "clinical_trials_search",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchProvider {
    Openalex,
    Crossref,
    EuropePmc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScholarlySearchRequest {
    pub provider: ResearchProvider,
    pub query: String,
    pub limit: Option<usize>,
    pub mailto: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScholarlyIngestResult {
    pub provider: ResearchProvider,
    pub total: Option<u64>,
    pub created: usize,
    pub updated: usize,
    #[serde(default)]
    pub sources: Vec<SourceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroSyncRequest {
    pub library_kind: ZoteroLibraryKind,
    pub library_id: String,
    pub api_key: String,
    pub limit: Option<usize>,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroSyncResult {
    pub imported: usize,
    pub updated: usize,
    pub exported: usize,
    #[serde(default)]
    pub sources: Vec<SourceRecord>,
    pub provider_response: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EuropePmcEnrichmentResult {
    pub source: SourceRecord,
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub async fn search_and_ingest(
    workspace_root: &Path,
    request: ScholarlySearchRequest,
) -> ResearchResult<ScholarlyIngestResult> {
    let query = request.query.trim();
    if query.is_empty() {
        return Err(ResearchError::Invalid(
            "scholarly search query cannot be empty".to_string(),
        ));
    }
    let limit = request.limit.unwrap_or(20).clamp(1, 100);
    let page = match request.provider {
        ResearchProvider::Openalex => OpenAlexClient::new(request.mailto)
            .map_err(external)?
            .search(query, limit)
            .await
            .map_err(external)?,
        ResearchProvider::Crossref => CrossrefClient::new(request.mailto)
            .map_err(external)?
            .search(query, limit)
            .await
            .map_err(external)?,
        ResearchProvider::EuropePmc => EuropePmcClient::new()
            .map_err(external)?
            .search(query, limit)
            .await
            .map_err(external)?,
    };
    ingest_page(workspace_root, request.provider, query, page)
}

pub async fn import_zotero(
    workspace_root: &Path,
    request: ZoteroSyncRequest,
) -> ResearchResult<ZoteroSyncResult> {
    let client = zotero_client(&request)?;
    let items = client
        .list_items(request.limit.unwrap_or(1_000).clamp(1, 10_000))
        .await
        .map_err(external)?;
    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut sources = Vec::new();
    for item in items {
        let Some(source_kind) = zotero_source_kind(&item.data.item_type) else {
            continue;
        };
        if item.data.title.trim().is_empty() {
            continue;
        }
        let mut work = scholarly_work_from_zotero(&item);
        work.provider_id = item.key;
        let mut source_request = source_request_from_work(work, None, Some("zotero".to_string()));
        source_request.source_kind = Some(source_kind);
        let result = ingest_source(workspace_root, source_request)?;
        if result.created {
            imported = imported.saturating_add(1);
        } else {
            updated = updated.saturating_add(1);
        }
        sources.push(result.source);
    }
    Ok(ZoteroSyncResult {
        imported,
        updated,
        exported: 0,
        sources,
        provider_response: None,
    })
}

pub async fn export_zotero(
    workspace_root: &Path,
    request: ZoteroSyncRequest,
) -> ResearchResult<ZoteroSyncResult> {
    if request.source_ids.is_empty() {
        return Err(ResearchError::Invalid(
            "Zotero export requires at least one source ID".to_string(),
        ));
    }
    let mut sources = Vec::new();
    for source_id in &request.source_ids {
        sources.push(get_source(workspace_root, source_id)?);
    }
    let items = sources
        .iter()
        .map(|source| {
            let mut item = scholarly_work_to_zotero(&source_to_scholarly_work(source));
            item.item_type = zotero_item_type(source.source_kind).to_string();
            item
        })
        .collect::<Vec<_>>();
    let response = zotero_client(&request)?
        .create_items(&items)
        .await
        .map_err(external)?;
    Ok(ZoteroSyncResult {
        imported: 0,
        updated: 0,
        exported: items.len(),
        sources,
        provider_response: Some(response),
    })
}

pub async fn enrich_from_europe_pmc(
    workspace_root: &Path,
    source_id: &str,
) -> ResearchResult<EuropePmcEnrichmentResult> {
    let source = get_source(workspace_root, source_id)?;
    let (provider_source, provider_id) = if let Some(pmcid) = source.pmcid.as_deref() {
        ("PMC", pmcid)
    } else if let Some(pmid) = source.pmid.as_deref() {
        ("MED", pmid)
    } else {
        return Err(ResearchError::Invalid(
            "Europe PMC enrichment requires a PMID or PMCID".to_string(),
        ));
    };
    let client = EuropePmcClient::new().map_err(external)?;
    let mut warnings = Vec::new();
    let citations = match client.citations(provider_source, provider_id).await {
        Ok(items) => items.into_iter().map(|item| item.id).collect(),
        Err(error) => {
            warnings.push(format!("citations: {error}"));
            Vec::new()
        }
    };
    let references = match client.references(provider_source, provider_id).await {
        Ok(items) => items.into_iter().map(|item| item.id).collect(),
        Err(error) => {
            warnings.push(format!("references: {error}"));
            Vec::new()
        }
    };
    let entities = match client.text_mined_terms(provider_source, provider_id).await {
        Ok(items) => items
            .into_iter()
            .map(|item| BiomedicalEntity {
                name: item.name,
                semantic_type: item.semantic_type,
                frequency: item.frequency,
            })
            .collect(),
        Err(error) => {
            warnings.push(format!("text-mined terms: {error}"));
            Vec::new()
        }
    };
    let full_text_path = if let Some(pmcid) = source.pmcid.as_deref() {
        match client.full_text_xml(pmcid).await {
            Ok(xml) => Some(write_full_text_xml(workspace_root, source_id, &xml)?),
            Err(error) => {
                warnings.push(format!("full text: {error}"));
                None
            }
        }
    } else {
        None
    };
    let source = save_europe_pmc_supplement(
        workspace_root,
        source_id,
        EuropePmcSupplement {
            citation_ids: citations,
            reference_ids: references,
            biomedical_entities: entities,
            full_text_path,
            enriched_at: Some(Utc::now()),
        },
    )?;
    Ok(EuropePmcEnrichmentResult { source, warnings })
}

pub fn ingest_tool_output(
    workspace_root: &Path,
    tool: &str,
    output: &str,
) -> ResearchResult<Vec<SourceIngestResult>> {
    if !AUTO_INGEST_TOOLS.contains(&tool) {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(output)?;
    let query = value
        .get("query")
        .and_then(Value::as_str)
        .map(str::to_string);
    let records = value
        .get("papers")
        .or_else(|| value.get("studies"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    records
        .iter()
        .filter_map(|record| source_request_from_tool_record(tool, query.as_deref(), record))
        .map(|request| ingest_source(workspace_root, request))
        .collect()
}

struct AutoIngestResearchTool {
    inner: Box<dyn Tool>,
}

impl AutoIngestResearchTool {
    fn new(inner: Box<dyn Tool>) -> Self {
        Self { inner }
    }
}

impl Tool for AutoIngestResearchTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> Value {
        self.inner.parameters()
    }

    fn execute_with_context<'a>(
        &'a self,
        parameters: ToolParameters,
        context: &'a ToolContext,
    ) -> BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let result = self.inner.execute_with_context(parameters, context).await?;
            if result.success {
                let workspace_root = context
                    .working_dir
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| PathBuf::from("."));
                match ingest_tool_output(&workspace_root, self.name(), &result.output) {
                    Ok(records) if !records.is_empty() => {
                        let created = records.iter().filter(|record| record.created).count();
                        tracing::info!(
                            tool = self.name(),
                            created,
                            total = records.len(),
                            "research results ingested"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(tool = self.name(), %error, "research result ingestion failed")
                    }
                }
            }
            Ok(result)
        })
    }

    fn validate_parameters<'a>(
        &'a self,
        parameters: &'a ToolParameters,
    ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
        self.inner.validate_parameters(parameters)
    }

    fn permissions(&self) -> Vec<echo_agent::tools::permission::ToolPermission> {
        self.inner.permissions()
    }

    fn risk_level(&self) -> ToolRiskLevel {
        self.inner.risk_level()
    }

    fn exempt_from_batch_timeout(&self) -> bool {
        self.inner.exempt_from_batch_timeout()
    }
}

pub fn install_auto_ingest_tools(agent: &mut ReactAgent) {
    for tool_name in AUTO_INGEST_TOOLS {
        if let Some(tool) = agent.remove_tool(tool_name) {
            agent.add_tool(Box::new(AutoIngestResearchTool::new(tool)));
        }
    }
}

fn ingest_page(
    workspace_root: &Path,
    provider: ResearchProvider,
    query: &str,
    page: ScholarlySearchPage,
) -> ResearchResult<ScholarlyIngestResult> {
    let mut created = 0usize;
    let mut updated = 0usize;
    let mut sources = Vec::new();
    for work in page.works {
        let result = ingest_source(
            workspace_root,
            source_request_from_work(work, Some(query.to_string()), None),
        )?;
        if result.created {
            created = created.saturating_add(1);
        } else {
            updated = updated.saturating_add(1);
        }
        sources.push(result.source);
    }
    Ok(ScholarlyIngestResult {
        provider,
        total: page.total,
        created,
        updated,
        sources,
    })
}

fn source_request_from_work(
    work: ScholarlyWork,
    query: Option<String>,
    provider_override: Option<String>,
) -> CreateSourceRequest {
    let provider = provider_override.unwrap_or_else(|| work.provider.clone());
    let zotero_key = (provider == "zotero").then_some(work.provider_id.clone());
    let record_url = work.url.clone();
    CreateSourceRequest {
        source_kind: Some(SourceKind::JournalArticle),
        title: work.title,
        authors: work.authors,
        abstract_text: work.abstract_text,
        doi: work.doi,
        pmid: work.pmid,
        pmcid: work.pmcid,
        arxiv_id: work.arxiv_id,
        openalex_id: work.openalex_id,
        zotero_key,
        year: work.year,
        venue: work.venue,
        url: work.url,
        tags: work.keywords,
        provenance: vec![SourceProvenance {
            provider,
            query,
            retrieved_at: Utc::now(),
            record_url,
        }],
        ..CreateSourceRequest::default()
    }
}

fn source_request_from_tool_record(
    tool: &str,
    query: Option<&str>,
    record: &Value,
) -> Option<CreateSourceRequest> {
    let title = text(record, "title")?;
    let provider = match tool {
        "arxiv_search" => "arxiv",
        "semantic_scholar_search" => "semantic_scholar",
        "pubmed_search" => "pubmed",
        "clinical_trials_search" => "clinicaltrials.gov",
        _ => return None,
    };
    let trial = tool == "clinical_trials_search";
    let url = text(record, "url").or_else(|| text(record, "pdf_url"));
    Some(CreateSourceRequest {
        source_kind: Some(if trial {
            SourceKind::TrialRegistration
        } else if tool == "arxiv_search" {
            SourceKind::Preprint
        } else {
            SourceKind::JournalArticle
        }),
        title,
        authors: string_array(record, "authors"),
        abstract_text: text(record, "abstract"),
        doi: text(record, "doi"),
        pmid: text(record, "pmid"),
        arxiv_id: text(record, "arxiv_id"),
        clinical_trial_id: text(record, "nct_id"),
        year: integer(record, "year").or_else(|| {
            text(record, "published")
                .as_deref()
                .and_then(first_year)
                .or_else(|| text(record, "start_date").as_deref().and_then(first_year))
        }),
        venue: text(record, "venue").or_else(|| text(record, "journal")),
        url: url.clone(),
        tags: string_array(record, "categories")
            .into_iter()
            .chain(string_array(record, "fields_of_study"))
            .chain(string_array(record, "mesh_terms"))
            .chain(string_array(record, "conditions"))
            .collect(),
        notes: trial.then(|| trial_notes(record)),
        provenance: vec![SourceProvenance {
            provider: provider.to_string(),
            query: query.map(str::to_string),
            retrieved_at: Utc::now(),
            record_url: url,
        }],
        ..CreateSourceRequest::default()
    })
}

fn source_to_scholarly_work(source: &SourceRecord) -> ScholarlyWork {
    ScholarlyWork {
        provider: "eko".to_string(),
        provider_id: source.id.clone(),
        title: source.title.clone(),
        authors: source.authors.clone(),
        abstract_text: source.abstract_text.clone(),
        doi: source.doi.clone(),
        pmid: source.pmid.clone(),
        pmcid: source.pmcid.clone(),
        arxiv_id: source.arxiv_id.clone(),
        openalex_id: source.openalex_id.clone(),
        year: source.year,
        venue: source.venue.clone(),
        url: source.url.clone(),
        keywords: source.tags.clone(),
    }
}

fn zotero_source_kind(item_type: &str) -> Option<SourceKind> {
    match item_type {
        "journalArticle" => Some(SourceKind::JournalArticle),
        "conferencePaper" => Some(SourceKind::ConferencePaper),
        "book" | "bookSection" => Some(SourceKind::Book),
        "dataset" => Some(SourceKind::Dataset),
        "preprint" => Some(SourceKind::Preprint),
        "webpage" | "blogPost" | "forumPost" => Some(SourceKind::Web),
        "report" | "document" | "statute" | "case" => Some(SourceKind::Guideline),
        "attachment" | "note" | "annotation" => None,
        _ => Some(SourceKind::Other),
    }
}

fn zotero_item_type(source_kind: SourceKind) -> &'static str {
    match source_kind {
        SourceKind::JournalArticle => "journalArticle",
        SourceKind::Preprint => "preprint",
        SourceKind::ConferencePaper => "conferencePaper",
        SourceKind::Book => "book",
        SourceKind::Dataset => "dataset",
        SourceKind::Guideline => "report",
        SourceKind::TrialRegistration | SourceKind::Web | SourceKind::Other => "webpage",
    }
}

fn zotero_client(request: &ZoteroSyncRequest) -> ResearchResult<ZoteroClient> {
    ZoteroClient::new(
        request.library_kind,
        request.library_id.clone(),
        request.api_key.clone(),
    )
    .map_err(external)
}

fn external(error: impl std::fmt::Display) -> ResearchError {
    ResearchError::External(error.to_string())
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn integer(value: &Value, key: &str) -> Option<i32> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|number| i32::try_from(number).ok())
        .or_else(|| text(value, key).and_then(|number| number.parse::<i32>().ok()))
}

fn first_year(value: &str) -> Option<i32> {
    let digits: String = value.chars().filter(char::is_ascii_digit).take(4).collect();
    (digits.chars().count() == 4)
        .then(|| digits.parse::<i32>().ok())
        .flatten()
}

fn trial_notes(record: &Value) -> String {
    let status = text(record, "status").unwrap_or_default();
    let phase = text(record, "phase").unwrap_or_default();
    let interventions = string_array(record, "interventions").join("; ");
    let outcomes = string_array(record, "primary_outcomes").join("; ");
    format!(
        "Status: {status}\nPhase: {phase}\nInterventions: {interventions}\nPrimary outcomes: {outcomes}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_are_ingested_idempotently() -> ResearchResult<()> {
        let workspace = tempfile::tempdir().map_err(ResearchError::Io)?;
        let output = serde_json::json!({
            "query": "test",
            "papers": [{
                "title": "A Paper", "authors": ["A. Author"], "doi": "10.1/test",
                "year": 2025, "abstract": "Finding"
            }]
        })
        .to_string();
        let first = ingest_tool_output(workspace.path(), "semantic_scholar_search", &output)?;
        let second = ingest_tool_output(workspace.path(), "semantic_scholar_search", &output)?;
        assert_eq!(first.first().map(|record| record.created), Some(true));
        assert_eq!(second.first().map(|record| record.created), Some(false));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "opt-in live provider ingestion smoke test; set EKO_PROVIDER_SMOKE=1"]
    async fn live_open_provider_results_are_persisted() -> ResearchResult<()> {
        if std::env::var("EKO_PROVIDER_SMOKE").as_deref() != Ok("1") {
            return Err(ResearchError::Invalid(
                "set EKO_PROVIDER_SMOKE=1 before running ignored provider smoke tests".to_string(),
            ));
        }
        let workspace = tempfile::tempdir().map_err(ResearchError::Io)?;
        for provider in [
            ResearchProvider::Openalex,
            ResearchProvider::Crossref,
            ResearchProvider::EuropePmc,
        ] {
            let result = search_and_ingest(
                workspace.path(),
                ScholarlySearchRequest {
                    provider,
                    query: "systematic review".to_string(),
                    limit: Some(1),
                    mailto: std::env::var("OPENALEX_MAILTO").ok(),
                },
            )
            .await?;
            if result.sources.is_empty() {
                return Err(ResearchError::External(format!(
                    "provider {provider:?} returned no persisted sources"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires ZOTERO_API_KEY and ZOTERO_LIBRARY_ID"]
    async fn live_zotero_import_persists_source_records() -> ResearchResult<()> {
        let api_key = std::env::var("ZOTERO_API_KEY").map_err(|_| {
            ResearchError::Invalid("ZOTERO_API_KEY is required for the Zotero smoke test".into())
        })?;
        let library_id = std::env::var("ZOTERO_LIBRARY_ID").map_err(|_| {
            ResearchError::Invalid("ZOTERO_LIBRARY_ID is required for the Zotero smoke test".into())
        })?;
        let library_kind = match std::env::var("ZOTERO_LIBRARY_KIND")
            .unwrap_or_else(|_| "user".to_string())
            .as_str()
        {
            "user" => ZoteroLibraryKind::User,
            "group" => ZoteroLibraryKind::Group,
            value => {
                return Err(ResearchError::Invalid(format!(
                    "ZOTERO_LIBRARY_KIND must be user or group, got {value}"
                )));
            }
        };
        let workspace = tempfile::tempdir().map_err(ResearchError::Io)?;
        let result = import_zotero(
            workspace.path(),
            ZoteroSyncRequest {
                library_kind,
                library_id,
                api_key,
                limit: Some(1),
                source_ids: Vec::new(),
            },
        )
        .await?;
        if result.sources.is_empty() {
            return Err(ResearchError::External(
                "Zotero library returned no importable source records".to_string(),
            ));
        }
        Ok(())
    }
}
