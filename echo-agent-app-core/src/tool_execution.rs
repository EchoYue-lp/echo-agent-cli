//! Application-owned durable tool-execution projection.
//!
//! Conversation messages and GUI stores keep only compact summaries. Complete
//! arguments and output stay in local files and are read lazily by `detail_ref`.

use echo_agent::tools::ToolFailure;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub const TOOL_ARGS_PREVIEW_CHARS: usize = 160;
pub const DEFAULT_DETAIL_PAGE_BYTES: usize = 64 * 1024;
// JSON escaping can expand control characters up to 6x. Keeping raw chunks at
// 8 KiB guarantees one encoded JSONL record remains below the 64 KiB page cap.
const STORED_OUTPUT_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Debug, Error)]
pub enum ToolExecutionError {
    #[error("tool execution not found: {0}")]
    NotFound(String),
    #[error("invalid detail cursor: {0}")]
    InvalidCursor(String),
    #[error("invalid UTF-8 in tool artifact: {0}")]
    InvalidUtf8(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type ToolExecutionResult<T> = Result<T, ToolExecutionError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
pub struct ToolExecutionDetailManifest {
    pub id: String,
    pub args_full: serde_json::Value,
    pub status: ToolExecutionStatus,
    pub failure: Option<ToolFailure>,
    pub metadata: HashMap<String, String>,
    pub truncated: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredManifest {
    summary: ToolExecutionSummary,
    args_full: serde_json::Value,
    failure: Option<ToolFailure>,
    metadata: HashMap<String, String>,
    truncated: bool,
    output_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalRecord {
    event: JournalEventKind,
    summary: ToolExecutionSummary,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JournalEventKind {
    Started,
    Finished,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredOutputChunk {
    channel: ToolExecutionDetailChannel,
    text: String,
}

#[derive(Debug, Clone)]
struct DetailLocation {
    manifest: PathBuf,
    output: PathBuf,
    journal: PathBuf,
}

#[derive(Debug, Clone)]
struct ActiveExecution {
    manifest: StoredManifest,
    location: DetailLocation,
    has_output: bool,
}

#[derive(Default)]
struct RepositoryState {
    active: HashMap<String, Arc<Mutex<ActiveExecution>>>,
    details: HashMap<String, DetailLocation>,
    summaries: HashMap<String, ToolExecutionSummary>,
}

pub struct ToolExecutionRepository {
    root: PathBuf,
    state: Mutex<RepositoryState>,
}

impl ToolExecutionRepository {
    pub fn open(root: impl Into<PathBuf>) -> ToolExecutionResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let repository = Self {
            root,
            state: Mutex::new(RepositoryState::default()),
        };
        repository.rebuild_index_and_recover()?;
        Ok(repository)
    }

    pub fn without_initialization(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state: Mutex::new(RepositoryState::default()),
        }
    }

    pub fn default_root() -> PathBuf {
        echo_agent::paths::user_data_path("tool-executions")
    }

    pub fn start(
        &self,
        owner: ToolExecutionOwner,
        conversation_id: Option<&str>,
        run_id: Option<&str>,
        call_id: &str,
        name: &str,
        args: &serde_json::Value,
    ) -> ToolExecutionResult<ToolExecutionSummary> {
        let detail_ref = uuid::Uuid::new_v4().to_string();
        let scope = self.scope_dir(conversation_id, run_id.or(Some(owner.key())));
        let detail_dir = scope.join("details");
        fs::create_dir_all(&detail_dir)?;
        let location = DetailLocation {
            manifest: detail_dir.join(format!("{detail_ref}.json")),
            output: detail_dir.join(format!("{detail_ref}.jsonl")),
            journal: scope.join("events.jsonl"),
        };
        let now = now_millis();
        let summary = ToolExecutionSummary {
            id: detail_ref.clone(),
            call_id: call_id.to_string(),
            owner,
            conversation_id: conversation_id.map(str::to_string),
            run_id: run_id.map(str::to_string),
            name: name.to_string(),
            args_preview: preview_args(args),
            status: ToolExecutionStatus::Running,
            started_at: now,
            finished_at: None,
            duration_ms: None,
            detail_ref: detail_ref.clone(),
        };
        let manifest = StoredManifest {
            summary: summary.clone(),
            args_full: args.clone(),
            failure: None,
            metadata: HashMap::new(),
            truncated: false,
            output_bytes: 0,
        };
        write_json_atomic(&location.manifest, &manifest)?;
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&location.output)?;
        append_json_line(
            &location.journal,
            &JournalRecord {
                event: JournalEventKind::Started,
                summary: summary.clone(),
            },
        )?;

        let key = execution_key(&summary.owner, call_id);
        let mut state = self.lock_state();
        state.details.insert(detail_ref, location.clone());
        state.summaries.insert(key.clone(), summary.clone());
        state.active.insert(
            key,
            Arc::new(Mutex::new(ActiveExecution {
                manifest,
                location,
                has_output: false,
            })),
        );
        Ok(summary)
    }

    pub fn append_output(
        &self,
        owner: &ToolExecutionOwner,
        call_id: &str,
        channel: ToolExecutionDetailChannel,
        text: &str,
    ) -> ToolExecutionResult<()> {
        let key = execution_key(owner, call_id);
        let active = self
            .lock_state()
            .active
            .get(&key)
            .cloned()
            .ok_or_else(|| ToolExecutionError::NotFound(call_id.to_string()))?;
        let mut execution = lock_recover(&active, "active tool execution");
        for chunk in split_utf8_chunks(text, STORED_OUTPUT_CHUNK_BYTES) {
            append_json_line(
                &execution.location.output,
                &StoredOutputChunk {
                    channel: channel.clone(),
                    text: chunk,
                },
            )?;
        }
        execution.manifest.output_bytes = execution
            .manifest
            .output_bytes
            .saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX));
        execution.has_output |= !text.is_empty();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &self,
        owner: &ToolExecutionOwner,
        call_id: &str,
        success: bool,
        result: &str,
        failure: Option<ToolFailure>,
        metadata: HashMap<String, String>,
        truncated: bool,
    ) -> ToolExecutionResult<ToolExecutionSummary> {
        let key = execution_key(owner, call_id);
        let active = self
            .lock_state()
            .active
            .get(&key)
            .cloned()
            .ok_or_else(|| ToolExecutionError::NotFound(call_id.to_string()))?;
        let mut execution = lock_recover(&active, "active tool execution");

        let artifact_available = metadata
            .get("artifact_path")
            .map(Path::new)
            .is_some_and(Path::is_file);
        if !execution.has_output && !artifact_available && !result.is_empty() {
            for chunk in split_utf8_chunks(result, STORED_OUTPUT_CHUNK_BYTES) {
                append_json_line(
                    &execution.location.output,
                    &StoredOutputChunk {
                        channel: ToolExecutionDetailChannel::Result,
                        text: chunk,
                    },
                )?;
            }
            execution.manifest.output_bytes = execution
                .manifest
                .output_bytes
                .saturating_add(u64::try_from(result.len()).unwrap_or(u64::MAX));
        }

        let finished_at = now_millis();
        execution.manifest.summary.status = if success {
            ToolExecutionStatus::Succeeded
        } else {
            ToolExecutionStatus::Failed
        };
        execution.manifest.summary.finished_at = Some(finished_at);
        execution.manifest.summary.duration_ms =
            Some(finished_at.saturating_sub(execution.manifest.summary.started_at));
        execution.manifest.failure = failure;
        execution.manifest.metadata = metadata;
        execution.manifest.truncated = truncated;
        write_json_atomic(&execution.location.manifest, &execution.manifest)?;
        append_json_line(
            &execution.location.journal,
            &JournalRecord {
                event: JournalEventKind::Finished,
                summary: execution.manifest.summary.clone(),
            },
        )?;
        let summary = execution.manifest.summary.clone();
        drop(execution);
        let mut state = self.lock_state();
        state.active.remove(&key);
        state.summaries.insert(key, summary.clone());
        Ok(summary)
    }

    pub fn cancel(
        &self,
        owner: &ToolExecutionOwner,
        call_id: &str,
    ) -> ToolExecutionResult<ToolExecutionSummary> {
        let key = execution_key(owner, call_id);
        let active = self
            .lock_state()
            .active
            .get(&key)
            .cloned()
            .ok_or_else(|| ToolExecutionError::NotFound(call_id.to_string()))?;
        let mut execution = lock_recover(&active, "active tool execution");
        let finished_at = now_millis();
        execution.manifest.summary.status = ToolExecutionStatus::Cancelled;
        execution.manifest.summary.finished_at = Some(finished_at);
        execution.manifest.summary.duration_ms =
            Some(finished_at.saturating_sub(execution.manifest.summary.started_at));
        write_json_atomic(&execution.location.manifest, &execution.manifest)?;
        append_json_line(
            &execution.location.journal,
            &JournalRecord {
                event: JournalEventKind::Cancelled,
                summary: execution.manifest.summary.clone(),
            },
        )?;
        let summary = execution.manifest.summary.clone();
        drop(execution);
        let mut state = self.lock_state();
        state.active.remove(&key);
        state.summaries.insert(key, summary.clone());
        Ok(summary)
    }

    pub fn detail_manifest(
        &self,
        detail_ref: &str,
    ) -> ToolExecutionResult<ToolExecutionDetailManifest> {
        let location = self.detail_location(detail_ref)?;
        let manifest: StoredManifest = read_json(&location.manifest)?;
        let mut metadata = manifest.metadata;
        metadata.remove("artifact_path");
        Ok(ToolExecutionDetailManifest {
            id: manifest.summary.id,
            args_full: manifest.args_full,
            status: manifest.summary.status,
            failure: manifest.failure,
            metadata,
            truncated: manifest.truncated,
            output_bytes: manifest.output_bytes,
        })
    }

    pub fn read_output(
        &self,
        detail_ref: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> ToolExecutionResult<ToolExecutionDetailPage> {
        let location = self.detail_location(detail_ref)?;
        let manifest: StoredManifest = read_json(&location.manifest)?;
        let page_bytes = limit.clamp(4, DEFAULT_DETAIL_PAGE_BYTES);
        if fs::metadata(&location.output)
            .map(|value| value.len())
            .unwrap_or(0)
            > 0
        {
            return read_jsonl_output_page(
                &location.output,
                cursor,
                page_bytes,
                manifest.summary.status != ToolExecutionStatus::Running,
            );
        }
        if let Some(path) = manifest.metadata.get("artifact_path") {
            let artifact = Path::new(path);
            if artifact.is_file() {
                return read_artifact_page(
                    artifact,
                    cursor,
                    page_bytes,
                    manifest.summary.status != ToolExecutionStatus::Running,
                );
            }
        }
        Ok(ToolExecutionDetailPage {
            chunks: Vec::new(),
            next_cursor: cursor.map(str::to_string),
            complete: manifest.summary.status != ToolExecutionStatus::Running,
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

    pub fn remove_conversation(&self, conversation_id: &str) -> ToolExecutionResult<()> {
        let scope = self
            .root
            .join(echo_agent::tools::artifact::artifact_scope_component(
                conversation_id,
            ));
        if !scope.exists() {
            return Ok(());
        }
        let trash_root = self.root.join(".trash");
        fs::create_dir_all(&trash_root)?;
        let tombstone = trash_root.join(format!(
            "{}-{}",
            echo_agent::tools::artifact::artifact_scope_component(conversation_id),
            uuid::Uuid::new_v4()
        ));
        fs::rename(&scope, &tombstone)?;
        {
            let mut state = self.lock_state();
            state
                .summaries
                .retain(|_, summary| summary.conversation_id.as_deref() != Some(conversation_id));
            state.details.retain(|_, location| {
                !location.manifest.starts_with(&scope)
                    && !location.output.starts_with(&scope)
                    && !location.journal.starts_with(&scope)
            });
            state.active.retain(|_, execution| {
                lock_recover(execution, "active tool execution")
                    .manifest
                    .summary
                    .conversation_id
                    .as_deref()
                    != Some(conversation_id)
            });
        }
        std::thread::spawn(move || {
            if let Err(error) = fs::remove_dir_all(&tombstone) {
                tracing::warn!(path = %tombstone.display(), %error, "tool execution tombstone cleanup failed");
            }
        });
        Ok(())
    }

    fn rebuild_index_and_recover(&self) -> ToolExecutionResult<()> {
        let journals = find_named_files(&self.root, "events.jsonl")?;
        for journal in journals {
            let records = read_journal_repairing_last_line(&journal)?;
            let mut latest = HashMap::<String, ToolExecutionSummary>::new();
            for record in records {
                let key = execution_key(&record.summary.owner, &record.summary.call_id);
                latest.insert(key, record.summary);
            }
            for (key, mut summary) in latest {
                let detail_dir = journal
                    .parent()
                    .map(|parent| parent.join("details"))
                    .unwrap_or_else(|| self.root.join("details"));
                let location = DetailLocation {
                    manifest: detail_dir.join(format!("{}.json", summary.detail_ref)),
                    output: detail_dir.join(format!("{}.jsonl", summary.detail_ref)),
                    journal: journal.clone(),
                };
                if summary.status == ToolExecutionStatus::Running {
                    let finished_at = now_millis();
                    summary.status = ToolExecutionStatus::Cancelled;
                    summary.finished_at = Some(finished_at);
                    summary.duration_ms = Some(finished_at.saturating_sub(summary.started_at));
                    if location.manifest.is_file()
                        && let Ok(mut manifest) = read_json::<StoredManifest>(&location.manifest)
                    {
                        manifest.summary = summary.clone();
                        write_json_atomic(&location.manifest, &manifest)?;
                    }
                    append_json_line(
                        &journal,
                        &JournalRecord {
                            event: JournalEventKind::Cancelled,
                            summary: summary.clone(),
                        },
                    )?;
                }
                let mut state = self.lock_state();
                state.details.insert(summary.detail_ref.clone(), location);
                state.summaries.insert(key, summary);
            }
        }
        Ok(())
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

    fn lock_state(&self) -> MutexGuard<'_, RepositoryState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("tool execution repository lock was poisoned; recovering state");
            poisoned.into_inner()
        })
    }
}

pub fn preview_args(args: &serde_json::Value) -> String {
    let serialized = serde_json::to_string(args).unwrap_or_else(|_| String::new());
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

fn execution_key(owner: &ToolExecutionOwner, call_id: &str) -> String {
    match owner {
        ToolExecutionOwner::Chat { message_id } => format!("chat\0{message_id}\0{call_id}"),
        ToolExecutionOwner::Subagent { subagent_run_id } => {
            format!("subagent\0{subagent_run_id}\0{call_id}")
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn lock_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!(%label, "tool execution lock was poisoned; recovering state");
        poisoned.into_inner()
    })
}

fn split_utf8_chunks(value: &str, max_bytes: usize) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if !current.is_empty()
            && current.len().saturating_add(character.len_utf8()) > max_bytes.max(1)
        {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(character);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn append_json_line<T: Serialize>(path: &Path, value: &T) -> ToolExecutionResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> ToolExecutionResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(value)?;
    {
        let mut file = File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(temp, path)?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> ToolExecutionResult<T> {
    let file = File::open(path)?;
    Ok(serde_json::from_reader(BufReader::new(file))?)
}

fn parse_cursor(cursor: Option<&str>) -> ToolExecutionResult<u64> {
    match cursor {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| ToolExecutionError::InvalidCursor(value.to_string())),
        None => Ok(0),
    }
}

fn read_jsonl_output_page(
    path: &Path,
    cursor: Option<&str>,
    limit: usize,
    terminal: bool,
) -> ToolExecutionResult<ToolExecutionDetailPage> {
    let start = parse_cursor(cursor)?;
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(start))?;
    let mut consumed = 0usize;
    let mut line = String::new();
    let mut chunks = Vec::new();
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        if consumed > 0 && consumed.saturating_add(bytes) > limit {
            reader.seek(SeekFrom::Current(-i64::try_from(bytes).unwrap_or(i64::MAX)))?;
            break;
        }
        consumed = consumed.saturating_add(bytes);
        if !line.trim().is_empty() {
            let stored: StoredOutputChunk = serde_json::from_str(&line)?;
            chunks.push(ToolExecutionDetailChunk {
                channel: stored.channel,
                text: stored.text,
            });
        }
        if consumed >= limit {
            break;
        }
    }
    let next = reader.stream_position()?;
    let end = fs::metadata(path)?.len();
    Ok(ToolExecutionDetailPage {
        chunks,
        next_cursor: (next < end || !terminal).then(|| next.to_string()),
        complete: terminal && next >= end,
    })
}

fn read_artifact_page(
    path: &Path,
    cursor: Option<&str>,
    limit: usize,
    terminal: bool,
) -> ToolExecutionResult<ToolExecutionDetailPage> {
    let start = parse_cursor(cursor)?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let page_limit = limit.max(4);
    let mut bytes = Vec::with_capacity(page_limit);
    let mut limited = file.take(u64::try_from(page_limit).unwrap_or(u64::MAX));
    limited.read_to_end(&mut bytes)?;
    let (text, read_bytes) = utf8_page_prefix(bytes)?;
    let next = start.saturating_add(u64::try_from(read_bytes).unwrap_or(u64::MAX));
    let end = fs::metadata(path)?.len();
    Ok(ToolExecutionDetailPage {
        chunks: if text.is_empty() {
            Vec::new()
        } else {
            vec![ToolExecutionDetailChunk {
                channel: ToolExecutionDetailChannel::Log,
                text,
            }]
        },
        next_cursor: (next < end || !terminal).then(|| next.to_string()),
        complete: terminal && next >= end,
    })
}

fn utf8_page_prefix(mut bytes: Vec<u8>) -> ToolExecutionResult<(String, usize)> {
    let mut removed = 0usize;
    loop {
        match String::from_utf8(bytes) {
            Ok(text) => {
                let consumed = text.len();
                return Ok((text, consumed));
            }
            Err(error) if removed < 3 => {
                bytes = error.into_bytes();
                if bytes.pop().is_none() {
                    return Ok((String::new(), 0));
                }
                removed = removed.saturating_add(1);
            }
            Err(error) => return Err(ToolExecutionError::InvalidUtf8(error.to_string())),
        }
    }
}

fn read_journal_repairing_last_line(path: &Path) -> ToolExecutionResult<Vec<JournalRecord>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut records = Vec::new();
    let mut line = String::new();
    let mut offset = 0u64;
    let mut last_good_offset = 0u64;
    let mut truncate_to = None;
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        offset = offset.saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        if line.trim().is_empty() {
            last_good_offset = offset;
            continue;
        }
        match serde_json::from_str::<JournalRecord>(&line) {
            Ok(record) => {
                records.push(record);
                last_good_offset = offset;
            }
            Err(error) => {
                if reader.fill_buf()?.is_empty() {
                    tracing::warn!(path = %path.display(), %error, "discarding malformed final tool journal line");
                    truncate_to = Some(last_good_offset);
                    break;
                }
                return Err(error.into());
            }
        }
    }
    drop(reader);
    if let Some(length) = truncate_to {
        OpenOptions::new().write(true).open(path)?.set_len(length)?;
    }
    Ok(records)
}

fn find_named_files(root: &Path, name: &str) -> ToolExecutionResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|value| value.to_str()) != Some(".trash") {
                    pending.push(path);
                }
            } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
                files.push(path);
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_owner() -> ToolExecutionOwner {
        ToolExecutionOwner::Chat {
            message_id: "message-1".to_string(),
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
    fn complete_output_is_read_in_pages() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = ToolExecutionRepository::open(temp.path())?;
        let owner = chat_owner();
        let summary = repository.start(
            owner.clone(),
            Some("conversation-1"),
            Some("run-1"),
            "call-1",
            "shell",
            &serde_json::json!({"command": "printf hello"}),
        )?;
        repository.append_output(
            &owner,
            "call-1",
            ToolExecutionDetailChannel::Stdout,
            "hello world",
        )?;
        repository.finish(
            &owner,
            "call-1",
            true,
            "hello world",
            None,
            HashMap::new(),
            false,
        )?;

        let page = repository.read_output(&summary.detail_ref, None, 64)?;
        assert_eq!(
            page.chunks.first().map(|chunk| chunk.text.as_str()),
            Some("hello world")
        );
        assert!(page.complete);
        Ok(())
    }

    #[test]
    fn malformed_final_journal_line_is_removed() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let journal = temp.path().join("events.jsonl");
        let summary = ToolExecutionSummary {
            id: "detail-1".to_string(),
            call_id: "call-1".to_string(),
            owner: chat_owner(),
            conversation_id: Some("conversation-1".to_string()),
            run_id: Some("run-1".to_string()),
            name: "shell".to_string(),
            args_preview: "{}".to_string(),
            status: ToolExecutionStatus::Succeeded,
            started_at: 1,
            finished_at: Some(2),
            duration_ms: Some(1),
            detail_ref: "detail-1".to_string(),
        };
        append_json_line(
            &journal,
            &JournalRecord {
                event: JournalEventKind::Finished,
                summary,
            },
        )?;
        OpenOptions::new()
            .append(true)
            .open(&journal)?
            .write_all(b"{partial")?;

        let records = read_journal_repairing_last_line(&journal)?;
        assert_eq!(records.len(), 1);
        let repaired = fs::read_to_string(journal)?;
        assert!(!repaired.contains("partial"));
        Ok(())
    }

    #[test]
    fn removing_conversation_drops_summary_and_detail_indexes()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repository = ToolExecutionRepository::open(temp.path())?;
        let owner = chat_owner();
        let summary = repository.start(
            owner.clone(),
            Some("conversation-1"),
            Some("run-1"),
            "call-1",
            "shell",
            &serde_json::json!({"command": "true"}),
        )?;
        repository.finish(&owner, "call-1", true, "ok", None, HashMap::new(), false)?;

        repository.remove_conversation("conversation-1")?;

        assert!(
            repository
                .summaries_for_conversation("conversation-1")
                .is_empty()
        );
        assert!(matches!(
            repository.detail_manifest(&summary.detail_ref),
            Err(ToolExecutionError::NotFound(_))
        ));
        Ok(())
    }

    #[test]
    fn artifact_pages_end_on_utf8_boundaries() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let artifact = temp.path().join("artifact.log");
        fs::write(&artifact, "你🙂好")?;

        let first = read_artifact_page(&artifact, None, 1, true)?;
        assert_eq!(
            first.chunks.first().map(|chunk| chunk.text.as_str()),
            Some("你")
        );
        let cursor = first
            .next_cursor
            .as_deref()
            .ok_or_else(|| "missing next cursor".to_string())?;
        let second = read_artifact_page(&artifact, Some(cursor), 8, true)?;
        assert_eq!(
            second.chunks.first().map(|chunk| chunk.text.as_str()),
            Some("🙂好")
        );
        assert!(second.complete);
        Ok(())
    }
}
