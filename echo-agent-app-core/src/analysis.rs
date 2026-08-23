//! File-backed, coding-first data analysis workspaces.
//!
//! An analysis is ordinary workspace content under `analysis/<id>/`: a
//! reviewable Python/R script, a small manifest, output files, and immutable
//! per-run records. Execution delegates to the framework `run_code` tool so
//! sandbox, timeout, cancellation, and output limits stay on one path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use echo_agent::agent::AgentHandle;
use echo_agent::tools::{ToolContext, ToolResultKind};
use echo_agent::tools::{ToolFailureCategory, ToolManager, ToolParameters};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const ANALYSIS_ROOT: &str = "analysis";
const MANIFEST_FILE: &str = "manifest.json";
const LATEST_RUN_FILE: &str = "latest-run.json";
const RUNS_DIR: &str = "runs";
const CONTRACT_VERSION: u32 = 1;
const MAX_SCRIPT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_JSON_BYTES: u64 = 2 * 1024 * 1024;
const MAX_OUTPUT_FILES: usize = 200;
const MAX_CAPTURE_CHARS: usize = 200_000;

#[derive(Debug, Error)]
pub enum AnalysisError {
    #[error("invalid analysis input: {0}")]
    Invalid(String),
    #[error("analysis not found: {0}")]
    NotFound(String),
    #[error("analysis changed on disk; reload before saving")]
    Conflict,
    #[error("analysis I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("analysis JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("analysis execution failed: {0}")]
    Execution(String),
    #[error("analysis runtime is unavailable: {0}")]
    RuntimeUnavailable(String),
}

pub type AnalysisResult<T> = Result<T, AnalysisError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisLanguage {
    Python,
    R,
}

impl AnalysisLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::R => "r",
        }
    }

    fn script_name(self) -> &'static str {
        match self {
            Self::Python => "analysis.py",
            Self::R => "analysis.R",
        }
    }

    fn starter_script(self) -> &'static str {
        match self {
            Self::Python => {
                r#"from pathlib import Path
import json
import platform
import random

OUTPUT_DIR = Path("outputs")
OUTPUT_DIR.mkdir(exist_ok=True)

manifest = json.loads(Path("manifest.json").read_text(encoding="utf-8"))
if manifest.get("random_seed") is not None:
    random.seed(manifest["random_seed"])

environment = {
    "python": platform.python_version(),
}
Path("environment.json").write_text(
    json.dumps(environment, indent=2), encoding="utf-8"
)

result = {
    "status": "ok",
    "message": "Replace this starter analysis with reviewable code.",
    "parameters": manifest.get("parameters", {}),
}
Path("result.json").write_text(
    json.dumps(result, indent=2), encoding="utf-8"
)
print(json.dumps(result))
"#
            }
            Self::R => {
                r#"dir.create("outputs", showWarnings = FALSE)

writeLines(
  sprintf('{\n  "r": "%s"\n}', paste(R.version$major, R.version$minor, sep = ".")),
  "environment.json"
)

writeLines(
  '{\n  "status": "ok",\n  "message": "Replace this starter analysis with reviewable code."\n}',
  "result.json"
)
cat('{"status":"ok","message":"Replace this starter analysis with reviewable code."}')
"#
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisManifest {
    pub contract_version: u32,
    pub analysis_id: String,
    pub title: String,
    pub language: AnalysisLanguage,
    pub script_path: String,
    #[serde(default)]
    pub input_paths: Vec<String>,
    #[serde(default)]
    pub parameters: Value,
    pub random_seed: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisFileFingerprint {
    pub path: String,
    pub available: bool,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisRunStatus {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisOutputArtifact {
    pub path: String,
    pub absolute_path: String,
    pub kind: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRunRecord {
    pub contract_version: u32,
    pub run_id: String,
    pub analysis_id: String,
    pub status: AnalysisRunStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub script: AnalysisFileFingerprint,
    pub inputs: Vec<AnalysisFileFingerprint>,
    pub parameters: Value,
    pub parameters_sha256: String,
    pub random_seed: Option<u64>,
    pub outputs: Vec<AnalysisOutputArtifact>,
    pub environment: BTreeMap<String, String>,
    pub exit_code: Option<i32>,
    pub sandbox_type: Option<String>,
    #[serde(default)]
    pub runtime_profile: Option<String>,
    pub output: String,
    pub error: Option<String>,
    pub output_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisSummary {
    pub analysis_id: String,
    pub title: String,
    pub language: AnalysisLanguage,
    pub script_path: String,
    pub updated_at: DateTime<Utc>,
    pub stale: bool,
    pub stale_reasons: Vec<String>,
    pub last_run_status: Option<AnalysisRunStatus>,
    pub last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisDocument {
    pub manifest: AnalysisManifest,
    pub script: String,
    pub script_revision: String,
    pub stale: bool,
    pub stale_reasons: Vec<String>,
    pub last_run: Option<AnalysisRunRecord>,
    pub outputs: Vec<AnalysisOutputArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveAnalysisRequest {
    pub title: String,
    pub script: String,
    pub expected_script_revision: String,
    #[serde(default)]
    pub input_paths: Vec<String>,
    #[serde(default)]
    pub parameters: Value,
    pub random_seed: Option<u64>,
}

pub async fn workspace_root_for_agent(agent: &AgentHandle) -> PathBuf {
    match agent.read(|agent| agent.working_dir()).await {
        Some(path) => path,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

pub fn list_analyses(workspace_root: &Path) -> AnalysisResult<Vec<AnalysisSummary>> {
    let root = workspace_root.join(ANALYSIS_ROOT);
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut summaries = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "skipping unreadable analysis entry");
                continue;
            }
        };
        if !entry.path().is_dir() {
            continue;
        }
        let Some(analysis_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        match load_analysis(workspace_root, &analysis_id) {
            Ok(document) => summaries.push(summary_from_document(&document)),
            Err(error) => {
                tracing::warn!(analysis_id, %error, "skipping invalid analysis workspace");
            }
        }
    }
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
    Ok(summaries)
}

pub fn create_analysis(
    workspace_root: &Path,
    title: &str,
    language: AnalysisLanguage,
) -> AnalysisResult<AnalysisDocument> {
    let title = title.trim();
    if title.is_empty() {
        return Err(AnalysisError::Invalid(
            "analysis title cannot be empty".to_string(),
        ));
    }

    let analysis_id = new_analysis_id(title);
    let analysis_dir = workspace_root.join(ANALYSIS_ROOT).join(&analysis_id);
    fs::create_dir_all(&analysis_dir)?;
    let now = Utc::now();
    let manifest = AnalysisManifest {
        contract_version: CONTRACT_VERSION,
        analysis_id: analysis_id.clone(),
        title: title.to_string(),
        language,
        script_path: language.script_name().to_string(),
        input_paths: Vec::new(),
        parameters: Value::Object(serde_json::Map::new()),
        random_seed: None,
        created_at: now,
        updated_at: now,
    };

    echo_agent::utils::fs::atomic_write(
        &analysis_dir.join(language.script_name()),
        language.starter_script().as_bytes(),
    )?;
    write_json(&analysis_dir.join(MANIFEST_FILE), &manifest)?;
    load_analysis(workspace_root, &analysis_id)
}

pub fn load_analysis(workspace_root: &Path, analysis_id: &str) -> AnalysisResult<AnalysisDocument> {
    validate_analysis_id(analysis_id)?;
    let analysis_dir = analysis_dir(workspace_root, analysis_id)?;
    let manifest: AnalysisManifest = read_json(&analysis_dir.join(MANIFEST_FILE))?;
    if manifest.contract_version != CONTRACT_VERSION || manifest.analysis_id != analysis_id {
        return Err(AnalysisError::Invalid(
            "analysis manifest identity or version is invalid".to_string(),
        ));
    }
    let script_path = safe_existing_relative_file(&analysis_dir, &manifest.script_path)?;
    let metadata = fs::metadata(&script_path)?;
    if metadata.len() > MAX_SCRIPT_BYTES {
        return Err(AnalysisError::Invalid(
            "analysis script exceeds 2 MiB".to_string(),
        ));
    }
    let script_bytes = fs::read(&script_path)?;
    let script = String::from_utf8(script_bytes.clone()).map_err(|error| {
        AnalysisError::Invalid(format!("analysis script is not valid UTF-8: {error}"))
    })?;
    let script_revision = hash_bytes(&script_bytes);
    let last_run = read_optional_json(&analysis_dir.join(LATEST_RUN_FILE))?;
    let (stale, stale_reasons) = stale_status(workspace_root, &manifest, last_run.as_ref())?;
    let outputs = last_run
        .as_ref()
        .map(|run: &AnalysisRunRecord| run.outputs.clone())
        .unwrap_or_default();
    Ok(AnalysisDocument {
        manifest,
        script,
        script_revision,
        stale,
        stale_reasons,
        last_run,
        outputs,
    })
}

pub fn save_analysis(
    workspace_root: &Path,
    analysis_id: &str,
    request: SaveAnalysisRequest,
) -> AnalysisResult<AnalysisDocument> {
    if request.script.len() as u64 > MAX_SCRIPT_BYTES {
        return Err(AnalysisError::Invalid(
            "analysis script exceeds 2 MiB".to_string(),
        ));
    }
    let mut document = load_analysis(workspace_root, analysis_id)?;
    if document.script_revision != request.expected_script_revision {
        return Err(AnalysisError::Conflict);
    }
    let title = request.title.trim();
    if title.is_empty() {
        return Err(AnalysisError::Invalid(
            "analysis title cannot be empty".to_string(),
        ));
    }
    let input_paths = normalized_input_paths(&request.input_paths)?;
    let analysis_dir = analysis_dir(workspace_root, analysis_id)?;
    let script_path = safe_existing_relative_file(&analysis_dir, &document.manifest.script_path)?;

    echo_agent::utils::fs::atomic_write(&script_path, request.script.as_bytes())?;
    document.manifest.title = title.to_string();
    document.manifest.input_paths = input_paths;
    document.manifest.parameters = request.parameters;
    document.manifest.random_seed = request.random_seed;
    document.manifest.updated_at = Utc::now();
    write_json(&analysis_dir.join(MANIFEST_FILE), &document.manifest)?;
    load_analysis(workspace_root, analysis_id)
}

pub async fn run_analysis_with_agent(
    agent: &AgentHandle,
    workspace_root: &Path,
    analysis_id: &str,
    cancel: Option<Arc<CancellationToken>>,
) -> AnalysisResult<AnalysisDocument> {
    let tool_manager = agent.read(|agent| agent.tool_manager().clone()).await;
    let document = load_analysis(workspace_root, analysis_id)?;
    let runtime = match document.manifest.language {
        AnalysisLanguage::Python => Some(
            crate::analysis_runtime::AnalyticsRuntime::default()
                .prepare_python()
                .await
                .map_err(|error| AnalysisError::RuntimeUnavailable(error.to_string()))?,
        ),
        AnalysisLanguage::R => None,
    };
    run_analysis_with_runtime(
        tool_manager.as_ref(),
        workspace_root,
        analysis_id,
        cancel,
        runtime.as_ref(),
    )
    .await
}

pub async fn run_analysis(
    tool_manager: &ToolManager,
    workspace_root: &Path,
    analysis_id: &str,
    cancel: Option<Arc<CancellationToken>>,
) -> AnalysisResult<AnalysisDocument> {
    run_analysis_with_runtime(tool_manager, workspace_root, analysis_id, cancel, None).await
}

async fn run_analysis_with_runtime(
    tool_manager: &ToolManager,
    workspace_root: &Path,
    analysis_id: &str,
    cancel: Option<Arc<CancellationToken>>,
    runtime: Option<&crate::analysis_runtime::PreparedAnalyticsRuntime>,
) -> AnalysisResult<AnalysisDocument> {
    let document = load_analysis(workspace_root, analysis_id)?;
    let analysis_dir = analysis_dir(workspace_root, analysis_id)?;
    let script = fingerprint_required(
        &analysis_dir.join(&document.manifest.script_path),
        &document.manifest.script_path,
    )?;
    let inputs = fingerprint_inputs(workspace_root, &document.manifest.input_paths)?;
    let parameters_sha256 = hash_json(&document.manifest.parameters)?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let started_at = Utc::now();
    let timer = Instant::now();
    clear_generated_outputs(&analysis_dir)?;
    let mut parameters = ToolParameters::new();
    parameters.insert(
        "language".to_string(),
        Value::String(document.manifest.language.as_str().to_string()),
    );
    parameters.insert(
        "script_path".to_string(),
        Value::String(document.manifest.script_path.clone()),
    );
    let context = ToolContext {
        working_dir: Some(analysis_dir.clone()),
        execution_id: Some(run_id.clone()),
        call_id: Some(format!("analysis:{run_id}")),
        script_execution_profile: runtime.map(|runtime| runtime.profile.clone()),
        cancel,
        ..ToolContext::default()
    };
    let execution = tool_manager
        .execute_tool_with_context("run_code", parameters, &context)
        .await;
    let finished_at = Utc::now();
    let duration_ms = u64::try_from(timer.elapsed().as_millis()).unwrap_or(u64::MAX);

    let (status, exit_code, sandbox_type, output, error, output_truncated) = match execution {
        Ok(result) => {
            let status = run_status(&result);
            let exit_code = match &result.kind {
                ToolResultKind::CommandOutput { exit_code } => *exit_code,
                _ => result
                    .metadata
                    .get("exit_code")
                    .and_then(|value| value.parse::<i32>().ok()),
            };
            (
                status,
                exit_code,
                result.metadata.get("sandbox_type").cloned(),
                bounded_text(&result.output, MAX_CAPTURE_CHARS),
                result.error.map(|value| bounded_text(&value, 8_000)),
                result.truncated,
            )
        }
        Err(error) => (
            AnalysisRunStatus::Failed,
            None,
            None,
            String::new(),
            Some(bounded_text(&error.to_string(), 8_000)),
            false,
        ),
    };

    let outputs = archive_outputs(workspace_root, &analysis_dir, &run_id)?;
    let mut environment = read_environment(&analysis_dir.join("environment.json"))?;
    if let Some(runtime) = runtime {
        environment.extend(runtime.environment.clone());
    }
    let record = AnalysisRunRecord {
        contract_version: CONTRACT_VERSION,
        run_id: run_id.clone(),
        analysis_id: analysis_id.to_string(),
        status,
        started_at,
        finished_at,
        duration_ms,
        script,
        inputs,
        parameters: document.manifest.parameters,
        parameters_sha256,
        random_seed: document.manifest.random_seed,
        outputs,
        environment,
        exit_code,
        sandbox_type,
        runtime_profile: runtime.map(|runtime| runtime.profile.id.clone()),
        output,
        error,
        output_truncated,
    };
    let runs_dir = analysis_dir.join(RUNS_DIR);
    fs::create_dir_all(&runs_dir)?;
    write_json(&runs_dir.join(format!("{run_id}.json")), &record)?;
    write_json(&analysis_dir.join(LATEST_RUN_FILE), &record)?;
    load_analysis(workspace_root, analysis_id)
}

pub fn format_analysis_list(summaries: &[AnalysisSummary]) -> String {
    if summaries.is_empty() {
        return "No file-backed analyses found.".to_string();
    }
    let mut output = String::from("Analyses:\n");
    for summary in summaries {
        let status = summary
            .last_run_status
            .map(|status| format!("{status:?}").to_ascii_lowercase())
            .unwrap_or_else(|| "not-run".to_string());
        let stale = if summary.stale { "stale" } else { "current" };
        output.push_str(&format!(
            "- {} [{}; {}] {} ({})\n",
            summary.analysis_id,
            summary.language.as_str(),
            status,
            summary.title,
            stale
        ));
    }
    output
}

pub fn format_analysis_document(document: &AnalysisDocument) -> String {
    let status = document
        .last_run
        .as_ref()
        .map(|run| format!("{:?}", run.status).to_ascii_lowercase())
        .unwrap_or_else(|| "not-run".to_string());
    let mut output = format!(
        "Analysis {}\nTitle: {}\nLanguage: {}\nScript: analysis/{}/{}\nStatus: {}\nStale: {}\n",
        document.manifest.analysis_id,
        document.manifest.title,
        document.manifest.language.as_str(),
        document.manifest.analysis_id,
        document.manifest.script_path,
        status,
        document.stale
    );
    if !document.stale_reasons.is_empty() {
        output.push_str("Stale reasons:\n");
        for reason in &document.stale_reasons {
            output.push_str(&format!("- {reason}\n"));
        }
    }
    if let Some(run) = &document.last_run {
        if !run.output.is_empty() {
            output.push_str("Output:\n");
            output.push_str(&bounded_text(&run.output, 4_000));
            output.push('\n');
        }
        if let Some(error) = &run.error {
            output.push_str(&format!("Error: {}\n", bounded_text(error, 2_000)));
        }
        if !run.outputs.is_empty() {
            output.push_str("Artifacts:\n");
            for artifact in &run.outputs {
                output.push_str(&format!(
                    "- {} ({}, {} bytes)\n",
                    artifact.path, artifact.kind, artifact.bytes
                ));
            }
        }
    }
    output
}

fn summary_from_document(document: &AnalysisDocument) -> AnalysisSummary {
    AnalysisSummary {
        analysis_id: document.manifest.analysis_id.clone(),
        title: document.manifest.title.clone(),
        language: document.manifest.language,
        script_path: document.manifest.script_path.clone(),
        updated_at: document.manifest.updated_at,
        stale: document.stale,
        stale_reasons: document.stale_reasons.clone(),
        last_run_status: document.last_run.as_ref().map(|run| run.status),
        last_run_at: document.last_run.as_ref().map(|run| run.finished_at),
    }
}

fn new_analysis_id(title: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;
    for character in title.chars() {
        if slug.chars().count() >= 32 {
            break;
        }
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator && !slug.is_empty() {
            slug.push('-');
            last_was_separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("analysis");
    }
    let suffix: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect();
    format!("{slug}-{suffix}")
}

fn validate_analysis_id(analysis_id: &str) -> AnalysisResult<()> {
    if analysis_id.is_empty()
        || !analysis_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return Err(AnalysisError::Invalid(
            "analysis id contains unsupported characters".to_string(),
        ));
    }
    Ok(())
}

fn analysis_dir(workspace_root: &Path, analysis_id: &str) -> AnalysisResult<PathBuf> {
    validate_analysis_id(analysis_id)?;
    let path = workspace_root.join(ANALYSIS_ROOT).join(analysis_id);
    if !path.is_dir() {
        return Err(AnalysisError::NotFound(analysis_id.to_string()));
    }
    Ok(path)
}

fn normalized_input_paths(paths: &[String]) -> AnalysisResult<Vec<String>> {
    let mut unique = BTreeSet::new();
    for raw in paths {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = Path::new(trimmed);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(AnalysisError::Invalid(format!(
                "input path must be workspace-relative: {trimmed}"
            )));
        }
        unique.insert(path.to_string_lossy().to_string());
    }
    Ok(unique.into_iter().collect())
}

fn safe_existing_relative_file(base: &Path, relative: &str) -> AnalysisResult<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AnalysisError::Invalid(
            "analysis file path escapes its directory".to_string(),
        ));
    }
    let canonical_base = base.canonicalize()?;
    let candidate = base.join(path);
    let canonical_candidate = candidate.canonicalize()?;
    if !canonical_candidate.starts_with(&canonical_base) || !canonical_candidate.is_file() {
        return Err(AnalysisError::Invalid(
            "analysis file must stay inside its directory".to_string(),
        ));
    }
    Ok(canonical_candidate)
}

fn stale_status(
    workspace_root: &Path,
    manifest: &AnalysisManifest,
    last_run: Option<&AnalysisRunRecord>,
) -> AnalysisResult<(bool, Vec<String>)> {
    let Some(last_run) = last_run else {
        return Ok((true, vec!["analysis has not been run".to_string()]));
    };
    let analysis_dir = analysis_dir(workspace_root, &manifest.analysis_id)?;
    let current_script = fingerprint_required(
        &analysis_dir.join(&manifest.script_path),
        &manifest.script_path,
    )?;
    let current_inputs = fingerprint_inputs(workspace_root, &manifest.input_paths)?;
    let mut reasons = Vec::new();
    if current_script.sha256 != last_run.script.sha256 {
        reasons.push("script changed since the last run".to_string());
    }
    if current_inputs.len() != last_run.inputs.len() {
        reasons.push("input list changed since the last run".to_string());
    } else {
        for current in &current_inputs {
            let previous = last_run
                .inputs
                .iter()
                .find(|item| item.path == current.path);
            if previous.is_none_or(|item| {
                item.available != current.available || item.sha256 != current.sha256
            }) {
                reasons.push(format!("input changed or is unavailable: {}", current.path));
            }
        }
    }
    if hash_json(&manifest.parameters)? != last_run.parameters_sha256 {
        reasons.push("parameters changed since the last run".to_string());
    }
    if manifest.random_seed != last_run.random_seed {
        reasons.push("random seed changed since the last run".to_string());
    }
    Ok((!reasons.is_empty(), reasons))
}

fn fingerprint_inputs(
    workspace_root: &Path,
    input_paths: &[String],
) -> AnalysisResult<Vec<AnalysisFileFingerprint>> {
    let mut fingerprints = Vec::with_capacity(input_paths.len());
    let canonical_root = workspace_root.canonicalize()?;
    for path in input_paths {
        let candidate = workspace_root.join(path);
        let canonical = match candidate.canonicalize() {
            Ok(canonical) if canonical.starts_with(&canonical_root) && canonical.is_file() => {
                canonical
            }
            _ => {
                fingerprints.push(AnalysisFileFingerprint {
                    path: path.clone(),
                    available: false,
                    bytes: None,
                    sha256: None,
                });
                continue;
            }
        };
        fingerprints.push(fingerprint_required(&canonical, path)?);
    }
    Ok(fingerprints)
}

fn fingerprint_required(
    path: &Path,
    display_path: &str,
) -> AnalysisResult<AnalysisFileFingerprint> {
    let metadata = fs::metadata(path)?;
    Ok(AnalysisFileFingerprint {
        path: display_path.to_string(),
        available: true,
        bytes: Some(metadata.len()),
        sha256: Some(hash_file(path)?),
    })
}

fn clear_generated_outputs(analysis_dir: &Path) -> AnalysisResult<()> {
    for file_name in ["environment.json", "result.json"] {
        remove_generated_path(&analysis_dir.join(file_name))?;
    }
    let outputs_dir = analysis_dir.join("outputs");
    remove_generated_path(&outputs_dir)?;
    fs::create_dir_all(&outputs_dir)?;
    Ok(())
}

fn remove_generated_path(path: &Path) -> AnalysisResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)?;
        }
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn collect_outputs(
    workspace_root: &Path,
    analysis_dir: &Path,
) -> AnalysisResult<Vec<AnalysisOutputArtifact>> {
    let mut paths = Vec::new();
    for file_name in ["environment.json", "result.json"] {
        let path = analysis_dir.join(file_name);
        if path.is_file() {
            paths.push(path);
        }
    }
    let outputs_dir = analysis_dir.join("outputs");
    if outputs_dir.is_dir() {
        collect_output_paths(&outputs_dir, &mut paths)?;
    }
    let mut outputs = Vec::new();
    for path in paths.into_iter().take(MAX_OUTPUT_FILES) {
        let metadata = fs::metadata(&path)?;
        let relative = path
            .strip_prefix(workspace_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        outputs.push(AnalysisOutputArtifact {
            path: relative,
            absolute_path: path.display().to_string(),
            kind: output_kind(&path).to_string(),
            bytes: metadata.len(),
            sha256: hash_file(&path)?,
        });
    }
    outputs.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(outputs)
}

fn archive_outputs(
    workspace_root: &Path,
    analysis_dir: &Path,
    run_id: &str,
) -> AnalysisResult<Vec<AnalysisOutputArtifact>> {
    let generated = collect_outputs(workspace_root, analysis_dir)?;
    let artifact_root = analysis_dir.join(RUNS_DIR).join(run_id).join("artifacts");
    let canonical_analysis = analysis_dir.canonicalize()?;
    let mut archived = Vec::with_capacity(generated.len());

    for artifact in generated {
        let source = PathBuf::from(&artifact.absolute_path).canonicalize()?;
        let relative = source.strip_prefix(&canonical_analysis).map_err(|_| {
            AnalysisError::Invalid(format!(
                "generated artifact is outside analysis directory: {}",
                source.display()
            ))
        })?;
        let target = artifact_root.join(relative);
        copy_artifact_atomically(&source, &target)?;
        let metadata = fs::metadata(&target)?;
        let path = target
            .strip_prefix(workspace_root)
            .map_err(|_| {
                AnalysisError::Invalid("archived artifact is outside workspace".to_string())
            })?
            .to_string_lossy()
            .to_string();
        archived.push(AnalysisOutputArtifact {
            path,
            absolute_path: target.display().to_string(),
            kind: output_kind(&target).to_string(),
            bytes: metadata.len(),
            sha256: hash_file(&target)?,
        });
    }
    archived.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(archived)
}

fn copy_artifact_atomically(source: &Path, target: &Path) -> AnalysisResult<()> {
    let parent = target.parent().ok_or_else(|| {
        AnalysisError::Invalid(format!("artifact path has no parent: {}", target.display()))
    })?;
    fs::create_dir_all(parent)?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AnalysisError::Invalid("artifact file name is not UTF-8".to_string()))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let copy_result = (|| -> std::io::Result<()> {
        let mut input = File::open(source)?;
        let mut output = File::create(&temporary)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        fs::rename(&temporary, target)
    })();
    if let Err(error) = copy_result {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

fn collect_output_paths(current: &Path, paths: &mut Vec<PathBuf>) -> AnalysisResult<()> {
    if paths.len() >= MAX_OUTPUT_FILES {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_output_paths(&path, paths)?;
        } else if metadata.is_file() {
            paths.push(path);
            if paths.len() >= MAX_OUTPUT_FILES {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn output_kind(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png" | "jpg" | "jpeg" | "webp" | "svg") => "chart",
        Some("csv" | "tsv" | "parquet" | "xlsx") => "table",
        Some("md" | "html" | "pdf" | "txt") => "report",
        Some("json") => "result",
        _ => "file",
    }
}

fn read_environment(path: &Path) -> AnalysisResult<BTreeMap<String, String>> {
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let value: Value = read_json(path)?;
    let Some(object) = value.as_object() else {
        return Ok(BTreeMap::new());
    };
    Ok(object
        .iter()
        .take(64)
        .map(|(key, value)| {
            let rendered = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            (key.clone(), bounded_text(&rendered, 256))
        })
        .collect())
}

fn run_status(result: &echo_agent::tools::ToolResult) -> AnalysisRunStatus {
    if result.success {
        return AnalysisRunStatus::Succeeded;
    }
    match result.failure.as_ref().map(|failure| failure.category) {
        Some(ToolFailureCategory::Cancelled) => AnalysisRunStatus::Cancelled,
        Some(ToolFailureCategory::Timeout) => AnalysisRunStatus::TimedOut,
        _ => AnalysisRunStatus::Failed,
    }
}

fn hash_file(path: &Path) -> AnalysisResult<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let Some(chunk) = buffer.get(..count) else {
            return Err(AnalysisError::Invalid(
                "file read exceeded the hash buffer".to_string(),
            ));
        };
        hasher.update(chunk);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hash_json(value: &Value) -> AnalysisResult<String> {
    Ok(hash_bytes(&serde_json::to_vec(value)?))
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> AnalysisResult<T> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_JSON_BYTES {
        return Err(AnalysisError::Invalid(format!(
            "analysis JSON exceeds {} bytes: {}",
            MAX_JSON_BYTES,
            path.display()
        )));
    }
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn read_optional_json<T: for<'de> Deserialize<'de>>(path: &Path) -> AnalysisResult<Option<T>> {
    if !path.is_file() {
        return Ok(None);
    }
    read_json(path).map(Some)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> AnalysisResult<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    echo_agent::utils::fs::atomic_write(path, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::error::Result as AgentResult;
    use echo_agent::tools::{Tool, ToolResult};
    use futures::future::BoxFuture;

    struct ScriptTool;
    struct ArtifactFailureTool;

    struct ProfileScriptTool {
        seen_profile: Arc<std::sync::Mutex<Option<String>>>,
    }

    impl Tool for ScriptTool {
        fn name(&self) -> &str {
            "run_code"
        }

        fn description(&self) -> &str {
            "test persisted script runner"
        }

        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        fn execute_with_context<'a>(
            &'a self,
            parameters: ToolParameters,
            context: &'a ToolContext,
        ) -> BoxFuture<'a, AgentResult<ToolResult>> {
            Box::pin(async move {
                let directory = context.working_dir.as_ref().ok_or_else(|| {
                    echo_agent::error::ReactError::Other("missing working directory".to_string())
                })?;
                let script = parameters
                    .get("script_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        echo_agent::error::ReactError::Other("missing script path".to_string())
                    })?;
                let execution_id = context.execution_id.as_deref().unwrap_or("missing");
                fs::write(
                    directory.join("result.json"),
                    format!("{{\"execution_id\":\"{execution_id}\"}}\n"),
                )?;
                fs::write(
                    directory.join("environment.json"),
                    "{\"python\": \"test\"}\n",
                )?;
                Ok(ToolResult::success(format!("ran {script}"))
                    .with_meta("exit_code", "0")
                    .with_meta("sandbox_type", "test"))
            })
        }
    }

    impl Tool for ArtifactFailureTool {
        fn name(&self) -> &str {
            "run_code"
        }

        fn description(&self) -> &str {
            "test artifact persistence failure"
        }

        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        fn execute_with_context<'a>(
            &'a self,
            _parameters: ToolParameters,
            context: &'a ToolContext,
        ) -> BoxFuture<'a, AgentResult<ToolResult>> {
            Box::pin(async move {
                let directory = context.working_dir.as_ref().ok_or_else(|| {
                    echo_agent::error::ReactError::Other("missing working directory".to_string())
                })?;
                let run_id = context.execution_id.as_deref().ok_or_else(|| {
                    echo_agent::error::ReactError::Other("missing execution id".to_string())
                })?;
                fs::write(directory.join("result.json"), "new output")?;
                fs::write(directory.join(RUNS_DIR).join(run_id), "blocks artifact dir")?;
                Ok(ToolResult::success(
                    "ran but artifact persistence will fail",
                ))
            })
        }
    }

    impl Tool for ProfileScriptTool {
        fn name(&self) -> &str {
            "run_code"
        }

        fn description(&self) -> &str {
            "test managed analysis script runner"
        }

        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        fn execute_with_context<'a>(
            &'a self,
            _parameters: ToolParameters,
            context: &'a ToolContext,
        ) -> BoxFuture<'a, AgentResult<ToolResult>> {
            Box::pin(async move {
                let directory = context.working_dir.as_ref().ok_or_else(|| {
                    echo_agent::error::ReactError::Other("missing working directory".to_string())
                })?;
                let profile_id = context
                    .script_execution_profile
                    .as_ref()
                    .map(|profile| profile.id.clone())
                    .ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "missing script execution profile".to_string(),
                        )
                    })?;
                *self
                    .seen_profile
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(profile_id);
                fs::write(directory.join("result.json"), "{\"status\":\"ok\"}\n")?;
                fs::write(
                    directory.join("environment.json"),
                    "{\"analysis.script\":\"managed\"}\n",
                )?;
                Ok(ToolResult::success("managed analysis completed")
                    .with_meta("exit_code", "0")
                    .with_meta("sandbox_type", "test"))
            })
        }
    }

    #[test]
    fn create_save_and_stale_detection_are_file_backed() -> AnalysisResult<()> {
        let workspace = tempfile::tempdir()?;
        fs::write(workspace.path().join("data.csv"), "value\n1\n")?;
        let created =
            create_analysis(workspace.path(), "Revenue Review", AnalysisLanguage::Python)?;
        assert!(created.stale);
        assert!(created.script.contains("result.json"));

        let saved = save_analysis(
            workspace.path(),
            &created.manifest.analysis_id,
            SaveAnalysisRequest {
                title: "Revenue Review".to_string(),
                script: "print('saved')\n".to_string(),
                expected_script_revision: created.script_revision,
                input_paths: vec!["data.csv".to_string()],
                parameters: serde_json::json!({"group": "region"}),
                random_seed: Some(7),
            },
        )?;
        assert_eq!(saved.manifest.input_paths, vec!["data.csv".to_string()]);
        assert_eq!(saved.manifest.random_seed, Some(7));
        assert!(saved.stale);
        Ok(())
    }

    #[test]
    fn r_starter_requires_only_the_base_runtime() -> AnalysisResult<()> {
        let workspace = tempfile::tempdir()?;
        let created = create_analysis(workspace.path(), "R Review", AnalysisLanguage::R)?;
        assert!(created.script.contains("R.version"));
        assert!(!created.script.contains("jsonlite"));
        Ok(())
    }

    #[test]
    fn unicode_titles_and_input_paths_remain_valid() -> AnalysisResult<()> {
        let workspace = tempfile::tempdir()?;
        fs::write(workspace.path().join("数据📈.csv"), "数值\n1\n")?;
        let created =
            create_analysis(workspace.path(), "医学数据分析📊", AnalysisLanguage::Python)?;
        let saved = save_analysis(
            workspace.path(),
            &created.manifest.analysis_id,
            SaveAnalysisRequest {
                title: "医学数据分析📊".to_string(),
                script: created.script,
                expected_script_revision: created.script_revision,
                input_paths: vec!["数据📈.csv".to_string()],
                parameters: Value::Null,
                random_seed: None,
            },
        )?;
        assert_eq!(saved.manifest.title, "医学数据分析📊");
        assert_eq!(saved.manifest.input_paths, vec!["数据📈.csv".to_string()]);
        Ok(())
    }

    #[tokio::test]
    async fn run_records_hashes_outputs_environment_and_staleness() -> AnalysisResult<()> {
        let workspace = tempfile::tempdir()?;
        fs::write(workspace.path().join("data.csv"), "value\n1\n")?;
        let created = create_analysis(workspace.path(), "Model", AnalysisLanguage::Python)?;
        let saved = save_analysis(
            workspace.path(),
            &created.manifest.analysis_id,
            SaveAnalysisRequest {
                title: "Model".to_string(),
                script: "print('model')\n".to_string(),
                expected_script_revision: created.script_revision,
                input_paths: vec!["data.csv".to_string()],
                parameters: Value::Object(serde_json::Map::new()),
                random_seed: Some(11),
            },
        )?;
        let manager = ToolManager::new();
        manager.register(Box::new(ScriptTool));
        let completed = run_analysis(
            &manager,
            workspace.path(),
            &saved.manifest.analysis_id,
            None,
        )
        .await?;
        assert!(!completed.stale);
        let run = completed
            .last_run
            .as_ref()
            .ok_or_else(|| AnalysisError::Invalid("missing run record".to_string()))?;
        assert_eq!(run.status, AnalysisRunStatus::Succeeded);
        assert_eq!(
            run.environment.get("python").map(String::as_str),
            Some("test")
        );
        assert!(
            run.outputs
                .iter()
                .any(|output| output.path.ends_with("result.json"))
        );
        assert!(run.script.sha256.is_some());
        assert!(run.inputs.iter().all(|input| input.sha256.is_some()));
        assert_eq!(run.random_seed, Some(11));
        assert_eq!(run.parameters, Value::Object(serde_json::Map::new()));

        let parameter_changed = save_analysis(
            workspace.path(),
            &saved.manifest.analysis_id,
            SaveAnalysisRequest {
                title: "Model".to_string(),
                script: saved.script.clone(),
                expected_script_revision: saved.script_revision.clone(),
                input_paths: vec!["data.csv".to_string()],
                parameters: serde_json::json!({"group": "region"}),
                random_seed: Some(11),
            },
        )?;
        assert!(parameter_changed.stale);
        assert!(
            parameter_changed
                .stale_reasons
                .iter()
                .any(|reason| reason.contains("parameters changed"))
        );

        fs::write(workspace.path().join("data.csv"), "value\n2\n")?;
        let changed = load_analysis(workspace.path(), &saved.manifest.analysis_id)?;
        assert!(changed.stale);
        assert!(
            changed
                .stale_reasons
                .iter()
                .any(|reason| reason.contains("input changed"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn managed_runtime_profile_reaches_run_code_and_run_record() -> AnalysisResult<()> {
        let workspace = tempfile::tempdir()?;
        let created = create_analysis(workspace.path(), "Managed", AnalysisLanguage::Python)?;
        let seen_profile = Arc::new(std::sync::Mutex::new(None));
        let manager = ToolManager::new();
        manager.register(Box::new(ProfileScriptTool {
            seen_profile: seen_profile.clone(),
        }));
        let prepared = crate::analysis_runtime::PreparedAnalyticsRuntime {
            profile: Arc::new(echo_agent::tools::ScriptExecutionProfile::new(
                "eko-analytics:test-lock",
                "python",
                "/tmp/eko-analytics/.venv/bin/python",
            )),
            environment: BTreeMap::from([
                (
                    "analytics.profile".to_string(),
                    "eko-analytics:test-lock".to_string(),
                ),
                ("python".to_string(), "3.12.4".to_string()),
                ("python.package.pandas".to_string(), "3.0.5".to_string()),
            ]),
        };

        let completed = run_analysis_with_runtime(
            &manager,
            workspace.path(),
            &created.manifest.analysis_id,
            None,
            Some(&prepared),
        )
        .await?;
        let run = completed
            .last_run
            .ok_or_else(|| AnalysisError::Invalid("missing managed run".to_string()))?;
        assert_eq!(
            seen_profile
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_deref(),
            Some("eko-analytics:test-lock")
        );
        assert_eq!(
            run.runtime_profile.as_deref(),
            Some("eko-analytics:test-lock")
        );
        assert_eq!(
            run.environment.get("python").map(String::as_str),
            Some("3.12.4")
        );
        assert_eq!(
            run.environment
                .get("python.package.pandas")
                .map(String::as_str),
            Some("3.0.5")
        );
        assert_eq!(
            run.environment.get("analysis.script").map(String::as_str),
            Some("managed")
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_rerun_does_not_inherit_previous_outputs() -> AnalysisResult<()> {
        let workspace = tempfile::tempdir()?;
        let created = create_analysis(workspace.path(), "Refresh", AnalysisLanguage::Python)?;
        let successful_manager = ToolManager::new();
        successful_manager.register(Box::new(ScriptTool));
        let completed = run_analysis(
            &successful_manager,
            workspace.path(),
            &created.manifest.analysis_id,
            None,
        )
        .await?;
        assert!(!completed.outputs.is_empty());

        let failed = run_analysis(
            &ToolManager::new(),
            workspace.path(),
            &created.manifest.analysis_id,
            None,
        )
        .await?;
        assert_eq!(
            failed.last_run.as_ref().map(|run| run.status),
            Some(AnalysisRunStatus::Failed)
        );
        assert!(failed.outputs.is_empty());
        assert!(
            failed
                .last_run
                .as_ref()
                .is_some_and(|run| run.environment.is_empty())
        );
        Ok(())
    }

    #[tokio::test]
    async fn rerun_preserves_prior_run_artifacts() -> AnalysisResult<()> {
        let workspace = tempfile::tempdir()?;
        let created = create_analysis(workspace.path(), "History", AnalysisLanguage::Python)?;
        let manager = ToolManager::new();
        manager.register(Box::new(ScriptTool));

        let first = run_analysis(
            &manager,
            workspace.path(),
            &created.manifest.analysis_id,
            None,
        )
        .await?;
        let first_run = first
            .last_run
            .ok_or_else(|| AnalysisError::Invalid("missing first run".to_string()))?;
        let first_result = first_run
            .outputs
            .iter()
            .find(|artifact| artifact.path.ends_with("result.json"))
            .ok_or_else(|| AnalysisError::Invalid("missing first result artifact".to_string()))?;
        let first_path = PathBuf::from(&first_result.absolute_path);
        let first_hash = first_result.sha256.clone();
        assert!(first_result.path.contains(&first_run.run_id));

        let second = run_analysis(
            &manager,
            workspace.path(),
            &created.manifest.analysis_id,
            None,
        )
        .await?;
        let second_run = second
            .last_run
            .ok_or_else(|| AnalysisError::Invalid("missing second run".to_string()))?;
        let second_result = second_run
            .outputs
            .iter()
            .find(|artifact| artifact.path.ends_with("result.json"))
            .ok_or_else(|| AnalysisError::Invalid("missing second result artifact".to_string()))?;

        assert_ne!(first_run.run_id, second_run.run_id);
        assert_ne!(first_result.path, second_result.path);
        assert_ne!(first_result.sha256, second_result.sha256);
        assert!(first_path.is_file());
        assert_eq!(hash_file(&first_path)?, first_hash);
        Ok(())
    }

    #[tokio::test]
    async fn artifact_failure_does_not_publish_a_new_run_record() -> AnalysisResult<()> {
        let workspace = tempfile::tempdir()?;
        let created = create_analysis(workspace.path(), "Publish", AnalysisLanguage::Python)?;
        let successful_manager = ToolManager::new();
        successful_manager.register(Box::new(ScriptTool));
        let completed = run_analysis(
            &successful_manager,
            workspace.path(),
            &created.manifest.analysis_id,
            None,
        )
        .await?;
        let previous_run_id = completed
            .last_run
            .as_ref()
            .map(|run| run.run_id.clone())
            .ok_or_else(|| AnalysisError::Invalid("missing previous run".to_string()))?;

        let failed_manager = ToolManager::new();
        failed_manager.register(Box::new(ArtifactFailureTool));
        let result = run_analysis(
            &failed_manager,
            workspace.path(),
            &created.manifest.analysis_id,
            None,
        )
        .await;
        assert!(matches!(result, Err(AnalysisError::Io(_))));

        let reloaded = load_analysis(workspace.path(), &created.manifest.analysis_id)?;
        assert_eq!(
            reloaded.last_run.map(|run| run.run_id),
            Some(previous_run_id)
        );
        Ok(())
    }

    #[test]
    fn save_rejects_stale_revision_and_parent_inputs() -> AnalysisResult<()> {
        let workspace = tempfile::tempdir()?;
        let created = create_analysis(workspace.path(), "Guard", AnalysisLanguage::Python)?;
        let conflict = save_analysis(
            workspace.path(),
            &created.manifest.analysis_id,
            SaveAnalysisRequest {
                title: "Guard".to_string(),
                script: "print(1)\n".to_string(),
                expected_script_revision: "old".to_string(),
                input_paths: Vec::new(),
                parameters: Value::Null,
                random_seed: None,
            },
        );
        assert!(matches!(conflict, Err(AnalysisError::Conflict)));

        let invalid_input = save_analysis(
            workspace.path(),
            &created.manifest.analysis_id,
            SaveAnalysisRequest {
                title: "Guard".to_string(),
                script: created.script,
                expected_script_revision: created.script_revision,
                input_paths: vec!["../secret.csv".to_string()],
                parameters: Value::Null,
                random_seed: None,
            },
        );
        assert!(matches!(invalid_input, Err(AnalysisError::Invalid(_))));
        Ok(())
    }
}
