#[derive(Debug, Clone, Serialize, Deserialize)]
struct NativePresentationCoverage {
    first_observed_sequence: u64,
    through_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJournalCursor {
    pub stream_id: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativePresentationScope {
    pub workspace_id: String,
    pub conversation_id: Option<String>,
    pub root_turn_id: String,
    pub turn_id: String,
    pub message_id: String,
}

/// 只是原journal的展示引用。digest不把此派生结构变成原envelope或发布证明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativePresentationRecord {
    pub cursor: NativeJournalCursor,
    pub event_id: String,
    pub content_hash: String,
    pub timestamp: DateTime<Utc>,
    pub scope: NativePresentationScope,
    pub payload: NativePresentationPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum NativePresentationPayload {
    ToolCall {
        execution_id: Option<String>,
        publication_event_id: String,
        call_id: String,
        name: String,
    },
    Chart {
        execution_id: Option<String>,
        publication_event_id: String,
        spec: serde_json::Value,
    },
    Subagent {
        event: Box<crate::tasks::task_runtime::executor::ExecEvent>,
    },
}

/// 同一原journal中的展示补充；不改变正文发布、执行状态或计费资格。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativePresentationSupplement {
    pub source: crate::conversation_projection::NativeTurnSource,
    pub journal_identity: JournalIdentity,
    pub through_sequence: u64,
    pub first_observed_sequence: u64,
    pub prefix_complete: bool,
    /// 同root引用；消费者仍匹配execution/parent，不把root当执行owner。
    pub records: Vec<NativePresentationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NativePresentationCheckpoint {
    key: String,
    root_turn_id: String,
    records: Vec<NativePresentationRecord>,
}

fn has_public_chart_result(event: &ChatEventEnvelope) -> bool {
    matches!(&event.payload,
        ChatDriverEvent::Agent(agent) if matches!(&agent.payload,
            echo_agent::agent::AgentEvent::ToolResult { name, result, .. }
            if name == "generate_chart" && result.success && !result.truncated
                && result.failure.is_none() && result.error.is_none()
                && matches!(result.kind, echo_agent::tools::ToolResultKind::Text | echo_agent::tools::ToolResultKind::Json)
                && result.artifact.is_none()
                && result.mime_type.as_deref() == Some("application/vnd.vegalite.v5+json")
                && result.data.is_some()))
}

fn matching_chart_calls<'a>(
    event: &ChatEventEnvelope,
    previous: &'a [NativePresentationRecord],
) -> Vec<&'a NativePresentationRecord> {
    let ChatDriverEvent::Agent(agent) = &event.payload else {
        return Vec::new();
    };
    let echo_agent::agent::AgentEvent::ToolResult { call_id, name, .. } = &agent.payload else {
        return Vec::new();
    };
    let Some(execution) = agent.execution_id.as_ref() else {
        return Vec::new();
    };
    previous
        .iter()
        .filter(|record| {
            let NativePresentationPayload::ToolCall {
                execution_id,
                call_id: original_call,
                name: original_name,
                ..
            } = &record.payload
            else {
                return false;
            };
            record.cursor.stream_id == event.stream_id
                && record.cursor.sequence < event.sequence
                && record.scope.workspace_id == event.workspace_id
                && record.scope.conversation_id == event.conversation_id
                && record.scope.root_turn_id == event.root_turn_id
                && record.scope.turn_id == event.turn_id
                && record.scope.message_id == event.message_id
                && execution_id.as_deref() == Some(execution.as_str())
                && original_call == call_id
                && original_name == name
        })
        .collect()
}

// 仅重复native调用别名需要查原cursor；不增字段/index，也不永久保存args/log。
fn normalized_chart_calls(
    authority: &StreamAuthority,
    calls: &[&NativePresentationRecord],
) -> Result<Vec<NativePresentationRecord>, ChatEventLogError> {
    let unchanged = || calls.iter().copied().cloned().collect::<Vec<_>>();
    if calls.len() < 2 {
        return Ok(unchanged());
    }
    let native_ids: HashSet<_> = calls
        .iter()
        .filter_map(|record| match &record.payload {
            NativePresentationPayload::ToolCall {
                publication_event_id,
                ..
            } => Some(publication_event_id.as_str()),
            _ => None,
        })
        .collect();
    if native_ids.len() != 1 {
        return Ok(unchanged());
    }
    let Some(first) = calls.iter().min_by_key(|record| record.cursor.sequence) else {
        return Ok(Vec::new());
    };
    let Some(last) = calls.iter().max_by_key(|record| record.cursor.sequence) else {
        return Ok(Vec::new());
    };
    if first.cursor.sequence < authority.journal.retained_floor() {
        return Ok(unchanged());
    }
    let span = last
        .cursor
        .sequence
        .saturating_sub(first.cursor.sequence)
        .saturating_add(1);
    let Ok(limit) = usize::try_from(span) else {
        return Ok(unchanged());
    };
    if limit > NATIVE_HISTORY_PROJECTION_BATCH {
        return Ok(unchanged());
    }
    let selected: BTreeMap<_, _> = calls
        .iter()
        .map(|record| (record.cursor.sequence, *record))
        .collect();
    if selected.len() != calls.len() {
        return Ok(unchanged());
    }
    let path = authority.journal.directory();
    let raw = authority
        .journal
        .replay_after(first.cursor.sequence.saturating_sub(1), limit)
        .map_err(|error| journal_error(path, error))?;
    let mut signatures = HashSet::new();
    let mut resolved = 0_usize;
    for record in raw {
        let Some(expected) = selected.get(&record.sequence) else {
            continue;
        };
        let original = envelope_from_record(record, path, &authority.expected_stream_id)?;
        let restored = presentation_record(&original, &[]).ok_or_else(|| {
            corrupt(
                path,
                "native chart call cursor does not refer to original ToolCall",
            )
        })?;
        let left = serde_json::to_value(&restored)
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        let right = serde_json::to_value(expected)
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        if left != right {
            return Err(corrupt(
                path,
                "native chart call reference disagrees with original journal",
            ));
        }
        let ChatDriverEvent::Agent(native) = original.payload else {
            return Err(corrupt(path, "native chart call is not an Agent event"));
        };
        let encoded = echo_agent::utils::canonical_json::canonical_json_bytes(native.as_ref())
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        signatures.insert(digest(&encoded));
        resolved = resolved.saturating_add(1);
    }
    // 原调用已被裁/跨度超过固定batch/真正内容冲突均保持未恢复，不能只按ID吞冲突。
    if resolved != selected.len() || signatures.len() != 1 {
        return Ok(unchanged());
    }
    Ok(vec![(**first).clone()])
}

fn presentation_record(
    event: &ChatEventEnvelope,
    previous: &[NativePresentationRecord],
) -> Option<NativePresentationRecord> {
    use crate::tasks::task_runtime::{RuntimeEventKind, executor::ExecEventScope};
    let payload = match &event.payload {
        ChatDriverEvent::Agent(agent) => match &agent.payload {
            echo_agent::agent::AgentEvent::ToolCall {
                call_id,
                invocation,
            } => NativePresentationPayload::ToolCall {
                execution_id: agent.execution_id.as_ref().map(ToString::to_string),
                publication_event_id: agent.event_id.to_string(),
                call_id: call_id.clone(),
                name: invocation.name.clone(),
            },
            echo_agent::agent::AgentEvent::ToolResult { result, .. }
                if has_public_chart_result(event) =>
            {
                let execution = agent.execution_id.as_ref()?.as_str();
                // 不同scope或不同原调用不能因同名/相邻时间被关联。
                if matching_chart_calls(event, previous).len() != 1 {
                    return None;
                }
                let spec = result.data.as_ref()?;
                if spec.get("$schema").and_then(serde_json::Value::as_str)
                    != Some("https://vega.github.io/schema/vega-lite/v5.json")
                {
                    return None;
                }
                NativePresentationPayload::Chart {
                    execution_id: Some(execution.to_string()),
                    publication_event_id: agent.event_id.to_string(),
                    spec: spec.clone(),
                }
            }
            echo_agent::agent::AgentEvent::Chart { spec } => NativePresentationPayload::Chart {
                execution_id: agent.execution_id.as_ref().map(ToString::to_string),
                publication_event_id: agent.event_id.to_string(),
                spec: spec.clone(),
            },
            _ => return None,
        },
        ChatDriverEvent::Execution(exec)
            if exec.scope == ExecEventScope::Subagent
                && matches!(
                    exec.event,
                    RuntimeEventKind::Started
                        | RuntimeEventKind::Completed
                        | RuntimeEventKind::Failed
                        | RuntimeEventKind::Cancelled
                        | RuntimeEventKind::TimedOut
                        | RuntimeEventKind::SubagentStreamGap
                        | RuntimeEventKind::IsolationObserved
                ) =>
        {
            NativePresentationPayload::Subagent {
                event: exec.clone(),
            }
        }
        _ => return None,
    };
    Some(NativePresentationRecord {
        cursor: NativeJournalCursor {
            stream_id: event.stream_id.clone(),
            sequence: event.sequence,
        },
        event_id: event.event_id.clone(),
        content_hash: event.content_hash.clone(),
        timestamp: event.timestamp,
        scope: NativePresentationScope {
            workspace_id: event.workspace_id.clone(),
            conversation_id: event.conversation_id.clone(),
            root_turn_id: event.root_turn_id.clone(),
            turn_id: event.turn_id.clone(),
            message_id: event.message_id.clone(),
        },
        payload,
    })
}

fn presentation_key(stream: &str, root: &str) -> Result<String, ChatEventLogError> {
    native_publication_key("presentation", &(stream, root))
}

fn validate_presentation_checkpoint(
    authority: &StreamAuthority,
    root: &str,
    frame: &echo_agent::state::journal::CheckpointFrame<NativePresentationCheckpoint>,
) -> Result<(), ChatEventLogError> {
    let path = authority.journal.directory();
    if &frame.journal_identity != authority.journal.journal_identity()
        || frame.sequence > authority.journal.last_sequence()
        || frame.state.root_turn_id != root
        || frame.state.key != presentation_key(&authority.expected_stream_id, root)?
    {
        return Err(corrupt(
            path,
            "native presentation checkpoint identity/scope mismatch",
        ));
    }
    for record in &frame.state.records {
        if record.cursor.stream_id != authority.expected_stream_id
            || record.scope.root_turn_id != root
            || record.cursor.sequence > frame.sequence
            || record.event_id.is_empty()
            || record.content_hash.is_empty()
            || stream_id(
                &record.scope.workspace_id,
                record.scope.conversation_id.as_deref(),
                root,
            )? != authority.expected_stream_id
        {
            return Err(corrupt(
                path,
                "native presentation contains a foreign reference",
            ));
        }
        if let NativePresentationPayload::Subagent { event } = &record.payload {
            let payload = ChatDriverEvent::Execution(event.clone());
            validate_driver_event(&payload)?;
            validate_event_stream_identity(
                &record.scope.workspace_id,
                record.scope.conversation_id.as_deref(),
                root,
                &payload,
            )?;
            let (turn, message) = event_identity(&payload, root);
            if turn != record.scope.turn_id || message != record.scope.message_id {
                return Err(corrupt(
                    path,
                    "native presentation reference lost original coordinates",
                ));
            }
        }
    }
    Ok(())
}

fn fold_presentation_event(
    authority: &StreamAuthority,
    coverage: &mut NativePresentationCoverage,
    event: &ChatEventEnvelope,
) -> Result<(), ChatEventLogError> {
    let direct = presentation_record(event, &[]);
    if direct.is_some() || has_public_chart_result(event) {
        let key = presentation_key(&event.stream_id, &event.root_turn_id)?;
        let checkpoint = native_history_checkpoint::<NativePresentationCheckpoint>(authority, &key);
        let loaded = checkpoint
            .load()
            .map_err(|error| journal_error(authority.journal.directory(), error))?;
        if let Some(frame) = &loaded {
            validate_presentation_checkpoint(authority, &event.root_turn_id, frame)?;
        }
        let mut state = loaded
            .as_ref()
            .map(|frame| frame.state.clone())
            .unwrap_or_else(|| NativePresentationCheckpoint {
                key,
                root_turn_id: event.root_turn_id.clone(),
                records: Vec::new(),
            });
        let calls = if direct.is_none() {
            normalized_chart_calls(authority, &matching_chart_calls(event, &state.records))?
        } else {
            Vec::new()
        };
        if let Some(record) = direct.or_else(|| presentation_record(event, &calls)) {
            let identical = state.records.iter().any(|previous| {
                previous.event_id == record.event_id && previous.content_hash == record.content_hash
            });
            let through = loaded
                .as_ref()
                .map_or(event.sequence, |frame| frame.sequence.max(event.sequence));
            if !identical {
                state.records.push(record);
            }
            if !identical
                || loaded
                    .as_ref()
                    .is_none_or(|frame| frame.sequence > coverage.through_sequence)
            {
                // 原barrier确认后才准许裁剪；rename可见不等于已确认。只保引用、公开图表与生命周期，
                // 不额外永久复制工具参数/日志、token噪声、私有推理或provider原文。
                checkpoint
                    .save(authority.journal.journal_identity(), &state, through)
                    .map_err(|error| journal_error(authority.journal.directory(), error))?;
            }
        }
    }
    coverage.through_sequence = event.sequence;
    Ok(())
}

impl ChatEventLog {
    fn native_presentation(
        &self,
        source: &crate::conversation_projection::NativeTurnSource,
    ) -> Result<NativePresentationSupplement, ChatEventLogError> {
        let stream = stream_id(
            &source.workspace_id,
            Some(&source.conversation_id),
            &source.root_turn_id,
        )?;
        let cached = self.stream_journal(&stream, false)?.ok_or_else(|| {
            ChatEventLogError::NativeHistoryGap {
                retained_floor: 0,
                key: "presentation-original-journal-missing".into(),
            }
        })?;
        let mut guard = lock_cached_stream(&cached);
        let authority = guard
            .as_mut()
            .ok_or(ChatEventLogError::NativeHistoryReadChanged)?;
        let (through, head) = advance_native_publication_projection(authority)?;
        if through < head {
            return Err(ChatEventLogError::NativeHistoryPending {
                through_sequence: through,
                latest_sequence: head,
            });
        }
        let scope_key = native_publication_key("scope", &source.execution_id)?;
        let witnesses = read_native_publication_anchor(authority, &scope_key, None)?;
        if witnesses.is_empty()
            || witnesses.iter().any(|event| {
                event.workspace_id != source.workspace_id
                    || event.conversation_id.as_deref() != Some(source.conversation_id.as_str())
                    || event.root_turn_id != source.root_turn_id
                    || event.turn_id != source.turn_id
                    || event.message_id != source.root_turn_id
            })
        {
            // 逻辑stream路径可被重新创建；当前generation没有原parent见证时，
            // 不能把另一个空/新journal当作“旧图表或子运行从未存在”的证明。
            return Err(ChatEventLogError::NativeHistoryGap {
                retained_floor: authority.journal.retention_metadata().retained_floor,
                key: "presentation-parent-source-unavailable".into(),
            });
        }
        let coverage =
            native_history_checkpoint::<NativePublicationCoverage>(authority, "coverage")
                .load()
                .map_err(|error| journal_error(authority.journal.directory(), error))?
                .and_then(|frame| frame.state.presentation)
                .ok_or(ChatEventLogError::NativeHistoryReadChanged)?;
        let key = presentation_key(&stream, &source.root_turn_id)?;
        let frame = native_history_checkpoint::<NativePresentationCheckpoint>(authority, &key)
            .load()
            .map_err(|error| journal_error(authority.journal.directory(), error))?;
        let records = if let Some(frame) = frame {
            validate_presentation_checkpoint(authority, &source.root_turn_id, &frame)?;
            frame.state.records
        } else {
            Vec::new()
        };
        Ok(NativePresentationSupplement {
            source: source.clone(),
            journal_identity: authority.journal.journal_identity().clone(),
            through_sequence: coverage.through_sequence,
            first_observed_sequence: coverage.first_observed_sequence,
            prefix_complete: coverage.first_observed_sequence == 1
                && coverage.through_sequence == head,
            records,
        })
    }

    pub async fn native_presentation_async(
        self: &Arc<Self>,
        source: &crate::conversation_projection::NativeTurnSource,
        cancel: &echo_agent::agent::CancellationToken,
    ) -> Result<NativePresentationSupplement, ChatEventLogError> {
        loop {
            if cancel.is_cancelled() {
                return Err(ChatEventLogError::NativeHistoryReadCancelled);
            }
            let permit = tokio::select! {
                biased;
                _=cancel.cancelled()=>return Err(ChatEventLogError::NativeHistoryReadCancelled),
                permit=PROCESS_CHAT_EVENT_IO.clone().acquire_owned()=>permit
                    .map_err(|error|ChatEventLogError::Serialization(error.to_string()))?,
            };
            let log = Arc::clone(self);
            let source = source.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                log.native_presentation(&source)
            })
            .await
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
            match result {
                Err(ChatEventLogError::NativeHistoryPending { .. }) => {
                    tokio::task::yield_now().await
                }
                result => return result,
            }
        }
    }
}
