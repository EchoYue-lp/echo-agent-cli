//! Incremental, discardable TaskRuntime history read models.
//!
//! The journal remains the sole authority. These files carry the source
//! journal sequence, have no mutation API or independent sequence, and may be
//! deleted and rebuilt at any time.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::event_rebuild::{artifact_from_event, review_from_event};
use super::types::{Artifact, ReviewResult, RuntimeTaskEvent};

const HISTORY_SCHEMA: u8 = 1;
const ARTIFACT_FILE: &str = "artifact-history.jsonl";
const ARTIFACT_METADATA_FILE: &str = "artifact-history.meta.jsonl";
const REVIEW_DIRECTORY: &str = "review-history";
const CURSOR_FILE: &str = "history-cursor.json";
const EMPTY_HASH_CHAIN: &str = "taskruntime-history-v1";
const MAX_REVIEW_FALLBACK_TASKS: usize = 8;
const MAX_REVIEW_FALLBACK_RECORDS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HistoryProjectionApplyStatus {
    Current,
    Degraded { error: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryCursor {
    schema_version: u8,
    through_sequence: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRecord {
    source_sequence: u64,
    artifact: Artifact,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewRecord {
    source_sequence: u64,
    task_id: String,
    review: ReviewResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentMetadata {
    schema_version: u8,
    record_count: u64,
    last_source_sequence: u64,
    batch_record_count: u64,
    hash_chain: String,
}

impl SegmentMetadata {
    fn initial() -> Self {
        Self {
            schema_version: HISTORY_SCHEMA,
            record_count: 0,
            last_source_sequence: 0,
            batch_record_count: 0,
            hash_chain: hex::encode(Sha256::digest(EMPTY_HASH_CHAIN.as_bytes())),
        }
    }

    fn hash_bytes(&self) -> Result<[u8; 32], String> {
        let decoded = hex::decode(&self.hash_chain).map_err(|error| error.to_string())?;
        <[u8; 32]>::try_from(decoded.as_slice())
            .map_err(|_| "history segment hash-chain length mismatch".to_string())
    }
}

trait HistoryRecord: Serialize {
    fn source_sequence(&self) -> u64;
}

struct CachedReviewFallback {
    journal_head: u64,
    reviews: Vec<ReviewResult>,
    last_access: u64,
}

impl HistoryRecord for ArtifactRecord {
    fn source_sequence(&self) -> u64 {
        self.source_sequence
    }
}

impl HistoryRecord for ReviewRecord {
    fn source_sequence(&self) -> u64 {
        self.source_sequence
    }
}

pub(crate) struct HistoryProjection {
    run_id: String,
    run_directory: PathBuf,
    through_sequence: u64,
    needs_full_rebuild: bool,
    artifact_metadata: Option<SegmentMetadata>,
    review_metadata: BTreeMap<String, SegmentMetadata>,
    artifact_fallback: Option<(u64, Vec<Artifact>)>,
    review_fallbacks: BTreeMap<String, CachedReviewFallback>,
    review_fallback_records: usize,
    fallback_access_clock: u64,
    #[cfg(test)]
    fail_next_review_append: bool,
    #[cfg(test)]
    suppress_next_auto_rebuild: bool,
    #[cfg(test)]
    fail_cursor_writes_remaining: usize,
    #[cfg(test)]
    segment_scan_count: usize,
    #[cfg(test)]
    segment_appended_bytes: u64,
    #[cfg(test)]
    fallback_replay_count: usize,
}

impl HistoryProjection {
    pub(crate) fn open(run_id: &str, run_directory: &Path, journal_head: u64) -> Self {
        let cursor_path = run_directory.join(CURSOR_FILE);
        let cursor = std::fs::read(&cursor_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<HistoryCursor>(&bytes).ok())
            .filter(|cursor| {
                cursor.schema_version == HISTORY_SCHEMA && cursor.through_sequence <= journal_head
            });
        Self {
            run_id: run_id.to_string(),
            run_directory: run_directory.to_path_buf(),
            through_sequence: cursor.as_ref().map_or(0, |cursor| cursor.through_sequence),
            needs_full_rebuild: cursor.is_none() && journal_head != 0,
            artifact_metadata: None,
            review_metadata: BTreeMap::new(),
            artifact_fallback: None,
            review_fallbacks: BTreeMap::new(),
            review_fallback_records: 0,
            fallback_access_clock: 0,
            #[cfg(test)]
            fail_next_review_append: false,
            #[cfg(test)]
            suppress_next_auto_rebuild: false,
            #[cfg(test)]
            fail_cursor_writes_remaining: 0,
            #[cfg(test)]
            segment_scan_count: 0,
            #[cfg(test)]
            segment_appended_bytes: 0,
            #[cfg(test)]
            fallback_replay_count: 0,
        }
    }

    pub(crate) fn through_sequence(&self) -> u64 {
        self.through_sequence
    }

    pub(crate) fn needs_full_rebuild(&self) -> bool {
        self.needs_full_rebuild
    }

    pub(crate) fn apply_events(
        &mut self,
        events: &[RuntimeTaskEvent],
        through_sequence: u64,
    ) -> HistoryProjectionApplyStatus {
        if events.is_empty() && through_sequence == self.through_sequence {
            return HistoryProjectionApplyStatus::Current;
        }
        self.artifact_fallback = None;
        self.review_fallbacks.clear();
        self.review_fallback_records = 0;
        match self.apply_events_inner(events, through_sequence) {
            Ok(()) => HistoryProjectionApplyStatus::Current,
            Err(error) => {
                #[cfg(test)]
                let suppress_auto_rebuild = std::mem::take(&mut self.suppress_next_auto_rebuild);
                #[cfg(not(test))]
                let suppress_auto_rebuild = false;
                if !suppress_auto_rebuild {
                    self.needs_full_rebuild = true;
                }
                HistoryProjectionApplyStatus::Degraded { error }
            }
        }
    }

    fn apply_events_inner(
        &mut self,
        events: &[RuntimeTaskEvent],
        through_sequence: u64,
    ) -> Result<(), String> {
        let mut artifacts = Vec::new();
        let mut reviews = BTreeMap::<String, Vec<ReviewRecord>>::new();
        for event in events {
            let source_sequence = u64::try_from(event.seq)
                .map_err(|_| format!("invalid history source sequence {}", event.seq))?;
            if let Some(artifact) = artifact_from_event(event) {
                artifacts.push(ArtifactRecord {
                    source_sequence,
                    artifact,
                });
            }
            if let Some(review) = review_from_event(event) {
                let task_id = review.task_id.clone();
                reviews
                    .entry(task_id.clone())
                    .or_default()
                    .push(ReviewRecord {
                        source_sequence,
                        task_id,
                        review,
                    });
            }
        }

        if !self.artifact_path().exists() {
            if self.through_sequence != 0 {
                return Err("artifact history segment is missing behind its cursor".to_string());
            }
            self.artifact_metadata = Some(replace_records::<ArtifactRecord>(
                &self.artifact_path(),
                &self.artifact_metadata_path(),
                &[],
            )?);
        }
        if !artifacts.is_empty() {
            let path = self.artifact_path();
            let metadata = match self.artifact_metadata.clone() {
                Some(metadata) => metadata,
                None => self.scan_artifacts()?.1,
            };
            let pending = artifacts
                .iter()
                .filter(|record| record.source_sequence > metadata.last_source_sequence)
                .collect::<Vec<_>>();
            #[cfg(test)]
            let appended_bytes = encoded_records_len(&pending)?;
            self.artifact_metadata = Some(append_records(
                &path,
                &self.artifact_metadata_path(),
                &pending,
                metadata,
            )?);
            #[cfg(test)]
            {
                self.segment_appended_bytes = self
                    .segment_appended_bytes
                    .checked_add(appended_bytes)
                    .ok_or_else(|| "history test byte counter overflow".to_string())?;
            }
        }

        for (task_id, records) in reviews {
            #[cfg(test)]
            if std::mem::take(&mut self.fail_next_review_append) {
                self.suppress_next_auto_rebuild = true;
                return Err("injected review history append failure".to_string());
            }
            let path = self.review_path(&task_id);
            if !path.exists() && self.through_sequence != 0 {
                return Err(format!(
                    "review history segment for task '{}' is missing behind its cursor",
                    task_id
                ));
            }
            if !path.exists() {
                let metadata_path = self.review_metadata_path(&task_id);
                self.review_metadata.insert(
                    task_id.clone(),
                    replace_records::<ReviewRecord>(&path, &metadata_path, &[])?,
                );
            }
            let metadata = match self.review_metadata.get(&task_id).cloned() {
                Some(metadata) => metadata,
                None => self.scan_reviews(&task_id)?.1,
            };
            let pending = records
                .iter()
                .filter(|record| record.source_sequence > metadata.last_source_sequence)
                .collect::<Vec<_>>();
            let metadata_path = self.review_metadata_path(&task_id);
            #[cfg(test)]
            let appended_bytes = encoded_records_len(&pending)?;
            self.review_metadata.insert(
                task_id,
                append_records(&path, &metadata_path, &pending, metadata)?,
            );
            #[cfg(test)]
            {
                self.segment_appended_bytes = self
                    .segment_appended_bytes
                    .checked_add(appended_bytes)
                    .ok_or_else(|| "history test byte counter overflow".to_string())?;
            }
        }

        self.write_cursor(through_sequence)?;
        self.through_sequence = through_sequence;
        self.needs_full_rebuild = false;
        Ok(())
    }

    pub(crate) fn rebuild_all(
        &mut self,
        events: &[RuntimeTaskEvent],
        through_sequence: u64,
    ) -> HistoryProjectionApplyStatus {
        match self.rebuild_all_inner(events, through_sequence) {
            Ok(()) => HistoryProjectionApplyStatus::Current,
            Err(error) => HistoryProjectionApplyStatus::Degraded { error },
        }
    }

    fn rebuild_all_inner(
        &mut self,
        events: &[RuntimeTaskEvent],
        through_sequence: u64,
    ) -> Result<(), String> {
        let artifacts = project_artifact_records(events)?;
        let reviews = project_review_records(events)?;
        let review_directory = self.run_directory.join(REVIEW_DIRECTORY);
        match std::fs::remove_dir_all(&review_directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        self.artifact_metadata = Some(replace_records(
            &self.artifact_path(),
            &self.artifact_metadata_path(),
            &artifacts,
        )?);
        self.review_metadata.clear();
        for (task_id, records) in &reviews {
            self.review_metadata.insert(
                task_id.clone(),
                replace_records(
                    &self.review_path(task_id),
                    &self.review_metadata_path(task_id),
                    records,
                )?,
            );
        }
        self.write_cursor(through_sequence)?;
        self.through_sequence = through_sequence;
        self.needs_full_rebuild = false;
        self.artifact_fallback = None;
        self.review_fallbacks.clear();
        self.review_fallback_records = 0;
        Ok(())
    }

    pub(crate) fn artifacts_with_suffix(
        &mut self,
        suffix: &[RuntimeTaskEvent],
    ) -> Result<Vec<Artifact>, String> {
        let (mut artifacts, metadata) = self.scan_artifacts()?;
        artifacts.extend(suffix.iter().filter_map(|event| {
            (u64::try_from(event.seq).ok()? > metadata.last_source_sequence)
                .then(|| artifact_from_event(event))
                .flatten()
        }));
        self.artifact_metadata = Some(metadata);
        Ok(artifacts)
    }

    pub(crate) fn reviews_with_suffix(
        &mut self,
        task_id: &str,
        suffix: &[RuntimeTaskEvent],
    ) -> Result<Vec<ReviewResult>, String> {
        let (mut reviews, metadata) = self.scan_reviews(task_id)?;
        reviews.extend(suffix.iter().filter_map(|event| {
            (u64::try_from(event.seq).ok()? > metadata.last_source_sequence
                && event.task_id.as_deref() == Some(task_id))
            .then(|| review_from_event(event))
            .flatten()
        }));
        self.review_metadata.insert(task_id.to_string(), metadata);
        Ok(reviews)
    }

    pub(crate) fn replace_artifacts(
        &mut self,
        events: &[RuntimeTaskEvent],
    ) -> Result<Vec<Artifact>, String> {
        let records = project_artifact_records(events)?;
        self.artifact_metadata = Some(replace_records(
            &self.artifact_path(),
            &self.artifact_metadata_path(),
            &records,
        )?);
        Ok(records.into_iter().map(|record| record.artifact).collect())
    }

    pub(crate) fn replace_reviews(
        &mut self,
        task_id: &str,
        events: &[RuntimeTaskEvent],
    ) -> Result<Vec<ReviewResult>, String> {
        let records = project_review_records(events)?
            .remove(task_id)
            .unwrap_or_default();
        let metadata = replace_records(
            &self.review_path(task_id),
            &self.review_metadata_path(task_id),
            &records,
        )?;
        self.review_metadata.insert(task_id.to_string(), metadata);
        Ok(records.into_iter().map(|record| record.review).collect())
    }

    fn scan_artifacts(&mut self) -> Result<(Vec<Artifact>, SegmentMetadata), String> {
        #[cfg(test)]
        {
            self.segment_scan_count = self.segment_scan_count.saturating_add(1);
        }
        if !self.artifact_path().exists() {
            return Err("artifact history segment is missing".to_string());
        }
        let (records, metadata) = read_validated_records::<ArtifactRecord>(
            &self.artifact_path(),
            &self.artifact_metadata_path(),
        )?;
        validate_increasing(records.iter().map(|record| record.source_sequence))?;
        if records
            .iter()
            .any(|record| record.artifact.run_id != self.run_id)
        {
            return Err("artifact history contains another TaskRun".to_string());
        }
        Ok((
            records.into_iter().map(|record| record.artifact).collect(),
            metadata,
        ))
    }

    fn scan_reviews(
        &mut self,
        task_id: &str,
    ) -> Result<(Vec<ReviewResult>, SegmentMetadata), String> {
        #[cfg(test)]
        {
            self.segment_scan_count = self.segment_scan_count.saturating_add(1);
        }
        if !self.review_path(task_id).exists() {
            return Err(format!(
                "review history segment for task '{task_id}' is missing"
            ));
        }
        let (records, metadata) = read_validated_records::<ReviewRecord>(
            &self.review_path(task_id),
            &self.review_metadata_path(task_id),
        )?;
        validate_increasing(records.iter().map(|record| record.source_sequence))?;
        if records.iter().any(|record| {
            record.task_id != task_id
                || record.review.task_id != task_id
                || record.review.run_id != self.run_id
        }) {
            return Err("review history identity mismatch".to_string());
        }
        Ok((
            records.into_iter().map(|record| record.review).collect(),
            metadata,
        ))
    }

    fn artifact_path(&self) -> PathBuf {
        self.run_directory.join(ARTIFACT_FILE)
    }

    fn artifact_metadata_path(&self) -> PathBuf {
        self.run_directory.join(ARTIFACT_METADATA_FILE)
    }

    fn review_path(&self, task_id: &str) -> PathBuf {
        let encoded = echo_agent::utils::fs::encode_utf8_path_identity(task_id);
        self.run_directory
            .join(REVIEW_DIRECTORY)
            .join(format!("{encoded}.jsonl"))
    }

    fn review_metadata_path(&self, task_id: &str) -> PathBuf {
        let encoded = echo_agent::utils::fs::encode_utf8_path_identity(task_id);
        self.run_directory
            .join(REVIEW_DIRECTORY)
            .join(format!("{encoded}.meta.jsonl"))
    }

    fn write_cursor(&mut self, through_sequence: u64) -> Result<(), String> {
        #[cfg(test)]
        if let Some(remaining) = self.fail_cursor_writes_remaining.checked_sub(1) {
            self.fail_cursor_writes_remaining = remaining;
            return Err("injected history cursor write failure".to_string());
        }
        let bytes = serde_json::to_vec(&HistoryCursor {
            schema_version: HISTORY_SCHEMA,
            through_sequence,
        })
        .map_err(|error| error.to_string())?;
        echo_agent::utils::fs::atomic_write(&self.run_directory.join(CURSOR_FILE), &bytes)
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_review_append_for_test(&mut self) {
        self.fail_next_review_append = true;
    }

    #[cfg(test)]
    pub(crate) fn fail_cursor_writes_for_test(&mut self, count: usize) {
        self.fail_cursor_writes_remaining = count;
    }

    #[cfg(test)]
    pub(crate) fn stats_for_test(&self) -> (usize, u64) {
        (self.segment_scan_count, self.segment_appended_bytes)
    }

    pub(crate) fn cached_artifacts(&self, journal_head: u64) -> Option<Vec<Artifact>> {
        self.artifact_fallback
            .as_ref()
            .filter(|(sequence, _)| *sequence == journal_head)
            .map(|(_, artifacts)| artifacts.clone())
    }

    pub(crate) fn cache_artifacts(&mut self, journal_head: u64, artifacts: Vec<Artifact>) {
        self.artifact_fallback = Some((journal_head, artifacts));
    }

    pub(crate) fn cached_reviews(
        &mut self,
        task_id: &str,
        journal_head: u64,
    ) -> Option<Vec<ReviewResult>> {
        if self
            .review_fallbacks
            .get(task_id)
            .is_some_and(|entry| entry.journal_head != journal_head)
        {
            let removed = self.review_fallbacks.remove(task_id)?;
            self.review_fallback_records = self
                .review_fallback_records
                .saturating_sub(removed.reviews.len());
            return None;
        }
        self.fallback_access_clock = self.fallback_access_clock.saturating_add(1);
        let entry = self.review_fallbacks.get_mut(task_id)?;
        entry.last_access = self.fallback_access_clock;
        Some(entry.reviews.clone())
    }

    pub(crate) fn cache_reviews(
        &mut self,
        task_id: &str,
        journal_head: u64,
        reviews: Vec<ReviewResult>,
    ) {
        if let Some(previous) = self.review_fallbacks.remove(task_id) {
            self.review_fallback_records = self
                .review_fallback_records
                .saturating_sub(previous.reviews.len());
        }
        if reviews.len() > MAX_REVIEW_FALLBACK_RECORDS {
            self.review_fallbacks.clear();
            self.review_fallback_records = 0;
            return;
        }
        while self.review_fallbacks.len() >= MAX_REVIEW_FALLBACK_TASKS
            || self.review_fallback_records.saturating_add(reviews.len())
                > MAX_REVIEW_FALLBACK_RECORDS
        {
            let Some(evicted_task) = self
                .review_fallbacks
                .iter()
                .min_by_key(|(_, cached)| cached.last_access)
                .map(|(task_id, _)| task_id.clone())
            else {
                break;
            };
            if let Some(evicted) = self.review_fallbacks.remove(&evicted_task) {
                self.review_fallback_records = self
                    .review_fallback_records
                    .saturating_sub(evicted.reviews.len());
            }
        }
        self.fallback_access_clock = self.fallback_access_clock.saturating_add(1);
        self.review_fallback_records = self.review_fallback_records.saturating_add(reviews.len());
        self.review_fallbacks.insert(
            task_id.to_string(),
            CachedReviewFallback {
                journal_head,
                reviews,
                last_access: self.fallback_access_clock,
            },
        );
    }

    #[cfg(test)]
    pub(crate) fn record_fallback_replay_for_test(&mut self) {
        self.fallback_replay_count = self.fallback_replay_count.saturating_add(1);
    }

    #[cfg(test)]
    pub(crate) fn fallback_replay_count_for_test(&self) -> usize {
        self.fallback_replay_count
    }

    #[cfg(test)]
    pub(crate) fn review_cache_stats_for_test(&self) -> (usize, usize) {
        (self.review_fallbacks.len(), self.review_fallback_records)
    }

    #[cfg(test)]
    pub(crate) fn paths_for_test(&self, task_id: &str) -> (PathBuf, PathBuf, PathBuf) {
        (
            self.artifact_path(),
            self.review_path(task_id),
            self.run_directory.join(CURSOR_FILE),
        )
    }
}

fn append_records<T: HistoryRecord>(
    path: &Path,
    metadata_path: &Path,
    records: &[&T],
    metadata: SegmentMetadata,
) -> Result<SegmentMetadata, String> {
    if records.is_empty() {
        return Ok(metadata);
    }
    let mut bytes = Vec::new();
    let mut last_source_sequence = metadata.last_source_sequence;
    for record in records {
        let source_sequence = record.source_sequence();
        validate_next_sequence(last_source_sequence, source_sequence)?;
        let encoded = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        bytes.extend_from_slice(&encoded);
        bytes.push(b'\n');
        last_source_sequence = source_sequence;
    }
    let batch_record_count = u64::try_from(records.len())
        .map_err(|_| "history batch record count exceeds u64".to_string())?;
    let next = next_metadata_frame(&metadata, batch_record_count, last_source_sequence, &bytes)?;
    if let Some(parent) = path.parent() {
        echo_agent::utils::fs::create_dir_all_durable(parent).map_err(|error| error.to_string())?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_data().map_err(|error| error.to_string())?;
    append_metadata_frame(metadata_path, &next)?;
    Ok(next)
}

#[cfg(test)]
fn encoded_records_len<T: HistoryRecord>(records: &[&T]) -> Result<u64, String> {
    let mut total = 0_u64;
    for record in records {
        let encoded = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        let record_len = u64::try_from(encoded.len())
            .map_err(|_| "history record byte length exceeds u64".to_string())?
            .checked_add(1)
            .ok_or_else(|| "history record byte length overflow".to_string())?;
        total = total
            .checked_add(record_len)
            .ok_or_else(|| "history appended byte length overflow".to_string())?;
    }
    Ok(total)
}

fn replace_records<T: HistoryRecord>(
    path: &Path,
    metadata_path: &Path,
    records: &[T],
) -> Result<SegmentMetadata, String> {
    if let Some(parent) = path.parent() {
        echo_agent::utils::fs::create_dir_all_durable(parent).map_err(|error| error.to_string())?;
    }
    let mut bytes = Vec::new();
    let initial = SegmentMetadata::initial();
    let mut last_source_sequence = 0_u64;
    for record in records {
        let source_sequence = record.source_sequence();
        validate_next_sequence(last_source_sequence, source_sequence)?;
        let encoded = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        bytes.extend_from_slice(&encoded);
        bytes.push(b'\n');
        last_source_sequence = source_sequence;
    }
    let metadata = if records.is_empty() {
        initial
    } else {
        next_metadata_frame(
            &initial,
            u64::try_from(records.len())
                .map_err(|_| "history record count exceeds u64".to_string())?,
            last_source_sequence,
            &bytes,
        )?
    };
    echo_agent::utils::fs::atomic_write(path, &bytes).map_err(|error| error.to_string())?;
    replace_metadata(metadata_path, (!records.is_empty()).then_some(&metadata))?;
    Ok(metadata)
}

fn read_validated_records<T: HistoryRecord + serde::de::DeserializeOwned>(
    path: &Path,
    metadata_path: &Path,
) -> Result<(Vec<T>, SegmentMetadata), String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let metadata_bytes = std::fs::read(metadata_path).map_err(|error| error.to_string())?;
    let frames = metadata_bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).map_err(|error| error.to_string()))
        .collect::<Result<Vec<SegmentMetadata>, String>>()?;
    let mut records = Vec::new();
    let mut lines = bytes.split_inclusive(|byte| *byte == b'\n');
    let mut previous = SegmentMetadata::initial();
    for frame in frames {
        if frame.schema_version != HISTORY_SCHEMA || frame.batch_record_count == 0 {
            return Err("history segment metadata frame is invalid".to_string());
        }
        let expected_count = previous
            .record_count
            .checked_add(frame.batch_record_count)
            .ok_or_else(|| "history segment record count overflow".to_string())?;
        if frame.record_count != expected_count {
            return Err("history segment metadata count is not contiguous".to_string());
        }
        let mut hasher = Sha256::new();
        hasher.update(previous.hash_bytes()?);
        let mut last_source_sequence = previous.last_source_sequence;
        for _ in 0..frame.batch_record_count {
            let raw = lines
                .next()
                .ok_or_else(|| "history segment is shorter than its metadata".to_string())?;
            let line = raw
                .strip_suffix(b"\n")
                .ok_or_else(|| "history segment has an incomplete final record".to_string())?;
            if line.is_empty() {
                return Err("history segment contains an empty record".to_string());
            }
            let record = serde_json::from_slice::<T>(line).map_err(|error| error.to_string())?;
            validate_next_sequence(last_source_sequence, record.source_sequence())?;
            last_source_sequence = record.source_sequence();
            hasher.update(raw);
            records.push(record);
        }
        if frame.last_source_sequence != last_source_sequence
            || frame.hash_chain != hex::encode(hasher.finalize())
        {
            return Err(format!(
                "history segment metadata mismatch for {}",
                path.display()
            ));
        }
        previous = frame;
    }
    if lines.next().is_some() {
        return Err(format!(
            "history segment metadata mismatch for {}",
            path.display()
        ));
    }
    Ok((records, previous))
}

fn next_metadata_frame(
    previous: &SegmentMetadata,
    batch_record_count: u64,
    last_source_sequence: u64,
    batch_bytes: &[u8],
) -> Result<SegmentMetadata, String> {
    if batch_record_count == 0 {
        return Err("history metadata frame cannot describe an empty batch".to_string());
    }
    let mut hasher = Sha256::new();
    hasher.update(previous.hash_bytes()?);
    hasher.update(batch_bytes);
    Ok(SegmentMetadata {
        schema_version: HISTORY_SCHEMA,
        record_count: previous
            .record_count
            .checked_add(batch_record_count)
            .ok_or_else(|| "history segment record count overflow".to_string())?,
        last_source_sequence,
        batch_record_count,
        hash_chain: hex::encode(hasher.finalize()),
    })
}

fn append_metadata_frame(path: &Path, metadata: &SegmentMetadata) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        echo_agent::utils::fs::create_dir_all_durable(parent).map_err(|error| error.to_string())?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, metadata).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_data().map_err(|error| error.to_string())
}

fn replace_metadata(path: &Path, metadata: Option<&SegmentMetadata>) -> Result<(), String> {
    let mut bytes = Vec::new();
    if let Some(metadata) = metadata {
        serde_json::to_writer(&mut bytes, metadata).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
    }
    echo_agent::utils::fs::atomic_write(path, &bytes).map_err(|error| error.to_string())
}

fn validate_next_sequence(previous: u64, sequence: u64) -> Result<(), String> {
    if sequence == 0 || sequence <= previous {
        return Err(format!(
            "history source sequence is not increasing: {previous} -> {sequence}"
        ));
    }
    Ok(())
}

fn validate_increasing(sequences: impl IntoIterator<Item = u64>) -> Result<(), String> {
    let mut previous = 0_u64;
    for sequence in sequences {
        if sequence == 0 || sequence <= previous {
            return Err(format!(
                "history source sequence is not increasing: {previous} -> {sequence}"
            ));
        }
        previous = sequence;
    }
    Ok(())
}

fn project_artifact_records(events: &[RuntimeTaskEvent]) -> Result<Vec<ArtifactRecord>, String> {
    events
        .iter()
        .filter_map(|event| {
            artifact_from_event(event).map(|artifact| {
                u64::try_from(event.seq)
                    .map(|source_sequence| ArtifactRecord {
                        source_sequence,
                        artifact,
                    })
                    .map_err(|_| format!("invalid artifact source sequence {}", event.seq))
            })
        })
        .collect()
}

pub(crate) fn artifacts_from_events(events: &[RuntimeTaskEvent]) -> Vec<Artifact> {
    events.iter().filter_map(artifact_from_event).collect()
}

fn project_review_records(
    events: &[RuntimeTaskEvent],
) -> Result<BTreeMap<String, Vec<ReviewRecord>>, String> {
    let mut projected = BTreeMap::<String, Vec<ReviewRecord>>::new();
    for event in events {
        let Some(review) = review_from_event(event) else {
            continue;
        };
        let source_sequence = u64::try_from(event.seq)
            .map_err(|_| format!("invalid review source sequence {}", event.seq))?;
        let task_id = review.task_id.clone();
        projected
            .entry(task_id.clone())
            .or_default()
            .push(ReviewRecord {
                source_sequence,
                task_id,
                review,
            });
    }
    Ok(projected)
}

pub(crate) fn reviews_from_events(task_id: &str, events: &[RuntimeTaskEvent]) -> Vec<ReviewResult> {
    events
        .iter()
        .filter(|event| event.task_id.as_deref() == Some(task_id))
        .filter_map(review_from_event)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::types::ReviewOutcome;
    use super::*;

    #[test]
    fn review_segment_uses_exact_safe_identity_encoding() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let projection = HistoryProjection::open("run", temp.path(), 0);
        let (_, review, _) = projection.paths_for_test("../review/\u{4e2d}\u{6587}/\u{1f600}");
        let review_directory = temp.path().join(REVIEW_DIRECTORY);
        if review.parent() != Some(review_directory.as_path()) {
            return Err("review segment escaped its derived directory".to_string());
        }
        let file_name = review
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| "review segment has no UTF-8 file name".to_string())?;
        assert!(file_name.starts_with("id-"));
        assert!(file_name.ends_with(".jsonl"));
        assert!(file_name.is_ascii());
        assert!(!file_name.contains('/'));
        assert!(!file_name.contains('\\'));
        Ok(())
    }

    #[test]
    fn oversized_review_fallback_is_not_cached() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut projection = HistoryProjection::open("run", temp.path(), 0);
        let review = ReviewResult {
            id: "review".to_string(),
            run_id: "run".to_string(),
            task_id: "task".to_string(),
            reviewer_agent: "reviewer".to_string(),
            outcome: ReviewOutcome::Pass,
            issues: Vec::new(),
            failure_fingerprint: None,
            created_fix_task_id: None,
            created_at: chrono::Utc::now(),
        };
        projection.cache_reviews(
            "task",
            1,
            vec![review; MAX_REVIEW_FALLBACK_RECORDS.saturating_add(1)],
        );
        assert_eq!(projection.review_cache_stats_for_test(), (0, 0));
        assert!(projection.cached_reviews("task", 1).is_none());
        Ok(())
    }
}
