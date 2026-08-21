//! Application-owned durable tool-execution projection.
//!
//! Each execution has one atomic manifest containing the canonical framework
//! invocation and terminal result. Non-terminal stdout/stderr/progress is kept
//! in a separate append-only trace so the GUI can follow a running tool without
//! treating streamed chunks as a second terminal result. Large terminal output
//! is still read only through the framework's verified artifact reader.

use echo_agent::agent::ToolInvocation;
use echo_agent::tools::{ToolFailureCategory, ToolResult};
use echo_agent::utils::time::now_millis;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use thiserror::Error;

pub const TOOL_ARGS_PREVIEW_CHARS: usize = 160;
pub const DEFAULT_DETAIL_PAGE_BYTES: usize = 64 * 1024;
const STREAM_SCAN_BYTES: usize = 8 * 1024;
const STREAM_CURSOR_PREFIX: &str = "stream-v1";

#[derive(Debug, Error)]
pub enum ToolExecutionError {
    #[error("tool execution not found: {0}")]
    NotFound(String),
    #[error("invalid detail cursor: {0}")]
    InvalidCursor(String),
    #[error("tool projection conflicts with the persisted execution: {0}")]
    ProjectionConflict(String),
    #[error("invalid orphan tool terminal status: {0:?}")]
    InvalidTerminalStatus(ToolExecutionStatus),
    #[error("tool artifact root is not registered: {0}")]
    ArtifactRootUnavailable(String),
    #[error("conversation still has active tool executions: {0}")]
    ActiveConversation(String),
    #[error(transparent)]
    ArtifactRead(#[from] echo_agent::tools::files::artifact::ArtifactReadError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type ToolExecutionResult<T> = Result<T, ToolExecutionError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolExecutionOwner {
    Chat { message_id: String },
    Subagent { subagent_run_id: String },
}

impl ToolExecutionOwner {
    fn key(&self) -> &str {
        match self {
            Self::Chat { message_id } => message_id,
            Self::Subagent { subagent_run_id } => subagent_run_id,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Interrupted,
    Unknown,
}

impl ToolExecutionStatus {
    fn from_result(result: &ToolResult) -> Self {
        result.failure.as_ref().map_or_else(
            || {
                if result.success {
                    Self::Succeeded
                } else {
                    Self::Failed
                }
            },
            |failure| match failure.category {
                ToolFailureCategory::Timeout => Self::TimedOut,
                ToolFailureCategory::Cancelled => Self::Cancelled,
                ToolFailureCategory::InvalidArguments
                | ToolFailureCategory::Unavailable
                | ToolFailureCategory::Transient
                | ToolFailureCategory::Permanent
                | ToolFailureCategory::PartialSideEffect => Self::Failed,
            },
        )
    }

    fn is_orphan_terminal(self) -> bool {
        matches!(
            self,
            Self::Cancelled | Self::TimedOut | Self::Interrupted | Self::Unknown
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolExecutionSummary {
    pub id: String,
    pub call_id: String,
    pub owner: ToolExecutionOwner,
    pub conversation_id: Option<String>,
    pub run_id: Option<String>,
    pub name: String,
    pub args_preview: String,
    pub status: ToolExecutionStatus,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub duration_ms: Option<u64>,
    pub detail_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolExecutionDetailManifest {
    pub id: String,
    pub invocation: ToolInvocation,
    pub status: ToolExecutionStatus,
    pub result: Option<ToolResult>,
    pub output_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionDetailChannel {
    Stdout,
    Stderr,
    Log,
    Result,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolExecutionDetailChunk {
    pub channel: ToolExecutionDetailChannel,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolExecutionDetailPage {
    pub chunks: Vec<ToolExecutionDetailChunk>,
    pub next_cursor: Option<String>,
    pub complete: bool,
}

/// Result of applying one canonical event to the durable projection.
///
/// Replayed events return the existing summary with `changed = false`. This
/// lets every surface share the same projector without emitting duplicate UI
/// updates when two canonical event buses observe the same execution.
#[derive(Debug, Clone)]
pub struct ToolExecutionMutation {
    pub summary: ToolExecutionSummary,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredManifest {
    summary: ToolExecutionSummary,
    invocation: ToolInvocation,
    result: Option<ToolResult>,
    output_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredStreamChunk {
    channel: ToolExecutionDetailChannel,
    text: String,
}

#[derive(Debug, Clone)]
struct DetailLocation {
    manifest: PathBuf,
    stream: PathBuf,
}

#[derive(Default)]
struct RepositoryState {
    details: HashMap<String, DetailLocation>,
    summaries: HashMap<String, ToolExecutionSummary>,
}

pub struct ToolExecutionRepository {
    root: PathBuf,
    state: Mutex<RepositoryState>,
    projection_lock: Mutex<()>,
    artifact_configs: Mutex<Vec<echo_agent::tools::artifact::ToolOutputArtifactConfig>>,
}

impl ToolExecutionRepository {
    pub fn open(root: impl Into<PathBuf>) -> ToolExecutionResult<Self> {
        let root = root.into();
        reject_symlink(&root)?;
        fs::create_dir_all(&root)?;
        let repository = Self {
            root,
            state: Mutex::new(RepositoryState::default()),
            projection_lock: Mutex::new(()),
            artifact_configs: Mutex::new(Vec::new()),
        };
        repository.rebuild_index_and_recover()?;
        Ok(repository)
    }

    pub fn without_initialization(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state: Mutex::new(RepositoryState::default()),
            projection_lock: Mutex::new(()),
            artifact_configs: Mutex::new(Vec::new()),
        }
    }

    pub fn default_root() -> PathBuf {
        echo_agent::paths::user_data_path("tool-executions")
    }

    pub fn register_artifact_config(
        &self,
        config: echo_agent::tools::artifact::ToolOutputArtifactConfig,
    ) {
        let mut configs = lock_recover(&self.artifact_configs, "tool artifact roots");
        if configs
            .iter()
            .all(|existing| existing.root_dir != config.root_dir)
        {
            configs.push(config);
        }
    }

    pub fn project_start(
        &self,
        owner: ToolExecutionOwner,
        conversation_id: Option<&str>,
        run_id: Option<&str>,
        call_id: &str,
        invocation: &ToolInvocation,
    ) -> ToolExecutionResult<ToolExecutionMutation> {
        let _projection = lock_recover(&self.projection_lock, "tool execution projection");
        if let Some(existing) = self.summary_for(&owner, call_id) {
            let manifest = self.read_manifest(&existing.detail_ref)?;
            let identity_matches = manifest.invocation == *invocation
                && existing.owner == owner
                && existing.conversation_id.as_deref() == conversation_id
                && existing.run_id.as_deref() == run_id;
            if identity_matches {
                return Ok(ToolExecutionMutation {
                    summary: existing,
                    changed: false,
                });
            }
            return Err(ToolExecutionError::ProjectionConflict(format!(
                "{}:{}",
                owner.key(),
                call_id
            )));
        }
        self.start_new(owner, conversation_id, run_id, call_id, invocation)
            .map(|summary| ToolExecutionMutation {
                summary,
                changed: true,
            })
    }

    fn start_new(
        &self,
        owner: ToolExecutionOwner,
        conversation_id: Option<&str>,
        run_id: Option<&str>,
        call_id: &str,
        invocation: &ToolInvocation,
    ) -> ToolExecutionResult<ToolExecutionSummary> {
        let detail_ref = uuid::Uuid::new_v4().to_string();
        let detail_dir = self
            .scope_dir(conversation_id, run_id.or(Some(owner.key())))
            .join("details");
        fs::create_dir_all(&detail_dir)?;
        let location = DetailLocation {
            manifest: detail_dir.join(format!("{detail_ref}.json")),
            stream: detail_dir.join(format!("{detail_ref}.stream.jsonl")),
        };
        let now = now_millis();
        let summary = ToolExecutionSummary {
            id: detail_ref.clone(),
            call_id: call_id.to_string(),
            owner,
            conversation_id: conversation_id.map(str::to_string),
            run_id: run_id.map(str::to_string),
            name: invocation.name.clone(),
            args_preview: preview_args(&invocation.args),
            status: ToolExecutionStatus::Running,
            started_at: now,
            finished_at: None,
            duration_ms: None,
            detail_ref: detail_ref.clone(),
        };
        let manifest = StoredManifest {
            summary: summary.clone(),
            invocation: invocation.clone(),
            result: None,
            output_bytes: 0,
        };
        write_manifest(&location.manifest, &manifest)?;

        let key = execution_key(&summary.owner, call_id);
        let mut state = self.lock_state();
        state.details.insert(detail_ref, location);
        state.summaries.insert(key, summary.clone());
        Ok(summary)
    }

    /// Append non-terminal tool output without changing the canonical result.
    pub fn project_stream(
        &self,
        owner: &ToolExecutionOwner,
        call_id: &str,
        channel: ToolExecutionDetailChannel,
        text: &str,
    ) -> ToolExecutionResult<()> {
        if text.is_empty() {
            return Ok(());
        }
        let _projection = lock_recover(&self.projection_lock, "tool execution projection");
        let summary = self
            .summary_for(owner, call_id)
            .ok_or_else(|| ToolExecutionError::NotFound(call_id.to_string()))?;
        if summary.status != ToolExecutionStatus::Running {
            return Err(ToolExecutionError::ProjectionConflict(format!(
                "{}:{} stream after terminal {:?}",
                owner.key(),
                call_id,
                summary.status
            )));
        }
        let location = self.detail_location(&summary.detail_ref)?;
        append_stream_chunk(
            &location.stream,
            &StoredStreamChunk {
                channel,
                text: text.to_string(),
            },
        )
    }

    pub fn project_finish(
        &self,
        owner: &ToolExecutionOwner,
        call_id: &str,
        result: &ToolResult,
    ) -> ToolExecutionResult<ToolExecutionMutation> {
        let _projection = lock_recover(&self.projection_lock, "tool execution projection");
        let existing = self
            .summary_for(owner, call_id)
            .ok_or_else(|| ToolExecutionError::NotFound(call_id.to_string()))?;
        let location = self.detail_location(&existing.detail_ref)?;
        let mut manifest = read_manifest_path(&location.manifest)?;
        if let Some(persisted) = manifest.result.as_ref() {
            if canonical_json(persisted)? == canonical_json(result)? {
                return Ok(ToolExecutionMutation {
                    summary: existing,
                    changed: false,
                });
            }
            return Err(ToolExecutionError::ProjectionConflict(format!(
                "{}:{} terminal result",
                owner.key(),
                call_id
            )));
        }

        let finished_at = now_millis();
        manifest.summary.status = ToolExecutionStatus::from_result(result);
        manifest.summary.finished_at = Some(finished_at);
        manifest.summary.duration_ms =
            Some(finished_at.saturating_sub(manifest.summary.started_at));
        manifest.output_bytes =
            stream_output_bytes(&location.stream)?.unwrap_or_else(|| result_output_bytes(result));
        manifest.result = Some(result.clone());
        write_manifest(&location.manifest, &manifest)?;

        let summary = manifest.summary;
        self.lock_state()
            .summaries
            .insert(execution_key(owner, call_id), summary.clone());
        Ok(ToolExecutionMutation {
            summary,
            changed: true,
        })
    }

    pub fn terminate_orphan(
        &self,
        owner: &ToolExecutionOwner,
        call_id: &str,
        status: ToolExecutionStatus,
    ) -> ToolExecutionResult<ToolExecutionMutation> {
        let _projection = lock_recover(&self.projection_lock, "tool execution projection");
        if !status.is_orphan_terminal() {
            return Err(ToolExecutionError::InvalidTerminalStatus(status));
        }
        let existing = self
            .summary_for(owner, call_id)
            .ok_or_else(|| ToolExecutionError::NotFound(call_id.to_string()))?;
        if existing.status != ToolExecutionStatus::Running {
            return Ok(ToolExecutionMutation {
                summary: existing,
                changed: false,
            });
        }
        let location = self.detail_location(&existing.detail_ref)?;
        let mut manifest = read_manifest_path(&location.manifest)?;
        let finished_at = now_millis();
        manifest.summary.status = status;
        manifest.summary.finished_at = Some(finished_at);
        manifest.summary.duration_ms =
            Some(finished_at.saturating_sub(manifest.summary.started_at));
        manifest.output_bytes = stream_output_bytes(&location.stream)?.unwrap_or(0);
        write_manifest(&location.manifest, &manifest)?;

        let summary = manifest.summary;
        self.lock_state()
            .summaries
            .insert(execution_key(owner, call_id), summary.clone());
        Ok(ToolExecutionMutation {
            summary,
            changed: true,
        })
    }

    pub fn detail_manifest(
        &self,
        detail_ref: &str,
    ) -> ToolExecutionResult<ToolExecutionDetailManifest> {
        let location = self.detail_location(detail_ref)?;
        let manifest = read_manifest_path(&location.manifest)?;
        Ok(ToolExecutionDetailManifest {
            id: manifest.summary.id,
            invocation: manifest.invocation,
            status: manifest.summary.status,
            result: manifest.result,
            output_bytes: stream_output_bytes(&location.stream)?.unwrap_or(manifest.output_bytes),
        })
    }

    pub fn read_output(
        &self,
        detail_ref: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> ToolExecutionResult<ToolExecutionDetailPage> {
        let location = self.detail_location(detail_ref)?;
        let manifest = read_manifest_path(&location.manifest)?;
        if stream_has_output(&location.stream)? {
            return read_stream_output_page(
                detail_ref,
                &location.stream,
                cursor,
                limit.clamp(4, DEFAULT_DETAIL_PAGE_BYTES),
                manifest.summary.status != ToolExecutionStatus::Running,
            );
        }
        let Some(result) = manifest.result.as_ref() else {
            if let Some(cursor) = cursor {
                return Err(ToolExecutionError::InvalidCursor(cursor.to_string()));
            }
            return Ok(ToolExecutionDetailPage {
                chunks: Vec::new(),
                next_cursor: None,
                complete: false,
            });
        };
        if let Some(artifact) =
            echo_agent::tools::artifact::ToolOutputArtifactRef::from_metadata(&result.metadata)
        {
            let config = self.artifact_config_for(&artifact.path).ok_or_else(|| {
                ToolExecutionError::ArtifactRootUnavailable(artifact.path.display().to_string())
            })?;
            let page = echo_agent::tools::files::artifact::read_artifact_page(
                &config,
                &artifact,
                cursor,
                echo_agent::tools::files::artifact::ArtifactPageLimit::Bytes(
                    limit.clamp(4, DEFAULT_DETAIL_PAGE_BYTES),
                ),
            )?;
            return Ok(ToolExecutionDetailPage {
                chunks: (!page.content.is_empty())
                    .then_some(ToolExecutionDetailChunk {
                        channel: ToolExecutionDetailChannel::Log,
                        text: page.content,
                    })
                    .into_iter()
                    .collect(),
                next_cursor: page.next_cursor,
                complete: page.complete,
            });
        }
        if let Some(cursor) = cursor {
            return Err(ToolExecutionError::InvalidCursor(cursor.to_string()));
        }
        let text = result_text(result);
        Ok(ToolExecutionDetailPage {
            chunks: (!text.is_empty())
                .then_some(ToolExecutionDetailChunk {
                    channel: ToolExecutionDetailChannel::Result,
                    text: text.to_string(),
                })
                .into_iter()
                .collect(),
            next_cursor: None,
            complete: true,
        })
    }

    pub fn summaries_for_conversation(&self, conversation_id: &str) -> Vec<ToolExecutionSummary> {
        let state = self.lock_state();
        let mut summaries = state
            .summaries
            .values()
            .filter(|summary| summary.conversation_id.as_deref() == Some(conversation_id))
            .cloned()
            .collect::<Vec<_>>();
        summaries.sort_by_key(|summary| summary.started_at);
        summaries
    }

    pub fn summary_for(
        &self,
        owner: &ToolExecutionOwner,
        call_id: &str,
    ) -> Option<ToolExecutionSummary> {
        self.lock_state()
            .summaries
            .get(&execution_key(owner, call_id))
            .cloned()
    }

    pub fn terminate_running_for_owner(
        &self,
        owner: &ToolExecutionOwner,
        status: ToolExecutionStatus,
    ) -> ToolExecutionResult<Vec<ToolExecutionSummary>> {
        if !status.is_orphan_terminal() {
            return Err(ToolExecutionError::InvalidTerminalStatus(status));
        }
        let call_ids = {
            let state = self.lock_state();
            state
                .summaries
                .values()
                .filter(|summary| summary.owner == *owner)
                .filter(|summary| summary.status == ToolExecutionStatus::Running)
                .map(|summary| summary.call_id.clone())
                .collect::<Vec<_>>()
        };
        let mut summaries = Vec::with_capacity(call_ids.len());
        for call_id in call_ids {
            let mutation = self.terminate_orphan(owner, &call_id, status)?;
            if mutation.changed {
                summaries.push(mutation.summary);
            }
        }
        Ok(summaries)
    }

    pub fn remove_conversation(&self, conversation_id: &str) -> ToolExecutionResult<()> {
        let scope = self
            .root
            .join(echo_agent::tools::artifact::artifact_scope_component(
                conversation_id,
            ));
        {
            let state = self.lock_state();
            if state.summaries.values().any(|summary| {
                summary.conversation_id.as_deref() == Some(conversation_id)
                    && summary.status == ToolExecutionStatus::Running
            }) {
                return Err(ToolExecutionError::ActiveConversation(
                    conversation_id.to_string(),
                ));
            }
        }
        let tombstone = if scope.exists() {
            let trash_root = self.root.join(".trash");
            fs::create_dir_all(&trash_root)?;
            let tombstone = trash_root.join(format!(
                "{}-{}",
                echo_agent::tools::artifact::artifact_scope_component(conversation_id),
                uuid::Uuid::new_v4()
            ));
            fs::rename(&scope, &tombstone)?;
            Some(tombstone)
        } else {
            None
        };
        {
            let mut state = self.lock_state();
            let removed_details = state
                .summaries
                .values()
                .filter(|summary| summary.conversation_id.as_deref() == Some(conversation_id))
                .map(|summary| summary.detail_ref.clone())
                .collect::<Vec<_>>();
            state
                .summaries
                .retain(|_, summary| summary.conversation_id.as_deref() != Some(conversation_id));
            for detail_ref in removed_details {
                state.details.remove(&detail_ref);
            }
        }
        if let Some(tombstone) = tombstone {
            std::thread::spawn(move || {
                if let Err(error) = fs::remove_dir_all(&tombstone) {
                    tracing::warn!(path = %tombstone.display(), %error, "tool execution tombstone cleanup failed");
                }
            });
        }
        Ok(())
    }

    fn rebuild_index_and_recover(&self) -> ToolExecutionResult<()> {
        for path in find_manifest_files(&self.root)? {
            let location = DetailLocation {
                stream: stream_path_for_manifest(&path),
                manifest: path,
            };
            let recovered = (|| -> ToolExecutionResult<StoredManifest> {
                repair_torn_stream_tail(&location.stream)?;
                let mut manifest = read_manifest_path(&location.manifest)?;
                if manifest.summary.status == ToolExecutionStatus::Running {
                    let finished_at = now_millis();
                    manifest.summary.status = ToolExecutionStatus::Interrupted;
                    manifest.summary.finished_at = Some(finished_at);
                    manifest.summary.duration_ms =
                        Some(finished_at.saturating_sub(manifest.summary.started_at));
                    manifest.output_bytes = stream_output_bytes(&location.stream)?.unwrap_or(0);
                    write_manifest(&location.manifest, &manifest)?;
                }
                Ok(manifest)
            })();
            let manifest = match recovered {
                Ok(manifest) => manifest,
                Err(error) => {
                    quarantine_manifest(&location.manifest, &error);
                    continue;
                }
            };
            let key = execution_key(&manifest.summary.owner, &manifest.summary.call_id);
            let mut state = self.lock_state();
            if state.summaries.contains_key(&key) {
                let error = ToolExecutionError::ProjectionConflict(format!(
                    "duplicate persisted identity {key:?}"
                ));
                drop(state);
                quarantine_manifest(&location.manifest, &error);
                continue;
            }
            state
                .details
                .insert(manifest.summary.detail_ref.clone(), location);
            state.summaries.insert(key, manifest.summary);
        }
        Ok(())
    }

    fn artifact_config_for(
        &self,
        path: &Path,
    ) -> Option<echo_agent::tools::artifact::ToolOutputArtifactConfig> {
        let canonical_path = fs::canonicalize(path).ok()?;
        let configs = lock_recover(&self.artifact_configs, "tool artifact roots");
        configs.iter().find_map(|config| {
            let canonical_root = fs::canonicalize(&config.root_dir).ok()?;
            canonical_path
                .starts_with(canonical_root)
                .then(|| config.clone())
        })
    }

    fn scope_dir(&self, conversation_id: Option<&str>, run_id: Option<&str>) -> PathBuf {
        let conversation = echo_agent::tools::artifact::artifact_scope_component(
            conversation_id.unwrap_or("unscoped-conversation"),
        );
        let run =
            echo_agent::tools::artifact::artifact_scope_component(run_id.unwrap_or("unscoped-run"));
        self.root.join(conversation).join(run)
    }

    fn detail_location(&self, detail_ref: &str) -> ToolExecutionResult<DetailLocation> {
        self.lock_state()
            .details
            .get(detail_ref)
            .cloned()
            .ok_or_else(|| ToolExecutionError::NotFound(detail_ref.to_string()))
    }

    fn read_manifest(&self, detail_ref: &str) -> ToolExecutionResult<StoredManifest> {
        read_manifest_path(&self.detail_location(detail_ref)?.manifest)
    }

    fn lock_state(&self) -> MutexGuard<'_, RepositoryState> {
        lock_recover(&self.state, "tool execution repository")
    }
}

pub fn preview_args(args: &serde_json::Value) -> String {
    let serialized = serde_json::to_string(args).unwrap_or_default();
    if serialized.chars().count() <= TOOL_ARGS_PREVIEW_CHARS {
        serialized
    } else {
        format!(
            "{}...",
            serialized
                .chars()
                .take(TOOL_ARGS_PREVIEW_CHARS)
                .collect::<String>()
        )
    }
}

fn result_text(result: &ToolResult) -> &str {
    if result.output.is_empty() {
        result.error.as_deref().unwrap_or_default()
    } else {
        &result.output
    }
}

fn result_output_bytes(result: &ToolResult) -> u64 {
    echo_agent::tools::artifact::ToolOutputArtifactRef::from_metadata(&result.metadata)
        .map(|artifact| artifact.artifact_bytes)
        .unwrap_or_else(|| u64::try_from(result_text(result).len()).unwrap_or(u64::MAX))
}

fn stream_path_for_manifest(manifest: &Path) -> PathBuf {
    manifest.with_extension("stream.jsonl")
}

fn stream_has_output(path: &Path) -> ToolExecutionResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "tool stream trace must not be a symlink: {}",
                path.display()
            ),
        )
        .into()),
        Ok(metadata) => Ok(metadata.is_file() && metadata.len() > 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn append_stream_chunk(path: &Path, chunk: &StoredStreamChunk) -> ToolExecutionResult<()> {
    reject_symlink(path)?;
    repair_torn_stream_tail(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, chunk)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

fn stream_output_bytes(path: &Path) -> ToolExecutionResult<Option<u64>> {
    if !stream_has_output(path)? {
        return Ok(None);
    }
    let mut total = 0_u64;
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        if !line.ends_with('\n') {
            break;
        }
        let chunk: StoredStreamChunk = serde_json::from_str(&line)?;
        total = total.saturating_add(u64::try_from(chunk.text.len()).unwrap_or(u64::MAX));
    }
    Ok(Some(total))
}

fn stream_cursor(detail_ref: &str, offset: u64) -> String {
    format!("{STREAM_CURSOR_PREFIX}:{detail_ref}:{offset}")
}

fn parse_stream_cursor(detail_ref: &str, cursor: Option<&str>) -> ToolExecutionResult<u64> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let prefix = format!("{STREAM_CURSOR_PREFIX}:{detail_ref}:");
    cursor
        .strip_prefix(&prefix)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| ToolExecutionError::InvalidCursor(cursor.to_string()))
}

fn read_stream_output_page(
    detail_ref: &str,
    path: &Path,
    cursor: Option<&str>,
    limit: usize,
    terminal: bool,
) -> ToolExecutionResult<ToolExecutionDetailPage> {
    reject_symlink(path)?;
    let start = parse_stream_cursor(detail_ref, cursor)?;
    let end = fs::metadata(path)?.len();
    if start > end {
        return Err(ToolExecutionError::InvalidCursor(
            cursor.unwrap_or_default().to_string(),
        ));
    }
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(start))?;
    let mut consumed = 0_usize;
    let mut line = String::new();
    let mut chunks = Vec::new();
    loop {
        line.clear();
        let line_start = reader.stream_position()?;
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        if !line.ends_with('\n') {
            reader.seek(SeekFrom::Start(line_start))?;
            break;
        }
        if consumed > 0 && consumed.saturating_add(bytes) > limit {
            reader.seek(SeekFrom::Start(line_start))?;
            break;
        }
        consumed = consumed.saturating_add(bytes);
        let stored: StoredStreamChunk = serde_json::from_str(&line)?;
        chunks.push(ToolExecutionDetailChunk {
            channel: stored.channel,
            text: stored.text,
        });
        if consumed >= limit {
            break;
        }
    }
    let next = reader.stream_position()?;
    Ok(ToolExecutionDetailPage {
        chunks,
        next_cursor: (next < end || !terminal).then(|| stream_cursor(detail_ref, next)),
        complete: terminal && next >= end,
    })
}

fn repair_torn_stream_tail(path: &Path) -> ToolExecutionResult<()> {
    reject_symlink(path)?;
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if length == 0 {
        return Ok(());
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)?;
    if last == *b"\n" {
        return Ok(());
    }

    let mut end = length;
    let scan_bytes = u64::try_from(STREAM_SCAN_BYTES).unwrap_or(u64::MAX);
    while end > 0 {
        let start = end.saturating_sub(scan_bytes);
        let size = usize::try_from(end.saturating_sub(start)).unwrap_or(STREAM_SCAN_BYTES);
        let mut chunk = vec![0_u8; size];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk)?;
        if let Some(position) = chunk.iter().rposition(|byte| *byte == b'\n') {
            let valid = start
                .saturating_add(u64::try_from(position).unwrap_or(u64::MAX))
                .saturating_add(1);
            file.set_len(valid)?;
            file.sync_data()?;
            return Ok(());
        }
        end = start;
    }
    file.set_len(0)?;
    file.sync_data()?;
    Ok(())
}

fn canonical_json<T: Serialize>(value: &T) -> ToolExecutionResult<serde_json::Value> {
    let mut value = serde_json::to_value(value)?;
    canonicalize_json(&mut value);
    Ok(value)
}

fn canonicalize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                canonicalize_json(value);
            }
        }
        serde_json::Value::Object(entries) => {
            for value in entries.values_mut() {
                canonicalize_json(value);
            }
            let mut ordered = std::mem::take(entries).into_iter().collect::<Vec<_>>();
            ordered.sort_by(|left, right| left.0.cmp(&right.0));
            entries.extend(ordered);
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

fn execution_key(owner: &ToolExecutionOwner, call_id: &str) -> String {
    match owner {
        ToolExecutionOwner::Chat { message_id } => format!("chat\0{message_id}\0{call_id}"),
        ToolExecutionOwner::Subagent { subagent_run_id } => {
            format!("subagent\0{subagent_run_id}\0{call_id}")
        }
    }
}

fn lock_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!(%label, "tool execution lock was poisoned; recovering state");
        poisoned.into_inner()
    })
}

fn reject_symlink(path: &Path) -> ToolExecutionResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "tool execution root must not be a symlink: {}",
                path.display()
            ),
        )
        .into()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_manifest(path: &Path, manifest: &StoredManifest) -> ToolExecutionResult<()> {
    let bytes = serde_json::to_vec(manifest)?;
    echo_core::utils::fs::atomic_write(path, &bytes)?;
    Ok(())
}

fn read_manifest_path(path: &Path) -> ToolExecutionResult<StoredManifest> {
    let bytes = echo_core::utils::fs::read_existing(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn quarantine_manifest(path: &Path, error: &dyn std::fmt::Display) {
    let target = path.with_extension(format!("json.corrupt-{}", uuid::Uuid::new_v4()));
    match fs::rename(path, &target) {
        Ok(()) => tracing::warn!(
            path = %path.display(),
            quarantine = %target.display(),
            %error,
            "isolated unreadable tool execution manifest"
        ),
        Err(rename_error) => tracing::warn!(
            path = %path.display(),
            %error,
            %rename_error,
            "failed to isolate unreadable tool execution manifest"
        ),
    }
}

fn find_manifest_files(root: &Path) -> ToolExecutionResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                if path.extension().and_then(|value| value.to_str()) == Some("json") {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "tool execution manifest must not be a symlink: {}",
                            path.display()
                        ),
                    )
                    .into());
                }
                continue;
            }
            if metadata.is_dir() {
                if path.file_name().and_then(|value| value.to_str()) != Some(".trash") {
                    pending.push(path);
                }
            } else if metadata.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some("json")
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|value| value.to_str())
                    == Some("details")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ToolInvocationRewrite;
    use echo_agent::tools::ToolFailure;
    use echo_core::tools::ToolResultKind;

    fn chat_owner() -> ToolExecutionOwner {
        ToolExecutionOwner::Chat {
            message_id: "message-1".to_string(),
        }
    }

    fn invocation(name: &str, args: serde_json::Value) -> ToolInvocation {
        ToolInvocation {
            requested_name: name.to_string(),
            requested_args: args.clone(),
            name: name.to_string(),
            args,
            rewrites: Vec::new(),
        }
    }

    #[test]
    fn preview_is_utf8_safe() {
        let args = serde_json::json!({"text": "你🙂".repeat(200)});
        let preview = preview_args(&args);
        assert!(preview.chars().count() <= TOOL_ARGS_PREVIEW_CHARS.saturating_add(3));
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn canonical_invocation_and_rich_result_survive_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let owner = chat_owner();
        let invocation = ToolInvocation {
            requested_name: "run".to_string(),
            requested_args: serde_json::json!({"command": "build"}),
            name: "shell".to_string(),
            args: serde_json::json!({"command": "./build"}),
            rewrites: vec![ToolInvocationRewrite::Approval],
        };
        let mut result = ToolResult::success_with_kind(ToolResultKind::Json, "{\"status\":\"ok\"}");
        result.data = Some(serde_json::json!({"status": "ok"}));
        result.mime_type = Some("application/json".to_string());
        result
            .metadata
            .insert("source".to_string(), "test".to_string());

        let detail_ref = {
            let repository = ToolExecutionRepository::open(temp.path())?;
            let mutation = repository.project_start(
                owner.clone(),
                Some("conversation-1"),
                Some("run-1"),
                "call-1",
                &invocation,
            )?;
            repository.project_finish(&owner, "call-1", &result)?;
            mutation.summary.detail_ref
        };

        let reopened = ToolExecutionRepository::open(temp.path())?;
        let detail = reopened.detail_manifest(&detail_ref)?;
        assert_eq!(detail.invocation, invocation);
        assert_eq!(
            canonical_json(&detail.result)?,
            canonical_json(&Some(result))?
        );
        assert_eq!(detail.status, ToolExecutionStatus::Succeeded);
        Ok(())
    }

    #[test]
    fn typed_failure_category_controls_terminal_status() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = ToolExecutionRepository::open(temp.path())?;
        let owner = chat_owner();
        repository.project_start(
            owner.clone(),
            Some("conversation-1"),
            Some("run-1"),
            "call-timeout",
            &invocation("shell", serde_json::json!({"command": "sleep 30"})),
        )?;
        let mut result = ToolResult::success("deadline exceeded");
        result.failure = Some(ToolFailure::new(ToolFailureCategory::Timeout));

        let mutation = repository.project_finish(&owner, "call-timeout", &result)?;
        assert_eq!(mutation.summary.status, ToolExecutionStatus::TimedOut);
        Ok(())
    }

    #[test]
    fn replay_is_idempotent_but_identity_conflicts_fail_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = ToolExecutionRepository::open(temp.path())?;
        let owner = chat_owner();
        let original_invocation = invocation("shell", serde_json::json!({"command": "true"}));
        let first = repository.project_start(
            owner.clone(),
            Some("conversation-1"),
            Some("run-1"),
            "call-1",
            &original_invocation,
        )?;
        let replayed = repository.project_start(
            owner.clone(),
            Some("conversation-1"),
            Some("run-1"),
            "call-1",
            &original_invocation,
        )?;
        assert!(first.changed);
        assert!(!replayed.changed);
        assert_eq!(replayed.summary.detail_ref, first.summary.detail_ref);

        assert!(matches!(
            repository.project_start(
                owner,
                Some("conversation-1"),
                Some("run-1"),
                "call-1",
                &invocation("shell", serde_json::json!({"command": "false"})),
            ),
            Err(ToolExecutionError::ProjectionConflict(_))
        ));
        Ok(())
    }

    #[test]
    fn restart_marks_running_execution_interrupted_and_terminal_replay_corrects_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let owner = chat_owner();
        let detail_ref = {
            let repository = ToolExecutionRepository::open(temp.path())?;
            repository
                .project_start(
                    owner.clone(),
                    Some("conversation-1"),
                    Some("run-1"),
                    "call-1",
                    &invocation("shell", serde_json::json!({"command": "true"})),
                )?
                .summary
                .detail_ref
        };
        let reopened = ToolExecutionRepository::open(temp.path())?;
        assert_eq!(
            reopened.detail_manifest(&detail_ref)?.status,
            ToolExecutionStatus::Interrupted
        );
        let result = ToolResult::success("done");
        let corrected = reopened.project_finish(&owner, "call-1", &result)?;
        assert_eq!(corrected.summary.status, ToolExecutionStatus::Succeeded);
        assert_eq!(corrected.summary.detail_ref, detail_ref);
        Ok(())
    }

    #[test]
    fn compact_result_is_returned_without_a_second_cursor_protocol()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = ToolExecutionRepository::open(temp.path())?;
        let owner = chat_owner();
        let mutation = repository.project_start(
            owner.clone(),
            Some("conversation-1"),
            Some("run-1"),
            "call-1",
            &invocation("shell", serde_json::json!({"command": "printf hello"})),
        )?;
        repository.project_finish(&owner, "call-1", &ToolResult::success("hello"))?;

        let page = repository.read_output(&mutation.summary.detail_ref, None, 4)?;
        assert_eq!(
            page.chunks.first().map(|chunk| chunk.text.as_str()),
            Some("hello")
        );
        assert!(page.complete);
        assert!(page.next_cursor.is_none());
        Ok(())
    }

    #[test]
    fn running_stream_trace_is_paged_without_becoming_a_terminal_result()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = ToolExecutionRepository::open(temp.path())?;
        let owner = chat_owner();
        let mutation = repository.project_start(
            owner.clone(),
            Some("conversation-1"),
            Some("run-1"),
            "call-stream",
            &invocation("shell", serde_json::json!({"command": "build"})),
        )?;
        repository.project_stream(
            &owner,
            "call-stream",
            ToolExecutionDetailChannel::Stdout,
            "building",
        )?;

        let detail = repository.detail_manifest(&mutation.summary.detail_ref)?;
        assert_eq!(detail.status, ToolExecutionStatus::Running);
        assert!(detail.result.is_none());
        assert_eq!(detail.output_bytes, 8);

        let first = repository.read_output(&mutation.summary.detail_ref, None, 4)?;
        assert_eq!(
            first.chunks,
            vec![ToolExecutionDetailChunk {
                channel: ToolExecutionDetailChannel::Stdout,
                text: "building".to_string(),
            }]
        );
        assert!(!first.complete);
        let cursor = first.next_cursor.ok_or("running stream cursor missing")?;
        let another = repository.project_start(
            owner.clone(),
            Some("conversation-1"),
            Some("run-1"),
            "call-other",
            &invocation("shell", serde_json::json!({"command": "other"})),
        )?;
        repository.project_stream(
            &owner,
            "call-other",
            ToolExecutionDetailChannel::Stdout,
            "other",
        )?;
        assert!(matches!(
            repository.read_output(&another.summary.detail_ref, Some(&cursor), 4),
            Err(ToolExecutionError::InvalidCursor(_))
        ));

        repository.project_stream(
            &owner,
            "call-stream",
            ToolExecutionDetailChannel::Stderr,
            "warning",
        )?;
        repository.project_finish(&owner, "call-stream", &ToolResult::success("done"))?;
        repository.terminate_orphan(&owner, "call-other", ToolExecutionStatus::Unknown)?;
        let second = repository.read_output(&mutation.summary.detail_ref, Some(&cursor), 4)?;
        assert_eq!(
            second.chunks,
            vec![ToolExecutionDetailChunk {
                channel: ToolExecutionDetailChannel::Stderr,
                text: "warning".to_string(),
            }]
        );
        assert!(second.complete);
        assert!(second.next_cursor.is_none());
        assert_eq!(
            repository
                .detail_manifest(&mutation.summary.detail_ref)?
                .output_bytes,
            15
        );
        Ok(())
    }

    #[test]
    fn active_conversation_cannot_be_removed() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = ToolExecutionRepository::open(temp.path())?;
        let owner = chat_owner();
        repository.project_start(
            owner.clone(),
            Some("conversation-1"),
            Some("run-1"),
            "call-1",
            &invocation("shell", serde_json::json!({"command": "true"})),
        )?;
        assert!(matches!(
            repository.remove_conversation("conversation-1"),
            Err(ToolExecutionError::ActiveConversation(_))
        ));
        repository.terminate_orphan(&owner, "call-1", ToolExecutionStatus::Unknown)?;
        repository.remove_conversation("conversation-1")?;
        assert!(
            repository
                .summaries_for_conversation("conversation-1")
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn open_isolates_unreadable_manifest_and_keeps_repository_available()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let details = temp.path().join("scope/run/details");
        fs::create_dir_all(&details)?;
        let manifest = details.join("broken.json");
        fs::write(&manifest, b"{not-json}\n")?;

        let repository = ToolExecutionRepository::open(temp.path())?;
        assert!(
            repository
                .summaries_for_conversation("conversation")
                .is_empty()
        );
        assert!(!manifest.exists());
        assert!(fs::read_dir(details)?.any(|entry| {
            entry
                .ok()
                .and_then(|entry| entry.file_name().to_str().map(str::to_string))
                .is_some_and(|name| name.starts_with("broken.json.corrupt-"))
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn manifest_symlink_fails_closed_without_changing_external_data()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let external = temp.path().join("external.json");
        fs::write(&external, b"external")?;
        let details = temp.path().join("scope/run/details");
        fs::create_dir_all(&details)?;
        symlink(&external, details.join("detail.json"))?;

        assert!(ToolExecutionRepository::open(temp.path()).is_err());
        assert_eq!(fs::read(external)?, b"external");
        Ok(())
    }
}
