/// 原 ChatEventLog 的只读派生累计；没有独立写入口、消息或执行状态权威。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageTotals {
    pub attempts: u64,
    pub steps: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub missing_usage: u64,
    pub unidentified_steps: u64,
    pub conflicts: u64,
    pub pending_calls: u64,
    pub timed_output_tokens: u64,
    pub output_duration_ms: u64,
    pub latest_speed: Option<f64>,
    pub latest_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ConversationUsageProjection {
    first_observed_sequence: u64,
    through_sequence: u64,
    turns: u64,
    turns_known: bool,
    main: UsageTotals,
    subagent: UsageTotals,
    models: BTreeMap<String, UsageTotals>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationUsageSnapshot {
    pub workspace_id: String,
    pub conversation_id: String,
    pub journal_identity: Option<JournalIdentity>,
    pub through_sequence: u64,
    pub latest_sequence: u64,
    pub first_observed_sequence: Option<u64>,
    /// 仅说明原日志覆盖；provider 漏报与调用身份缺失另列，不能合并成零消耗。
    pub prefix_complete: bool,
    pub turns: u64,
    pub turns_known: bool,
    pub main: UsageTotals,
    pub subagent: UsageTotals,
    pub models: BTreeMap<String, UsageTotals>,
    pub auxiliary_coverage: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObservedUsage {
    model: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_prompt_tokens: u64,
    cache_creation_prompt_tokens: u64,
    usage_reported: bool,
    #[serde(default)]
    observation: Option<echo_agent::agent::ModelCallObservation>,
}

#[derive(Deserialize)]
struct UsageSourceCoordinates {
    workspace_id: String,
    conversation_id: Option<String>,
    root_turn_id: Option<String>,
    turn_id: String,
    execution_id: Option<String>,
    event_id: String,
}

struct UsageFact {
    usage: ObservedUsage,
    main: bool,
    attempt_key: String,
    call_key: Option<String>,
    turn_key: String,
}

fn usage_fact(event: &ChatEventEnvelope) -> Result<Option<UsageFact>, ChatEventLogError> {
    use crate::tasks::task_runtime::RuntimeEventKind;
    use crate::tasks::task_runtime::executor::ExecEventScope;
    use echo_agent::agent::AgentEvent;
    let (usage, main, original_id, execution, root, turn) = match &event.payload {
        ChatDriverEvent::Agent(agent) => match &agent.payload {
            AgentEvent::LlmUsage {
                model,
                prompt_tokens,
                completion_tokens,
                cached_prompt_tokens,
                cache_creation_prompt_tokens,
                usage_reported,
                observation,
                ..
            } => (
                ObservedUsage {
                    model: model.clone(),
                    prompt_tokens: u64::try_from(*prompt_tokens).unwrap_or(u64::MAX),
                    completion_tokens: u64::try_from(*completion_tokens).unwrap_or(u64::MAX),
                    cached_prompt_tokens: u64::try_from(*cached_prompt_tokens).unwrap_or(u64::MAX),
                    cache_creation_prompt_tokens: u64::try_from(*cache_creation_prompt_tokens)
                        .unwrap_or(u64::MAX),
                    usage_reported: *usage_reported,
                    observation: observation.clone(),
                },
                event.turn_id == event.root_turn_id && event.message_id == event.root_turn_id,
                agent.event_id.to_string(),
                agent.execution_id.as_ref().map(ToString::to_string),
                event.root_turn_id.clone(),
                event.turn_id.clone(),
            ),
            _ => return Ok(None),
        },
        ChatDriverEvent::Execution(exec) if exec.event == RuntimeEventKind::Usage => {
            // 不把 TaskRun 的累计快照加回单次请求；只消费原 sink 的 usage_event_id。
            let Some(id) = exec
                .payload
                .get("usage_event_id")
                .and_then(serde_json::Value::as_str)
            else {
                return Ok(None);
            };
            let usage = serde_json::from_value::<ObservedUsage>(exec.payload.clone())
                .unwrap_or_else(|_| ObservedUsage {
                    model: "unknown".into(),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                    usage_reported: false,
                    observation: None,
                });
            let source = exec
                .payload
                .get("usage_source")
                .and_then(|raw| serde_json::from_value::<UsageSourceCoordinates>(raw.clone()).ok())
                .filter(|source| {
                    source.workspace_id == event.workspace_id
                        && source.conversation_id == event.conversation_id
                        && source.event_id == id
                        && !source.turn_id.trim().is_empty()
                        && source
                            .root_turn_id
                            .as_ref()
                            .is_none_or(|root| !root.trim().is_empty())
                });
            // 只转交原native坐标。Run ID不代表execution，旧投影缺字段就保持未知。
            let execution = source
                .as_ref()
                .and_then(|source| source.execution_id.clone())
                .filter(|id| !id.trim().is_empty())
                .or_else(|| exec.subagent_run_id.clone());
            let root = source
                .as_ref()
                .and_then(|source| source.root_turn_id.clone())
                .unwrap_or_else(|| event.root_turn_id.clone());
            let turn = source
                .as_ref()
                .map(|source| source.turn_id.clone())
                .unwrap_or_else(|| event.turn_id.clone());
            (
                usage,
                exec.scope == ExecEventScope::Run && exec.task_id.is_none(),
                id.to_string(),
                execution,
                root,
                turn,
            )
        }
        _ => return Ok(None),
    };
    let turn_key = native_publication_key(
        "usage-turn",
        &(&event.workspace_id, &event.conversation_id, &root, &turn),
    )?;
    let identified_execution = execution.as_ref().is_some_and(|id| !id.trim().is_empty());
    let scope = (
        &event.workspace_id,
        &event.conversation_id,
        &root,
        &turn,
        execution,
    );
    let associated = usage
        .observation
        .as_ref()
        .filter(|value| !value.call_id.trim().is_empty() && !value.attempt_id.trim().is_empty());
    let attempt_key = if let Some(observation) = associated {
        native_publication_key(
            "usage-attempt",
            &(&scope, &observation.call_id, &observation.attempt_id),
        )?
    } else {
        native_publication_key("usage-attempt", &(&scope, original_id))?
    };
    let call_key = associated
        .filter(|_| identified_execution)
        .map(|observation| native_publication_key("usage-call", &(&scope, &observation.call_id)))
        .transpose()?;
    Ok(Some(UsageFact {
        usage,
        main,
        attempt_key,
        call_key,
        turn_key,
    }))
}

fn usage_turn_key(event: &ChatEventEnvelope) -> Result<Option<String>, ChatEventLogError> {
    let admitted = match &event.payload {
        ChatDriverEvent::TurnConfiguration { .. } => true,
        ChatDriverEvent::Agent(agent) => {
            matches!(&agent.payload,
            echo_agent::agent::AgentEvent::ExecutionStatus(status)
            if !status.adapter_generated && status.status == echo_agent::agent::ExecutionStatusKind::Started
                && agent.execution_id.as_ref().is_some_and(|id|id.as_str()==status.execution_id))
                && event.turn_id == event.root_turn_id
                && event.message_id == event.root_turn_id
        }
        _ => false,
    };
    if !admitted {
        return Ok(None);
    }
    usage_turn_scope_key(event).map(Some)
}

fn usage_turn_scope_key(event: &ChatEventEnvelope) -> Result<String, ChatEventLogError> {
    native_publication_key(
        "usage-turn",
        &(
            &event.workspace_id,
            &event.conversation_id,
            &event.root_turn_id,
            &event.turn_id,
        ),
    )
}

fn usage_started_key(
    event: &ChatEventEnvelope,
) -> Result<Option<(String, String)>, ChatEventLogError> {
    let ChatDriverEvent::Agent(agent) = &event.payload else {
        return Ok(None);
    };
    let echo_agent::agent::AgentEvent::ExecutionStatus(status) = &agent.payload else {
        return Ok(None);
    };
    if status.adapter_generated
        || status.status != echo_agent::agent::ExecutionStatusKind::Waiting
        || !agent
            .execution_id
            .as_ref()
            .is_some_and(|id| id.as_str() == status.execution_id)
        || event.turn_id != event.root_turn_id
        || event.message_id != event.root_turn_id
    {
        return Ok(None);
    }
    let Some(call) = status
        .request_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let scope = (
        &event.workspace_id,
        &event.conversation_id,
        &event.root_turn_id,
        &event.turn_id,
        agent.execution_id.as_ref().map(ToString::to_string),
    );
    Ok(Some((
        native_publication_key("usage-start", &(&scope, call))?,
        native_publication_key("usage-call", &(&scope, call))?,
    )))
}

fn usage_anchor_keys(event: &ChatEventEnvelope) -> Result<Vec<String>, ChatEventLogError> {
    let mut keys = Vec::new();
    if let Some((key, _)) = usage_started_key(event)? {
        keys.push(key);
    }
    if let Some(key) = usage_turn_key(event)? {
        keys.push(key);
    }
    if let Some(fact) = usage_fact(event)? {
        keys.push(fact.attempt_key);
        if let Some(key) = fact.call_key {
            keys.push(key);
        }
    }
    Ok(keys)
}

fn usage_anchor_seen(
    authority: &StreamAuthority,
    key: &str,
    through: u64,
) -> Result<Option<ChatEventEnvelope>, ChatEventLogError> {
    let checkpoint = native_history_checkpoint::<NativePublicationAnchor>(authority, key);
    let loaded = checkpoint
        .load()
        .map_err(|error| journal_error(authority.journal.directory(), error))?;
    if let Some(frame) = loaded {
        validate_native_anchor(authority, key, &frame)?;
        return Ok(frame
            .state
            .records
            .into_iter()
            .find(|event| event.sequence <= through));
    }
    Ok(None)
}

impl UsageTotals {
    fn record(&mut self, usage: &ObservedUsage, new_call: bool, identified: bool) {
        self.attempts = self.attempts.saturating_add(1);
        if new_call {
            self.steps = self.steps.saturating_add(1);
        }
        if !identified {
            self.unidentified_steps = self.unidentified_steps.saturating_add(1);
        }
        if !usage.usage_reported {
            self.missing_usage = self.missing_usage.saturating_add(1);
            return;
        }
        self.input_tokens = self.input_tokens.saturating_add(usage.prompt_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.completion_tokens);
        self.cached_tokens = self
            .cached_tokens
            .saturating_add(usage.cached_prompt_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(usage.cache_creation_prompt_tokens);
        self.latest_input_tokens = Some(usage.prompt_tokens);
        if usage.cached_prompt_tokens > usage.prompt_tokens {
            self.conflicts = self.conflicts.saturating_add(1);
        }
        if let Some(observation) = &usage.observation
            && let Some(speed) =
                observation.observed_output_tokens_per_second(usage.completion_tokens)
        {
            self.latest_speed = Some(speed);
            if let Some(duration) = observation
                .last_output_ms
                .zip(observation.first_output_ms)
                .and_then(|(last, first)| last.checked_sub(first))
            {
                self.timed_output_tokens = self
                    .timed_output_tokens
                    .saturating_add(usage.completion_tokens);
                self.output_duration_ms = self.output_duration_ms.saturating_add(duration);
            }
        }
    }
}

fn fold_usage_event(
    authority: &StreamAuthority,
    state: &mut ConversationUsageProjection,
    event: &ChatEventEnvelope,
) -> Result<(), ChatEventLogError> {
    let context_invalidated = match &event.payload {
        ChatDriverEvent::ContextCompressed { .. } => true,
        ChatDriverEvent::Agent(agent) => {
            matches!(
                agent.payload,
                echo_agent::agent::AgentEvent::ContextCompressed { .. }
            ) && event.turn_id == event.root_turn_id
                && event.message_id == event.root_turn_id
        }
        _ => false,
    };
    // 压缩令“最近已发送 prompt”不再代表当前上下文；已发生费用继续累计。
    // Option 保留未知和实际报告 0 的区别，冷开不能再次展示压缩前的占用。
    if context_invalidated {
        state.main.latest_input_tokens = None;
    }
    if let Some((start_key, call_key)) = usage_started_key(event)?
        && usage_anchor_seen(authority, &start_key, state.through_sequence)?.is_none()
        && usage_anchor_seen(authority, &call_key, state.through_sequence)?.is_none()
    {
        state.main.steps = state.main.steps.saturating_add(1);
        state.main.pending_calls = state.main.pending_calls.saturating_add(1);
    }
    if let Some(key) = usage_turn_key(event)?
        && usage_anchor_seen(authority, &key, state.through_sequence)?.is_none()
    {
        state.turns = state.turns.saturating_add(1);
    }
    if let Some(fact) = usage_fact(event)? {
        if fact.main
            && usage_anchor_seen(authority, &fact.turn_key, state.through_sequence)?.is_none()
        {
            state.turns_known = false;
        }
        let previous = usage_anchor_seen(authority, &fact.attempt_key, state.through_sequence)?;
        if let Some(previous) = previous {
            if let Some(previous) = usage_fact(&previous)?
                && serde_json::to_value(&previous.usage).ok()
                    != serde_json::to_value(&fact.usage).ok()
            {
                let totals = if fact.main {
                    &mut state.main
                } else {
                    &mut state.subagent
                };
                totals.conflicts = totals.conflicts.saturating_add(1);
            }
        } else {
            let first_usage_for_call = match &fact.call_key {
                Some(key) => usage_anchor_seen(authority, key, state.through_sequence)?.is_none(),
                None => false,
            };
            let started = if let Some(observation) = &fact.usage.observation {
                let start_key = fact
                    .call_key
                    .as_ref()
                    .map(|key| key.replacen("usage-call-", "usage-start-", 1));
                match start_key {
                    Some(key) if !observation.call_id.is_empty() => {
                        usage_anchor_seen(authority, &key, state.through_sequence)?.is_some()
                    }
                    _ => false,
                }
            } else {
                false
            };
            let new_call = first_usage_for_call && !started;
            let totals = if fact.main {
                &mut state.main
            } else {
                &mut state.subagent
            };
            if started && first_usage_for_call {
                totals.pending_calls = totals.pending_calls.saturating_sub(1);
            }
            totals.record(&fact.usage, new_call, fact.call_key.is_some());
            if fact.main {
                state
                    .models
                    .entry(fact.usage.model.clone())
                    .or_default()
                    .record(&fact.usage, first_usage_for_call, fact.call_key.is_some());
            }
        }
    }
    // 事实锚先落盘，后提交累计覆盖。若进程中断，锚 sequence > 已提交 through，
    // 恢复会重新累计那次原事件；不能把“文件已存在”误当“已计费”。
    for key in usage_anchor_keys(event)? {
        preserve_native_publication_key(authority, event, &key, state.through_sequence)?;
    }
    state.through_sequence = event.sequence;
    Ok(())
}

impl ChatEventLog {
    /// 一次固定 batch；调用者看到 Pending 后续读，不把尾窗冒充累计。
    pub fn conversation_usage(
        &self,
        workspace: &str,
        conversation: &str,
    ) -> Result<ConversationUsageSnapshot, ChatEventLogError> {
        let stream = stream_id(workspace, Some(conversation), conversation)?;
        let mut result = ConversationUsageSnapshot {
            workspace_id: workspace.into(),
            conversation_id: conversation.into(),
            journal_identity: None,
            through_sequence: 0,
            latest_sequence: 0,
            first_observed_sequence: None,
            prefix_complete: false,
            turns: 0,
            turns_known: false,
            main: UsageTotals::default(),
            subagent: UsageTotals::default(),
            models: BTreeMap::new(),
            auxiliary_coverage: "unobserved".into(),
        };
        let Some(cached) = self.stream_journal(&stream, false)? else {
            return Ok(result);
        };
        let mut guard = lock_cached_stream(&cached);
        let Some(authority) = guard.as_mut() else {
            return Ok(result);
        };
        let (through, head) = advance_native_publication_projection(authority)?;
        if through < head {
            return Err(ChatEventLogError::NativeHistoryPending {
                through_sequence: through,
                latest_sequence: head,
            });
        }
        let frame = native_history_checkpoint::<NativePublicationCoverage>(authority, "coverage")
            .load()
            .map_err(|error| journal_error(authority.journal.directory(), error))?;
        if let Some(frame) = frame
            && let Some(usage) = frame.state.usage
        {
            result.journal_identity = Some(frame.journal_identity);
            result.through_sequence = usage.through_sequence;
            result.latest_sequence = head;
            result.first_observed_sequence = Some(usage.first_observed_sequence);
            result.prefix_complete =
                usage.first_observed_sequence == 1 && usage.through_sequence == head;
            result.turns = usage.turns;
            result.turns_known = usage.turns_known;
            result.main = usage.main;
            result.subagent = usage.subagent;
            result.models = usage.models;
        }
        Ok(result)
    }

    pub async fn conversation_usage_async(
        self: &Arc<Self>,
        workspace: &str,
        conversation: &str,
        cancel: &echo_agent::agent::CancellationToken,
    ) -> Result<ConversationUsageSnapshot, ChatEventLogError> {
        loop {
            if cancel.is_cancelled() {
                return Err(ChatEventLogError::NativeHistoryReadCancelled);
            }
            let permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ChatEventLogError::NativeHistoryReadCancelled),
                permit = PROCESS_CHAT_EVENT_IO.clone().acquire_owned() => permit
                    .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?,
            };
            let log = Arc::clone(self);
            let workspace = workspace.to_string();
            let conversation = conversation.to_string();
            let result = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                log.conversation_usage(&workspace, &conversation)
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
