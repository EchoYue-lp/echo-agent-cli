//! EKO research integrations and automatic search-result ingestion.

use std::path::Path;

use chrono::Utc;
use echo_agent::agent::ReactAgent;
use echo_agent::tools::ToolContext;
use echo_agent::tools::research::{
    CrossrefClient, EuropePmcClient, OpenAlexClient, ScholarlySearchPage, ScholarlyWork,
    ZoteroClient, ZoteroLibraryKind, scholarly_work_from_zotero, scholarly_work_to_zotero,
};
use echo_agent::tools::{Tool, ToolFailureCategory, ToolParameters, ToolResult, ToolRiskLevel};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::research::{
    BiomedicalEntity, CreateSourceRequest, EuropePmcEnrichmentAttempt, EuropePmcSupplementUpdate,
    ResearchError, ResearchResult, SourceIngestResult, SourceKind, SourceProvenance, SourceRecord,
    get_source, ingest_source, save_europe_pmc_supplement, write_full_text_xml,
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
    flow: &crate::product_data_io::ProductDataIoFlow,
    resource_guards: &[echo_agent::tools::InvocationResourceGuard],
    workspace_root: &Path,
    request: ScholarlySearchRequest,
) -> ResearchResult<ScholarlyIngestResult> {
    search_and_ingest_inner(
        workspace_root,
        request,
        ResearchIo::Application {
            flow,
            resource_guards,
        },
    )
    .await
}

pub async fn search_and_ingest_scoped(
    product_data: &crate::product_data_io::ScopedProductData,
    request: ScholarlySearchRequest,
) -> ResearchResult<ScholarlyIngestResult> {
    let flow = product_data
        .begin_owned_flow("search and ingest research sources")
        .map_err(|error| ResearchError::External(error.to_string()))?;
    let result =
        search_and_ingest_inner(product_data.data_root(), request, ResearchIo::Scoped(&flow)).await;
    let failure = result
        .as_ref()
        .err()
        .filter(|error| error.is_durable_settlement_debt())
        .map(ToString::to_string);
    flow.settle(failure);
    result
}

async fn search_and_ingest_inner(
    workspace_root: &Path,
    request: ScholarlySearchRequest,
    product_data: ResearchIo<'_>,
) -> ResearchResult<ScholarlyIngestResult> {
    let query = request.query.trim().to_string();
    if query.is_empty() {
        return Err(ResearchError::Invalid(
            "scholarly search query cannot be empty".to_string(),
        ));
    }
    let limit = request.limit.unwrap_or(20).clamp(1, 100);
    let page = match request.provider {
        ResearchProvider::Openalex => OpenAlexClient::new(request.mailto)
            .map_err(external)?
            .search(&query, limit)
            .await
            .map_err(external)?,
        ResearchProvider::Crossref => CrossrefClient::new(request.mailto)
            .map_err(external)?
            .search(&query, limit)
            .await
            .map_err(external)?,
        ResearchProvider::EuropePmc => EuropePmcClient::new()
            .map_err(external)?
            .search(&query, limit)
            .await
            .map_err(external)?,
    };
    let root = workspace_root.to_path_buf();
    research_io(product_data, "ingest scholarly search page", move || {
        ingest_page(&root, request.provider, &query, page)
    })
    .await
}

pub async fn import_zotero(
    flow: &crate::product_data_io::ProductDataIoFlow,
    resource_guards: &[echo_agent::tools::InvocationResourceGuard],
    workspace_root: &Path,
    request: ZoteroSyncRequest,
) -> ResearchResult<ZoteroSyncResult> {
    import_zotero_inner(
        workspace_root,
        request,
        ResearchIo::Application {
            flow,
            resource_guards,
        },
    )
    .await
}

pub async fn import_zotero_scoped(
    product_data: &crate::product_data_io::ScopedProductData,
    request: ZoteroSyncRequest,
) -> ResearchResult<ZoteroSyncResult> {
    let flow = product_data
        .begin_owned_flow("import Zotero research library")
        .map_err(|error| ResearchError::External(error.to_string()))?;
    let result =
        import_zotero_inner(product_data.data_root(), request, ResearchIo::Scoped(&flow)).await;
    let failure = result
        .as_ref()
        .err()
        .filter(|error| error.is_durable_settlement_debt())
        .map(ToString::to_string);
    flow.settle(failure);
    result
}

async fn import_zotero_inner(
    workspace_root: &Path,
    request: ZoteroSyncRequest,
    product_data: ResearchIo<'_>,
) -> ResearchResult<ZoteroSyncResult> {
    let client = zotero_client(&request)?;
    let items = client
        .list_items(request.limit.unwrap_or(1_000).clamp(1, 10_000))
        .await
        .map_err(external)?;
    let root = workspace_root.to_path_buf();
    research_io(product_data, "ingest Zotero library", move || {
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
            let mut source_request =
                source_request_from_work(work, None, Some("zotero".to_string()));
            source_request.source_kind = Some(source_kind);
            let result = ingest_source(&root, source_request)?;
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
    })
    .await
}

pub async fn export_zotero(
    flow: &crate::product_data_io::ProductDataIoFlow,
    resource_guards: &[echo_agent::tools::InvocationResourceGuard],
    workspace_root: &Path,
    request: ZoteroSyncRequest,
) -> ResearchResult<ZoteroSyncResult> {
    export_zotero_inner(
        workspace_root,
        request,
        ResearchIo::Application {
            flow,
            resource_guards,
        },
    )
    .await
}

pub async fn export_zotero_scoped(
    product_data: &crate::product_data_io::ScopedProductData,
    request: ZoteroSyncRequest,
) -> ResearchResult<ZoteroSyncResult> {
    let flow = product_data
        .begin_owned_flow("export Zotero research library")
        .map_err(|error| ResearchError::External(error.to_string()))?;
    let result =
        export_zotero_inner(product_data.data_root(), request, ResearchIo::Scoped(&flow)).await;
    let failure = result
        .as_ref()
        .err()
        .filter(|error| error.is_durable_settlement_debt())
        .map(ToString::to_string);
    flow.settle(failure);
    result
}

async fn export_zotero_inner(
    workspace_root: &Path,
    request: ZoteroSyncRequest,
    product_data: ResearchIo<'_>,
) -> ResearchResult<ZoteroSyncResult> {
    if request.source_ids.is_empty() {
        return Err(ResearchError::Invalid(
            "Zotero export requires at least one source ID".to_string(),
        ));
    }
    let root = workspace_root.to_path_buf();
    let source_ids = request.source_ids.clone();
    let sources = research_io(product_data, "load Zotero export sources", move || {
        let mut sources = Vec::new();
        for source_id in &source_ids {
            sources.push(get_source(&root, source_id)?);
        }
        Ok(sources)
    })
    .await?;
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
    flow: &crate::product_data_io::ProductDataIoFlow,
    resource_guards: &[echo_agent::tools::InvocationResourceGuard],
    workspace_root: &Path,
    source_id: &str,
) -> ResearchResult<EuropePmcEnrichmentResult> {
    enrich_from_europe_pmc_inner(
        workspace_root,
        source_id,
        ResearchIo::Application {
            flow,
            resource_guards,
        },
    )
    .await
}

pub async fn enrich_from_europe_pmc_scoped(
    product_data: &crate::product_data_io::ScopedProductData,
    source_id: &str,
) -> ResearchResult<EuropePmcEnrichmentResult> {
    let flow = product_data
        .begin_owned_flow("enrich research source from Europe PMC")
        .map_err(|error| ResearchError::External(error.to_string()))?;
    let result = enrich_from_europe_pmc_inner(
        product_data.data_root(),
        source_id,
        ResearchIo::Scoped(&flow),
    )
    .await;
    let failure = result
        .as_ref()
        .err()
        .filter(|error| error.is_durable_settlement_debt())
        .map(ToString::to_string);
    flow.settle(failure);
    result
}

async fn enrich_from_europe_pmc_inner(
    workspace_root: &Path,
    source_id: &str,
    product_data: ResearchIo<'_>,
) -> ResearchResult<EuropePmcEnrichmentResult> {
    let root = workspace_root.to_path_buf();
    let id = source_id.to_string();
    let source = research_io(product_data, "load Europe PMC source", move || {
        get_source(&root, &id)
    })
    .await?;
    let (provider_source, provider_id) = if let Some(pmcid) = source.pmcid.clone() {
        ("PMC", pmcid)
    } else if let Some(pmid) = source.pmid.clone() {
        ("MED", pmid)
    } else {
        return Err(ResearchError::Invalid(
            "Europe PMC enrichment requires a PMID or PMCID".to_string(),
        ));
    };
    let client = EuropePmcClient::new().map_err(external)?;
    let mut warnings = Vec::new();
    let citations = match client.citations(provider_source, &provider_id).await {
        Ok(items) => Some(items.into_iter().map(|item| item.id).collect()),
        Err(error) => {
            warnings.push(format!("citations: {error}"));
            None
        }
    };
    let references = match client.references(provider_source, &provider_id).await {
        Ok(items) => Some(items.into_iter().map(|item| item.id).collect()),
        Err(error) => {
            warnings.push(format!("references: {error}"));
            None
        }
    };
    let entities = match client.text_mined_terms(provider_source, &provider_id).await {
        Ok(items) => Some(
            items
                .into_iter()
                .map(|item| BiomedicalEntity {
                    name: item.name,
                    semantic_type: item.semantic_type,
                    frequency: item.frequency,
                })
                .collect(),
        ),
        Err(error) => {
            warnings.push(format!("text-mined terms: {error}"));
            None
        }
    };
    let supports_full_text = source.pmcid.is_some();
    let full_text_path = if let Some(pmcid) = source.pmcid.clone() {
        match client.full_text_xml(&pmcid).await {
            Ok(xml) => {
                let root = workspace_root.to_path_buf();
                let id = source_id.to_string();
                match research_io(product_data, "write Europe PMC full text", move || {
                    write_full_text_xml(&root, &id, &xml)
                })
                .await
                {
                    Ok(path) => Some(Some(path)),
                    Err(error) => {
                        warnings.push(format!("full text: {error}"));
                        None
                    }
                }
            }
            Err(error) => {
                warnings.push(format!("full text: {error}"));
                None
            }
        }
    } else {
        None
    };
    let successful_fields = enrichment_fields(
        citations.is_some(),
        references.is_some(),
        entities.is_some(),
        supports_full_text && full_text_path.is_some(),
    );
    let failed_fields = enrichment_fields(
        citations.is_none(),
        references.is_none(),
        entities.is_none(),
        supports_full_text && full_text_path.is_none(),
    );
    let root = workspace_root.to_path_buf();
    let id = source_id.to_string();
    let source = research_io(product_data, "save Europe PMC supplement", move || {
        save_europe_pmc_supplement(
            &root,
            &id,
            EuropePmcSupplementUpdate {
                citation_ids: citations,
                reference_ids: references,
                biomedical_entities: entities,
                full_text_path,
                attempt: Some(EuropePmcEnrichmentAttempt {
                    attempt_id: uuid::Uuid::new_v4().to_string(),
                    provider: "europe_pmc".to_string(),
                    attempted_at: Utc::now(),
                    successful_fields,
                    failed_fields,
                }),
            },
        )
    })
    .await?;
    Ok(EuropePmcEnrichmentResult { source, warnings })
}

#[derive(Clone, Copy)]
enum ResearchIo<'a> {
    Scoped(&'a crate::product_data_io::ScopedProductDataFlow),
    Application {
        flow: &'a crate::product_data_io::ProductDataIoFlow,
        resource_guards: &'a [echo_agent::tools::InvocationResourceGuard],
    },
}

async fn research_io<T, F>(
    product_data: ResearchIo<'_>,
    operation: &'static str,
    function: F,
) -> ResearchResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ResearchResult<T> + Send + 'static,
{
    match product_data {
        ResearchIo::Scoped(product_data) => product_data
            .run(operation, function)
            .await
            .map_err(|error| ResearchError::External(error.to_string()))?,
        ResearchIo::Application {
            flow,
            resource_guards,
        } => {
            let resource_guards = resource_guards.to_vec();
            flow.run(operation, move || {
                let _resource_guards = resource_guards;
                function()
            })
            .await
            .map_err(|error| ResearchError::External(error.to_string()))?
        }
    }
}

pub fn ingest_tool_output(
    workspace_root: &Path,
    tool: &str,
    output: &str,
) -> ResearchResult<Vec<SourceIngestResult>> {
    ingest_tool_output_with_status(workspace_root, tool, output).map_err(|failure| failure.error)
}

struct IngestFailure {
    error: ResearchError,
    persisted_count: usize,
}

fn ingest_tool_output_with_status(
    workspace_root: &Path,
    tool: &str,
    output: &str,
) -> Result<Vec<SourceIngestResult>, IngestFailure> {
    if !AUTO_INGEST_TOOLS.contains(&tool) {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(output).map_err(|error| IngestFailure {
        error: error.into(),
        persisted_count: 0,
    })?;
    let query = value
        .get("query")
        .and_then(Value::as_str)
        .map(str::to_string);
    let records = value
        .get("papers")
        .or_else(|| value.get("studies"))
        .and_then(Value::as_array)
        .ok_or_else(|| IngestFailure {
            error: ResearchError::Invalid(
                "research provider output must contain a papers or studies array".to_string(),
            ),
            persisted_count: 0,
        })?;
    let mut ingested = Vec::new();
    for request in records
        .iter()
        .filter_map(|record| source_request_from_tool_record(tool, query.as_deref(), record))
    {
        match ingest_source(workspace_root, request) {
            Ok(record) => ingested.push(record),
            Err(error) if ingested.is_empty() => {
                return Err(IngestFailure {
                    error,
                    persisted_count: 0,
                });
            }
            Err(error) => {
                return Err(IngestFailure {
                    error,
                    persisted_count: ingested.len(),
                });
            }
        }
    }
    Ok(ingested)
}

struct AutoIngestResearchTool {
    inner: Box<dyn Tool>,
    workspace_io_identity: crate::workspace::WorkspaceIoIdentity,
    product_data_io: crate::product_data_io::ProductDataIoService,
    #[cfg(test)]
    barrier: std::sync::Mutex<Option<AutoIngestTestBarrier>>,
}

impl AutoIngestResearchTool {
    fn new(
        inner: Box<dyn Tool>,
        workspace_io_identity: crate::workspace::WorkspaceIoIdentity,
        product_data_io: crate::product_data_io::ProductDataIoService,
    ) -> Self {
        Self {
            inner,
            workspace_io_identity,
            product_data_io,
            #[cfg(test)]
            barrier: std::sync::Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn with_barrier(
        inner: Box<dyn Tool>,
        workspace_io_identity: crate::workspace::WorkspaceIoIdentity,
        product_data_io: crate::product_data_io::ProductDataIoService,
        barrier: AutoIngestTestBarrier,
    ) -> Self {
        Self {
            inner,
            workspace_io_identity,
            product_data_io,
            barrier: std::sync::Mutex::new(Some(barrier)),
        }
    }
}

#[cfg(test)]
struct AutoIngestTestBarrier {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

fn apply_auto_ingest(workspace_root: &Path, tool_name: &str, mut result: ToolResult) -> ToolResult {
    if !result.success {
        return result;
    }
    match ingest_tool_output_with_status(workspace_root, tool_name, &result.output) {
        Ok(records) => {
            let created = records.iter().filter(|record| record.created).count();
            let status = if records.is_empty() {
                "no_records"
            } else {
                "persisted"
            };
            result
                .metadata
                .insert("provider_call".to_string(), "completed".to_string());
            result.metadata.insert(
                "research_persistence_status".to_string(),
                status.to_string(),
            );
            result.metadata.insert(
                "research_persisted_count".to_string(),
                records.len().to_string(),
            );
            if !records.is_empty() {
                tracing::info!(
                    tool = tool_name,
                    created,
                    total = records.len(),
                    "research results ingested"
                );
            }
            result
        }
        Err(failure) => {
            let IngestFailure {
                error,
                persisted_count,
            } = failure;
            tracing::warn!(tool = tool_name, %error, "research result ingestion failed");
            let persistence_status = if persisted_count > 0 {
                "partial"
            } else {
                "failed"
            };
            let mut failed = ToolResult::failure(
                ToolFailureCategory::PartialSideEffect,
                format!("{tool_name} completed, but EKO research persistence failed: {error}"),
            )
            .with_output(result.output);
            failed.metadata = result.metadata;
            failed
                .metadata
                .insert("provider_call".to_string(), "completed".to_string());
            failed.metadata.insert(
                "research_retrieval_status".to_string(),
                "succeeded".to_string(),
            );
            failed.metadata.insert(
                "research_persistence_status".to_string(),
                persistence_status.to_string(),
            );
            failed.metadata.insert(
                "research_persisted_count".to_string(),
                persisted_count.to_string(),
            );
            failed
        }
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
            // Execution working_dir may be a writer worktree. Product-data
            // root comes only from this workspace-local tool descriptor.
            let Some(workspace_io) =
                crate::state::WorkspaceIoInvocation::from_tool_context_for_identity(
                    context,
                    &self.workspace_io_identity,
                )
            else {
                return Ok(auto_ingest_preflight_failure(
                    self.name(),
                    "the invocation did not retain an EKO workspace lifetime receipt",
                ));
            };
            let flow = self
                .product_data_io
                .begin_owned_flow("automatic research ingest")
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
            let result = match self.inner.execute_with_context(parameters, context).await {
                Ok(result) => result,
                Err(error) => {
                    flow.settle(None);
                    return Err(error);
                }
            };
            let workspace_root = workspace_io.data_root().to_path_buf();
            let resource_guards = workspace_io.resource_guards();
            let tool_name = self.name().to_string();
            #[cfg(test)]
            let barrier = self
                .barrier
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            let persisted = flow
                .run("persist automatic research ingest", move || {
                    let _resource_guards = resource_guards;
                    #[cfg(test)]
                    if let Some(barrier) = barrier {
                        let _ = barrier.entered.send(());
                        let _ = barrier.release.blocking_recv();
                    }
                    apply_auto_ingest(&workspace_root, &tool_name, result)
                })
                .await;
            match persisted {
                Ok(result) => {
                    let failure = (!result.success).then(|| {
                        result
                            .error
                            .clone()
                            .unwrap_or_else(|| "automatic research persistence failed".to_string())
                    });
                    flow.settle(failure);
                    Ok(result)
                }
                Err(error) => {
                    let detail = error.to_string();
                    flow.settle(Some(detail.clone()));
                    Err(echo_agent::error::ReactError::Other(detail))
                }
            }
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

fn auto_ingest_preflight_failure(tool_name: &str, detail: &str) -> ToolResult {
    let mut failed = ToolResult::failure(
        ToolFailureCategory::InvalidArguments,
        format!("{tool_name} was refused before provider execution: {detail}"),
    );
    failed
        .metadata
        .insert("provider_call".to_string(), "not_started".to_string());
    failed.metadata.insert(
        "research_retrieval_status".to_string(),
        "not_started".to_string(),
    );
    failed.metadata.insert(
        "research_persistence_status".to_string(),
        "refused".to_string(),
    );
    failed
        .metadata
        .insert("research_persisted_count".to_string(), "0".to_string());
    failed
}

fn enrichment_fields(
    citations: bool,
    references: bool,
    biomedical_entities: bool,
    full_text_path: bool,
) -> Vec<String> {
    [
        (citations, "citation_ids"),
        (references, "reference_ids"),
        (biomedical_entities, "biomedical_entities"),
        (full_text_path, "full_text_path"),
    ]
    .into_iter()
    .filter(|(included, _)| *included)
    .map(|(_, field)| field.to_string())
    .collect()
}

pub(crate) fn install_auto_ingest_tools(
    agent: &mut ReactAgent,
    workspace_io_identity: crate::workspace::WorkspaceIoIdentity,
    product_data_io: crate::product_data_io::ProductDataIoService,
) {
    for tool_name in AUTO_INGEST_TOOLS {
        if let Some(tool) = agent.remove_tool(tool_name) {
            agent.add_tool(Box::new(AutoIngestResearchTool::new(
                tool,
                workspace_io_identity.clone(),
                product_data_io.clone(),
            )));
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
struct AutoIngestBarrierFixtureTool;

#[cfg(test)]
impl Tool for AutoIngestBarrierFixtureTool {
    fn name(&self) -> &str {
        "semantic_scholar_search"
    }

    fn description(&self) -> &str {
        "auto-ingest lifetime barrier fixture"
    }

    fn parameters(&self) -> Value {
        serde_json::json!({"type": "object"})
    }

    fn execute_with_context<'a>(
        &'a self,
        _parameters: ToolParameters,
        _context: &'a ToolContext,
    ) -> BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            Ok(ToolResult::success(
                serde_json::json!({
                    "query": "lifetime barrier",
                    "papers": [{
                        "title": "Auto-ingest lifetime barrier",
                        "doi": "10.1/auto-ingest-lifetime"
                    }]
                })
                .to_string(),
            ))
        })
    }
}

#[cfg(test)]
pub(crate) async fn run_auto_ingest_barrier_fixture(
    context: ToolContext,
    workspace_io_identity: crate::workspace::WorkspaceIoIdentity,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
) -> echo_agent::error::Result<ToolResult> {
    AutoIngestResearchTool::with_barrier(
        Box::new(AutoIngestBarrierFixtureTool),
        workspace_io_identity,
        crate::product_data_io::ProductDataIoService::new(),
        AutoIngestTestBarrier { entered, release },
    )
    .execute_with_context(ToolParameters::new(), &context)
    .await
}

#[cfg(test)]
pub(crate) fn install_auto_ingest_barrier_fixture(
    agent: &mut ReactAgent,
    workspace_io_identity: crate::workspace::WorkspaceIoIdentity,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
) {
    agent.add_tool(Box::new(AutoIngestResearchTool::with_barrier(
        Box::new(AutoIngestBarrierFixtureTool),
        workspace_io_identity,
        crate::product_data_io::ProductDataIoService::new(),
        AutoIngestTestBarrier { entered, release },
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::error::Result as AgentResult;

    fn guarded_context(root: &Path) -> ToolContext {
        let receipt = crate::state::ScopedWorkspaceIoReceipt::global_for_test(root);
        ToolContext {
            working_dir: Some(root.to_path_buf()),
            resource_guards: receipt.invocation().resource_guards(),
            ..ToolContext::default()
        }
    }

    #[test]
    fn workspace_io_scope_keeps_only_exact_eko_receipt_guards() -> AgentResult<()> {
        let workspace = tempfile::tempdir()?;
        let mut context = guarded_context(workspace.path());
        context.resource_guards.insert(
            0,
            echo_agent::tools::InvocationResourceGuard::new("unrelated-lease".to_string()),
        );

        let identity = crate::workspace::WorkspaceIoIdentity::global(workspace.path());
        let scope = crate::state::WorkspaceIoInvocation::from_tool_context_for_identity(
            &context, &identity,
        )
        .ok_or_else(|| {
            echo_agent::error::ReactError::Other("typed workspace receipt was lost".to_string())
        })?;
        let filtered = scope.resource_guards();

        assert_eq!(filtered.len(), 1);
        assert!(filtered.first().is_some_and(
            echo_agent::tools::InvocationResourceGuard::retains::<
                crate::state::ScopedWorkspaceIoReceipt,
            >
        ));
        Ok(())
    }

    struct SuccessfulResearchTool {
        output: String,
    }

    impl Tool for SuccessfulResearchTool {
        fn name(&self) -> &str {
            "semantic_scholar_search"
        }

        fn description(&self) -> &str {
            "test research provider"
        }

        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        fn execute_with_context<'a>(
            &'a self,
            _parameters: ToolParameters,
            _context: &'a ToolContext,
        ) -> BoxFuture<'a, AgentResult<ToolResult>> {
            let output = self.output.clone();
            Box::pin(async move { Ok(ToolResult::success(output)) })
        }
    }

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
    async fn auto_ingest_reports_malformed_provider_output_as_failure() -> AgentResult<()> {
        let workspace = tempfile::tempdir()?;
        let tool = AutoIngestResearchTool::new(
            Box::new(SuccessfulResearchTool {
                output: "not json".to_string(),
            }),
            crate::workspace::WorkspaceIoIdentity::global(workspace.path()),
            crate::product_data_io::ProductDataIoService::new(),
        );
        let context = guarded_context(workspace.path());

        let result = tool
            .execute_with_context(ToolParameters::new(), &context)
            .await?;

        assert!(!result.success);
        assert_eq!(
            result
                .metadata
                .get("research_retrieval_status")
                .map(String::as_str),
            Some("succeeded")
        );
        assert_eq!(
            result
                .metadata
                .get("research_persistence_status")
                .map(String::as_str),
            Some("failed")
        );
        assert_eq!(
            result.metadata.get("provider_call").map(String::as_str),
            Some("completed")
        );
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.category),
            Some(ToolFailureCategory::PartialSideEffect)
        );
        assert_eq!(result.output, "not json");
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("persistence failed"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn auto_ingest_reports_local_persistence_failure() -> AgentResult<()> {
        let workspace = tempfile::tempdir()?;
        std::fs::write(
            workspace.path().join("research"),
            "blocks directory creation",
        )?;
        let tool = AutoIngestResearchTool::new(
            Box::new(SuccessfulResearchTool {
                output: serde_json::json!({
                    "query": "test",
                    "papers": [{"title": "Cannot persist", "doi": "10.1/failure"}]
                })
                .to_string(),
            }),
            crate::workspace::WorkspaceIoIdentity::global(workspace.path()),
            crate::product_data_io::ProductDataIoService::new(),
        );
        let context = guarded_context(workspace.path());

        let result = tool
            .execute_with_context(ToolParameters::new(), &context)
            .await?;

        assert!(!result.success);
        assert_eq!(
            result
                .metadata
                .get("research_persistence_status")
                .map(String::as_str),
            Some("failed")
        );
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.category),
            Some(ToolFailureCategory::PartialSideEffect)
        );
        assert!(result.output.contains("Cannot persist"));
        Ok(())
    }

    #[tokio::test]
    async fn auto_ingest_reports_partial_persistence_after_first_record() -> AgentResult<()> {
        let workspace = tempfile::tempdir()?;
        let oversized_title = "x".repeat(4 * 1024 * 1024);
        let tool = AutoIngestResearchTool::new(
            Box::new(SuccessfulResearchTool {
                output: serde_json::json!({
                    "query": "partial",
                    "papers": [
                        {"title": "Persisted first", "doi": "10.1/first"},
                        {"title": oversized_title, "doi": "10.1/too-large"}
                    ]
                })
                .to_string(),
            }),
            crate::workspace::WorkspaceIoIdentity::global(workspace.path()),
            crate::product_data_io::ProductDataIoService::new(),
        );
        let context = guarded_context(workspace.path());

        let result = tool
            .execute_with_context(ToolParameters::new(), &context)
            .await?;

        assert!(!result.success);
        assert_eq!(
            result
                .metadata
                .get("research_persistence_status")
                .map(String::as_str),
            Some("partial")
        );
        assert_eq!(
            result
                .metadata
                .get("research_persisted_count")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            crate::research::list_sources(workspace.path(), None, None)
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn auto_ingest_with_only_unrelated_guard_fails_closed_without_writing() -> AgentResult<()>
    {
        let workspace = tempfile::tempdir()?;
        let tool = AutoIngestResearchTool::new(
            Box::new(SuccessfulResearchTool {
                output: serde_json::json!({
                    "query": "unguarded",
                    "papers": [{"title": "Must not persist", "doi": "10.1/unguarded"}]
                })
                .to_string(),
            }),
            crate::workspace::WorkspaceIoIdentity::global(workspace.path()),
            crate::product_data_io::ProductDataIoService::new(),
        );
        let context = ToolContext {
            working_dir: Some(workspace.path().to_path_buf()),
            resource_guards: vec![echo_agent::tools::InvocationResourceGuard::new(
                "unrelated-lease".to_string(),
            )],
            ..ToolContext::default()
        };

        let result = tool
            .execute_with_context(ToolParameters::new(), &context)
            .await?;

        assert!(!result.success);
        assert_eq!(
            result
                .metadata
                .get("research_persistence_status")
                .map(String::as_str),
            Some("refused")
        );
        assert_eq!(
            result.metadata.get("provider_call").map(String::as_str),
            Some("not_started")
        );
        assert_eq!(
            result
                .metadata
                .get("research_persisted_count")
                .map(String::as_str),
            Some("0")
        );
        assert!(!workspace.path().join("research").exists());
        Ok(())
    }

    #[tokio::test]
    async fn auto_ingest_uses_product_root_when_writer_working_dir_is_isolated() -> AgentResult<()>
    {
        let workspace = tempfile::tempdir()?;
        let writer = tempfile::tempdir()?;
        let tool = AutoIngestResearchTool::new(
            Box::new(SuccessfulResearchTool {
                output: serde_json::json!({
                    "query": "writer isolation",
                    "papers": [{"title": "Product root", "doi": "10.1/product-root"}]
                })
                .to_string(),
            }),
            crate::workspace::WorkspaceIoIdentity::global(workspace.path()),
            crate::product_data_io::ProductDataIoService::new(),
        );
        let mut context = guarded_context(workspace.path());
        context.working_dir = Some(writer.path().to_path_buf());

        let result = tool
            .execute_with_context(ToolParameters::new(), &context)
            .await?;

        assert!(result.success);
        assert_eq!(
            crate::research::list_sources(workspace.path(), None, None)
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
                .len(),
            1
        );
        assert!(!writer.path().join("research").exists());
        Ok(())
    }

    #[tokio::test]
    async fn preaccepted_auto_ingest_completes_after_phase_one_seal() -> AgentResult<()> {
        let workspace = tempfile::tempdir()?;
        let product_data_io = crate::product_data_io::ProductDataIoService::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let tool = AutoIngestResearchTool::with_barrier(
            Box::new(AutoIngestBarrierFixtureTool),
            crate::workspace::WorkspaceIoIdentity::global(workspace.path()),
            product_data_io.clone(),
            AutoIngestTestBarrier {
                entered: entered_tx,
                release: release_rx,
            },
        );
        let context = guarded_context(workspace.path());
        let operation = tokio::spawn(async move {
            tool.execute_with_context(ToolParameters::new(), &context)
                .await
        });
        entered_rx
            .await
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
        product_data_io
            .begin_shutdown()
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
        let shutdown_service = product_data_io.clone();
        let shutdown = tokio::spawn(async move { shutdown_service.join_shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        release_tx.send(()).map_err(|_| {
            echo_agent::error::ReactError::Other("auto-ingest release receiver closed".to_string())
        })?;
        let result = operation
            .await
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))??;
        assert!(result.success);
        shutdown
            .await
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
            .map_err(echo_agent::error::ReactError::Other)?;
        Ok(())
    }

    #[tokio::test]
    async fn auto_ingest_rejects_wrong_or_ambiguous_workspace_identity() -> AgentResult<()> {
        let workspace = tempfile::tempdir()?;
        let other = tempfile::tempdir()?;
        let output = serde_json::json!({
            "query": "identity mismatch",
            "papers": [{"title": "Must not persist", "doi": "10.1/identity-mismatch"}]
        })
        .to_string();
        let expected_identity = crate::workspace::WorkspaceIoIdentity::global(workspace.path());
        let wrong_tool = AutoIngestResearchTool::new(
            Box::new(SuccessfulResearchTool {
                output: output.clone(),
            }),
            expected_identity.clone(),
            crate::product_data_io::ProductDataIoService::new(),
        );
        let wrong_context = guarded_context(other.path());
        let wrong = wrong_tool
            .execute_with_context(ToolParameters::new(), &wrong_context)
            .await?;
        assert!(!wrong.success);

        let ambiguous_tool = AutoIngestResearchTool::new(
            Box::new(SuccessfulResearchTool { output }),
            expected_identity,
            crate::product_data_io::ProductDataIoService::new(),
        );
        let mut ambiguous_context = guarded_context(workspace.path());
        let duplicate = ambiguous_context
            .resource_guards
            .first()
            .cloned()
            .ok_or_else(|| {
                echo_agent::error::ReactError::Other(
                    "workspace guard fixture was empty".to_string(),
                )
            })?;
        ambiguous_context.resource_guards.push(duplicate);
        let ambiguous = ambiguous_tool
            .execute_with_context(ToolParameters::new(), &ambiguous_context)
            .await?;
        assert!(!ambiguous.success);
        assert!(!workspace.path().join("research").exists());
        assert!(!other.path().join("research").exists());
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
        let product_data_io = crate::product_data_io::ProductDataIoService::new();
        let flow = product_data_io
            .begin_owned_flow("live research provider smoke")
            .map_err(|error| ResearchError::External(error.to_string()))?;
        for provider in [
            ResearchProvider::Openalex,
            ResearchProvider::Crossref,
            ResearchProvider::EuropePmc,
        ] {
            let result = search_and_ingest(
                &flow,
                &[],
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
        flow.settle(None);
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
        let product_data_io = crate::product_data_io::ProductDataIoService::new();
        let flow = product_data_io
            .begin_owned_flow("live Zotero smoke")
            .map_err(|error| ResearchError::External(error.to_string()))?;
        let result = import_zotero(
            &flow,
            &[],
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
        flow.settle(None);
        Ok(())
    }
}
