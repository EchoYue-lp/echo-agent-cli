// 查询/追平只读一个固定原journal batch；调用者可用Pending中的cursor续接，
// 不能把tail4096或尚未追平的投影冒充完整history。
const NATIVE_HISTORY_PROJECTION_BATCH: usize = REPLAY_BATCH_SIZE;
const NATIVE_HISTORY_DIRECTORY: &str = "native-publications";

fn native_history_message_requires_publications(message: &echo_agent::llm::types::Message) -> bool {
    message.role == echo_agent::llm::types::Role::Assistant
        && (message
            .approved_assistant_blocks
            .as_ref()
            .is_some_and(|blocks| !blocks.is_empty())
            || message.execution_status.is_some())
}

fn native_publication_key<T: Serialize>(
    kind: &str,
    value: &T,
) -> Result<String, ChatEventLogError> {
    let bytes = echo_agent::utils::canonical_json::canonical_json_bytes(value)
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
    Ok(format!("{kind}-{}", digest(&bytes)))
}

fn native_product_key(event: &ChatEventEnvelope) -> Result<String, ChatEventLogError> {
    native_publication_key(
        "product",
        &(
            &event.workspace_id,
            &event.conversation_id,
            &event.root_turn_id,
            &event.turn_id,
            &event.message_id,
        ),
    )
}

fn native_publication_fact_key(
    event: &ChatEventEnvelope,
) -> Result<Option<String>, ChatEventLogError> {
    use echo_agent::agent::{AgentEvent, ExecutionStatusKind};
    match &event.payload {
        ChatDriverEvent::Agent(agent) => match &agent.payload {
            AgentEvent::AssistantMessage { block } => {
                native_publication_key("block", block).map(Some)
            }
            AgentEvent::FinalAnswer(_) => agent
                .execution_id
                .as_ref()
                .map(|execution| native_publication_key("final", &execution.as_str()))
                .transpose(),
            AgentEvent::ExecutionStatus(status)
                if matches!(
                    status.status,
                    ExecutionStatusKind::Settled
                        | ExecutionStatusKind::Failed
                        | ExecutionStatusKind::Cancelled
                ) =>
            {
                agent
                    .execution_id
                    .as_ref()
                    .map(|execution| native_publication_key("terminal", &execution.as_str()))
                    .transpose()
            }
            _ => Ok(None),
        },
        ChatDriverEvent::TurnStatus { status }
            if matches!(status.as_str(), "completed" | "failed" | "cancelled") =>
        {
            native_product_key(event).map(Some)
        }
        _ => Ok(None),
    }
}

fn native_execution_scope_key(
    event: &ChatEventEnvelope,
) -> Result<Option<String>, ChatEventLogError> {
    match &event.payload {
        ChatDriverEvent::Agent(agent) => agent
            .execution_id
            .as_ref()
            .map(|execution| native_publication_key("scope", &execution.as_str()))
            .transpose(),
        _ => Ok(None),
    }
}

fn native_publication_same_scope(left: &ChatEventEnvelope, right: &ChatEventEnvelope) -> bool {
    left.workspace_id == right.workspace_id
        && left.conversation_id == right.conversation_id
        && left.root_turn_id == right.root_turn_id
        && left.turn_id == right.turn_id
        && left.message_id == right.message_id
}

fn native_history_checkpoint<S>(authority: &StreamAuthority, name: &str) -> FileCheckpointStore<S> {
    FileCheckpointStore::open(
        authority
            .journal
            .directory()
            .join(NATIVE_HISTORY_DIRECTORY)
            .join(format!("{name}.json")),
    )
}

fn validate_native_anchor(
    authority: &StreamAuthority,
    expected_key: &str,
    frame: &echo_agent::state::journal::CheckpointFrame<NativePublicationAnchor>,
) -> Result<(), ChatEventLogError> {
    let path = authority.journal.directory();
    if &frame.journal_identity != authority.journal.journal_identity()
        || frame.sequence > authority.journal.last_sequence()
        || frame.state.key != expected_key
        || frame.state.records.len() > 2
    {
        return Err(corrupt(
            path,
            "native publication checkpoint identity/key/coverage mismatch",
        ));
    }
    for event in &frame.state.records {
        if event.stream_id != authority.expected_stream_id
            || event.sequence > frame.sequence
            || (native_publication_fact_key(event)?.as_deref() != Some(expected_key)
                && native_execution_scope_key(event)?.as_deref() != Some(expected_key))
        {
            return Err(corrupt(
                path,
                "native publication checkpoint contains a foreign original record",
            ));
        }
        validate_driver_event(&event.payload)?;
        validate_event_stream_identity(
            &event.workspace_id,
            event.conversation_id.as_deref(),
            &event.root_turn_id,
            &event.payload,
        )?;
    }
    Ok(())
}

fn preserve_native_publication(
    authority: &StreamAuthority,
    event: &ChatEventEnvelope,
    confirmed_through: u64,
) -> Result<(), ChatEventLogError> {
    // 原statusful validator检查同execution全部Agent事件的outer scope。
    // 每个不同scope保原record见证，不能丢掉Token/ToolCall中的跨owner拒绝依据。
    let mut keys = Vec::new();
    if let Some(key) = native_publication_fact_key(event)? {
        keys.push(key);
    }
    if let Some(key) = native_execution_scope_key(event)? {
        keys.push(key);
    }
    for key in keys {
        preserve_native_publication_key(authority, event, &key, confirmed_through)?;
    }
    Ok(())
}

fn preserve_native_publication_key(
    authority: &StreamAuthority,
    event: &ChatEventEnvelope,
    key: &str,
    confirmed_through: u64,
) -> Result<(), ChatEventLogError> {
    let checkpoint = native_history_checkpoint::<NativePublicationAnchor>(authority, key);
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(authority, "anchor_load", "attempt");
    let load_result = checkpoint.load();
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(
        authority,
        "anchor_load",
        if load_result.is_ok() {
            "success"
        } else {
            "failure"
        },
    );
    let loaded =
        load_result.map_err(|error| journal_error(authority.journal.directory(), error))?;
    let mut state = if let Some(frame) = loaded.as_ref() {
        validate_native_anchor(authority, key, frame)?;
        frame.state.clone()
    } else {
        NativePublicationAnchor {
            key: key.to_string(),
            records: Vec::new(),
            ambiguous: false,
        }
    };
    let through = loaded
        .as_ref()
        .filter(|frame| &frame.journal_identity == authority.journal.journal_identity())
        .map_or(event.sequence, |frame| frame.sequence.max(event.sequence));
    let identical = if key.starts_with("scope-") {
        state
            .records
            .iter()
            .any(|previous| native_publication_same_scope(previous, event))
    } else {
        let encoded = echo_agent::utils::canonical_json::canonical_json_bytes(event)
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        state
            .records
            .iter()
            .map(echo_agent::utils::canonical_json::canonical_json_bytes)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?
            .contains(&encoded)
    };
    if identical
        && loaded
            .as_ref()
            .is_some_and(|frame| frame.sequence <= confirmed_through)
    {
        // 此原record的barrier已由先前成功投影/当前有界batch确认，无须每个Token重写快照。
        return Ok(());
    }
    if !identical {
        if key.starts_with("product-") {
            // 原validator本来取同scope最后一个真实product terminal；不保整条状态流。
            if state
                .records
                .first()
                .is_none_or(|previous| event.sequence > previous.sequence)
            {
                state.records = vec![event.clone()];
            }
        } else if state.records.len() < 2 {
            // 唯一许可需要原全记录；两个不同见证已足以暴露非唯一来源。
            state.records.push(event.clone());
        } else {
            state.ambiguous = true;
        }
    }
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(authority, "anchor_save", "attempt");
    let save_result = checkpoint.save(authority.journal.journal_identity(), &state, through);
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(
        authority,
        "anchor_save",
        if save_result.is_ok() {
            "success"
        } else {
            "failure"
        },
    );
    save_result.map_err(|error| journal_error(authority.journal.directory(), error))
}

fn advance_native_publication_projection(
    authority: &StreamAuthority,
) -> Result<(u64, u64), ChatEventLogError> {
    advance_native_publication_projection_through(authority, authority.journal.last_sequence())
}

fn advance_native_publication_projection_through(
    authority: &StreamAuthority,
    frozen_head: u64,
) -> Result<(u64, u64), ChatEventLogError> {
    let path = authority.journal.directory();
    // 先确认原journal，不从仅存在于checkpoint的内容创造事实。
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(authority, "journal_sync", "attempt");
    let sync_result = authority.journal.sync_data();
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(
        authority,
        "journal_sync",
        if sync_result.is_ok() {
            "success"
        } else {
            "failure"
        },
    );
    sync_result.map_err(|error| journal_error(path, error))?;
    let floor = authority.journal.retention_metadata().retained_floor;
    let actual_head = authority.journal.last_sequence();
    if frozen_head > actual_head {
        return Err(ChatEventLogError::NativeHistoryReadChanged);
    }
    let head = frozen_head;
    let checkpoint = native_history_checkpoint::<NativePublicationCoverage>(authority, "coverage");
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(authority, "coverage_load", "attempt");
    let load_result = checkpoint.load();
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(
        authority,
        "coverage_load",
        if load_result.is_ok() {
            "success"
        } else {
            "failure"
        },
    );
    let loaded = load_result.map_err(|error| journal_error(path, error))?;
    let (mut through, coverage) = if let Some(frame) = loaded {
        if &frame.journal_identity != authority.journal.journal_identity() {
            return Err(corrupt(
                path,
                "native publication coverage belongs to a foreign journal generation",
            ));
        }
        if frame.sequence > actual_head
            || frame.state.first_observed_sequence == 0
            || frame.state.first_observed_sequence > frame.sequence.saturating_add(1)
        {
            return Err(corrupt(
                path,
                "native publication coverage exceeds original journal",
            ));
        }
        if frame.sequence < floor.saturating_sub(1) {
            return Err(ChatEventLogError::NativeHistoryGap {
                retained_floor: floor,
                key: "coverage".into(),
            });
        }
        (frame.sequence, frame.state)
    } else {
        (
            floor.saturating_sub(1),
            NativePublicationCoverage {
                first_observed_sequence: floor,
            },
        )
    };
    // 原前缀未受此generation的可靠coverage覆盖，就不能证明后缀没有漏掉第二owner。
    // 这不是空checkpoint：保留原journal并明确gap，不静默重建唯一性或成功。
    if coverage.first_observed_sequence != 1 {
        return Err(ChatEventLogError::NativeHistoryGap {
            retained_floor: floor,
            key: "original-prefix-uncovered".into(),
        });
    }
    if through < head {
        let batch = authority
            .journal
            .replay_after(through, NATIVE_HISTORY_PROJECTION_BATCH)
            .map_err(|error| journal_error(path, error))?;
        if batch.is_empty() {
            return Err(corrupt(
                path,
                "native publication bounded replay did not reach original head",
            ));
        }
        for record in batch {
            if record.sequence > head {
                break;
            }
            if record.sequence != through.saturating_add(1) {
                return Err(corrupt(
                    path,
                    "native publication bounded replay skipped original sequence",
                ));
            }
            let event = envelope_from_record(record, path, &authority.expected_stream_id)?;
            preserve_native_publication(authority, &event, through)?;
            through = event.sequence;
        }
    }
    // 所有原pub原子落盘以后才提交coverage；任何barrier错误都阻止后续prune。
    // 即使load已见rename，也重新save以确认本次父目录barrier，不把degraded当成功。
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(authority, "coverage_save", "attempt");
    let save_result = checkpoint.save(authority.journal.journal_identity(), &coverage, through);
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(
        authority,
        "coverage_save",
        if save_result.is_ok() {
            "success"
        } else {
            "failure"
        },
    );
    save_result.map_err(|error| journal_error(path, error))?;
    Ok((through, head))
}

fn read_native_publication_anchor(
    authority: &StreamAuthority,
    key: &str,
    frozen_head: Option<u64>,
) -> Result<Vec<ChatEventEnvelope>, ChatEventLogError> {
    let checkpoint = native_history_checkpoint::<NativePublicationAnchor>(authority, key);
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(authority, "anchor_load", "attempt");
    let load_result = checkpoint.load();
    #[cfg(feature = "test-utils")]
    native_history_perf_io_event(
        authority,
        "anchor_load",
        if load_result.is_ok() {
            "success"
        } else {
            "failure"
        },
    );
    let loaded =
        load_result.map_err(|error| journal_error(authority.journal.directory(), error))?;
    let Some(frame) = loaded else {
        // 完整coverage下未出现的事实是权威缺席；例如Candidate不保证真的发过Settled。
        // 原前缀缺coverage已由advance明确Gap，不能按当前floor误判为事实丢失。
        return Ok(Vec::new());
    };
    validate_native_anchor(authority, key, &frame)?;
    if let Some(head) = frozen_head
        && (frame
            .state
            .records
            .iter()
            .any(|event| event.sequence > head)
            || (frame.state.ambiguous && frame.sequence > head))
    {
        // product最后状态等投影已越过冻结读点，不能拼接不同head的事实。
        return Err(ChatEventLogError::NativeHistorySnapshotAdvanced {
            frozen_head: head,
            latest_sequence: authority.journal.last_sequence(),
        });
    }
    if frame.state.ambiguous {
        return Err(corrupt(
            authority.journal.directory(),
            "native publication has more than one original source",
        ));
    }
    Ok(frame.state.records)
}

fn native_publications_for_message(
    authority: &StreamAuthority,
    message: &echo_agent::llm::types::Message,
    frozen_head: Option<u64>,
) -> Result<Vec<ChatEventEnvelope>, ChatEventLogError> {
    let mut keys = Vec::new();
    for block in message
        .approved_assistant_blocks
        .as_deref()
        .unwrap_or_default()
    {
        keys.push(native_publication_key("block", block)?);
    }
    if let Some(status) = message.execution_status.as_ref() {
        keys.push(native_publication_key("final", &status.execution_id)?);
        keys.push(native_publication_key("terminal", &status.execution_id)?);
        keys.push(native_publication_key("scope", &status.execution_id)?);
    }
    keys.sort();
    keys.dedup();
    let mut events = Vec::new();
    let mut scope_missing = false;
    for key in keys {
        let records = read_native_publication_anchor(authority, &key, frozen_head)?;
        if key.starts_with("scope-") && records.is_empty() {
            scope_missing = true;
        }
        events.extend(records);
    }
    if let Some(status) = message.execution_status.as_ref()
        && scope_missing
        && events.iter().any(|event| {
            matches!(&event.payload, ChatDriverEvent::Agent(agent)
            if agent.execution_id.as_ref().is_some_and(|id| id.as_str() == status.execution_id))
        })
    {
        // 已有同exec原事实却没scope见证，不能过滤掉跨消息拒绝依据后授权成功。
        return Err(ChatEventLogError::NativeHistoryGap {
            retained_floor: authority.journal.retention_metadata().retained_floor,
            key: "required-execution-scope".into(),
        });
    }
    // product terminal按原outer scope读，绝不用Message/Candidate推断它是否存在。
    if message.execution_status.is_some() {
        let products = events
            .iter()
            .filter(|event| {
                matches!(&event.payload, ChatDriverEvent::Agent(agent)
                if matches!(&agent.payload, echo_agent::agent::AgentEvent::FinalAnswer(_)
                    | echo_agent::agent::AgentEvent::ExecutionStatus(_)))
            })
            .map(native_product_key)
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        for key in products {
            events.extend(read_native_publication_anchor(
                authority,
                &key,
                frozen_head,
            )?);
        }
    }
    events.sort_by_key(|event| event.sequence);
    let mut seen = HashSet::new();
    let mut original = Vec::new();
    for event in events {
        let encoded = echo_agent::utils::canonical_json::canonical_json_bytes(&event)
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        if seen.insert(encoded) {
            original.push(event);
        }
    }
    Ok(original)
}

struct EnumeratedStream {
    stream_id: String,
    path: PathBuf,
    first: ChatEventEnvelope,
}

fn open_stream_authority(
    path: &Path,
    expected_stream_id: &str,
    retention: ChatEventRetention,
) -> Result<StreamAuthority, ChatEventLogError> {
    // Framework ADR0007 verifies the complete committed prefix. Only an
    // unterminated incomplete active-tail frame is discarded; it could not
    // admit an effect through our confirmed InputLifecycle claim barrier.
    let journal = StreamJournal::open(
        path,
        retention.segment_rollover_bytes,
        FileDurability::Flush,
    )
    .map_err(|error| journal_error(path, error))?;
    let pins = RetentionPins::recover(&journal, path, expected_stream_id)?;
    Ok(StreamAuthority {
        expected_stream_id: expected_stream_id.to_string(),
        retention,
        journal,
        pins,
        barrier_pending: false,
        #[cfg(any(test, feature = "channels"))]
        weixin_control_active: false,
    })
}

fn validate_authority_config(
    authority: &StreamAuthority,
    expected_stream_id: &str,
    retention: ChatEventRetention,
    path: &Path,
) -> Result<(), ChatEventLogError> {
    if authority.expected_stream_id != expected_stream_id || authority.retention != retention {
        return Err(corrupt(
            path,
            "chat event stream is already open with a different identity or retention configuration",
        ));
    }
    Ok(())
}

fn validate_authority_storage(
    authority: &StreamAuthority,
    path: &Path,
) -> Result<(), ChatEventLogError> {
    let floor = authority.journal.retention_metadata().retained_floor;
    if let Some(record) = authority
        .journal
        .replay_after(floor.saturating_sub(1), 1)
        .map_err(|error| journal_error(path, error))?
        .first()
    {
        validate_persisted_record(record, path, Some(&authority.expected_stream_id))?;
    }
    Ok(())
}

impl RetentionPins {
    fn recover(
        journal: &StreamJournal,
        path: &Path,
        expected_stream_id: &str,
    ) -> Result<Self, ChatEventLogError> {
        let mut projection = Self::default();
        let mut cursor = journal
            .retention_metadata()
            .retained_floor
            .saturating_sub(1);
        loop {
            let batch = journal
                .replay_after(cursor, REPLAY_BATCH_SIZE)
                .map_err(|error| journal_error(path, error))?;
            if batch.is_empty() {
                return Ok(projection);
            }
            let next_cursor = batch.last().map(|record| record.sequence).unwrap_or(cursor);
            if next_cursor <= cursor {
                return Err(corrupt(
                    path,
                    "framework journal pin recovery did not advance its cursor",
                ));
            }
            for record in &batch {
                validate_persisted_record(record, path, Some(expected_stream_id))?;
                projection.apply(record.sequence, record.event.as_ref());
                #[cfg(test)]
                {
                    projection.recovered_records = projection.recovered_records.saturating_add(1);
                }
            }
            cursor = next_cursor;
            if batch.len() < REPLAY_BATCH_SIZE {
                return Ok(projection);
            }
        }
    }

    fn apply(&mut self, sequence: u64, event: &PersistedChatEvent) {
        self.cursor = sequence;
        if let Some(fact_key) = command_cell_watch_fact_key(&event.payload) {
            self.command_cell_watch_facts
                .entry(fact_key)
                .or_insert(sequence);
        }
        match &event.payload {
            ChatDriverEvent::CommandCellStarted { cell } => {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.active_cells.entry(cell.cell_id.clone())
                {
                    entry.insert(sequence);
                    self.earliest = Some(self.earliest.map_or(sequence, |old| old.min(sequence)));
                }
            }
            ChatDriverEvent::CommandCellSettled { cell } => {
                let removed = self.active_cells.remove(&cell.cell_id);
                self.refresh_earliest_if_removed(removed);
            }
            ChatDriverEvent::CommandCellWatchReady { result } => {
                let key = command_cell_watch_receipt_key(&result.receipt);
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.pending_command_cell_watches.entry(key)
                {
                    entry.insert(sequence);
                    self.earliest = Some(self.earliest.map_or(sequence, |old| old.min(sequence)));
                }
            }
            ChatDriverEvent::CommandCellWatchDeliveryStarted { acknowledgement } => {
                let key = command_cell_watch_ack_key(acknowledgement);
                let removed = self.pending_command_cell_watches.remove(&key);
                self.started_command_cell_watches
                    .entry(key)
                    .or_insert_with(|| (sequence, acknowledgement.clone()));
                self.refresh_earliest_if_removed(removed);
                self.earliest = Some(self.earliest.map_or(sequence, |old| old.min(sequence)));
            }
            ChatDriverEvent::CommandCellWatchAcknowledged { acknowledgement } => {
                let key = command_cell_watch_ack_key(acknowledgement);
                let pending = self.pending_command_cell_watches.remove(&key);
                let started = self
                    .started_command_cell_watches
                    .remove(&key)
                    .map(|(sequence, _)| sequence);
                self.refresh_earliest_if_removed(pending.or(started));
            }
            ChatDriverEvent::WeixinControlDelivery(_) => {
                self.weixin_control_first.get_or_insert(sequence);
                self.refresh_earliest();
            }
            ChatDriverEvent::InputLifecycle(fact) => {
                self.apply_conversation_input_fact(sequence, fact, true);
            }
            _ => {}
        }
    }

    fn earliest(&self) -> Option<u64> {
        self.earliest
    }

    fn discard_before(&mut self, retained_floor: u64) {
        self.command_cell_watch_facts
            .retain(|_, sequence| *sequence >= retained_floor);
        self.pending_command_cell_watches
            .retain(|_, sequence| *sequence >= retained_floor);
        self.started_command_cell_watches
            .retain(|_, (sequence, _)| *sequence >= retained_floor);
        self.active_cells
            .retain(|_, sequence| *sequence >= retained_floor);
        self.conversation_inputs
            .retain(|_, entry| conversation_input_pin_sequence(entry) >= retained_floor);
        self.queue_order
            .retain(|input_id| self.conversation_inputs.contains_key(input_id));
        self.refresh_earliest();
    }

    fn refresh_earliest_if_removed(&mut self, removed: Option<u64>) {
        if removed.is_some_and(|sequence| self.earliest == Some(sequence)) {
            self.refresh_earliest();
        }
    }

    fn refresh_earliest(&mut self) {
        let conversation_input_earliest = self
            .conversation_inputs
            .values()
            .map(conversation_input_pin_sequence)
            .min();
        self.earliest = self
            .pending_command_cell_watches
            .values()
            .chain(
                self.started_command_cell_watches
                    .values()
                    .map(|(sequence, _)| sequence),
            )
            .chain(self.active_cells.values())
            .chain(conversation_input_earliest.iter())
            .chain(self.weixin_control_first.iter())
            .copied()
            .min();
    }

    fn apply_conversation_input_fact(
        &mut self,
        sequence: u64,
        fact: &ConversationInputFact,
        terminal_fact_self_contained: bool,
    ) {
        self.queue_revision = self.queue_revision.max(sequence);
        match fact {
            ConversationInputFact::Persisted { identity, payload } => {
                let previous_position = self
                    .queue_order
                    .iter()
                    .position(|queued| queued == &identity.input_id);
                let previous_first_sequence = self
                    .conversation_inputs
                    .get(&identity.input_id)
                    .map(|entry| entry.first_sequence);
                let receipt = ConversationInputReceipt {
                    identity: identity.clone(),
                    phase: ConversationInputPhase::Persisted,
                    attempt: None,
                    attempt_id: None,
                    turn_id: None,
                    outcome: None,
                    drained: false,
                    reason: None,
                    duplicate: false,
                    queue_revision: self.queue_revision,
                };
                self.conversation_inputs.insert(
                    identity.input_id.clone(),
                    FoldedConversationInput {
                        projection: ConversationInputProjection {
                            receipt,
                            payload: payload.as_ref().clone(),
                            task_target: self
                                .conversation_inputs
                                .get(&identity.input_id)
                                .and_then(|entry| entry.projection.task_target.clone()),
                            claimed_before_current: self
                                .conversation_inputs
                                .get(&identity.input_id)
                                .is_some_and(|entry| entry.ever_claimed),
                            active_attempt: None,
                        },
                        first_sequence: previous_first_sequence.unwrap_or(sequence),
                        last_sequence: sequence,
                        terminal_fact_self_contained: false,
                        ever_claimed: self
                            .conversation_inputs
                            .get(&identity.input_id)
                            .is_some_and(|entry| entry.ever_claimed),
                    },
                );
                self.queue_order
                    .retain(|queued| queued != &identity.input_id);
                if let Some(position) = previous_position {
                    self.queue_order.insert(
                        position.min(self.queue_order.len()),
                        identity.input_id.clone(),
                    );
                } else {
                    self.queue_order.push(identity.input_id.clone());
                }
            }
            ConversationInputFact::AttemptStarted { attempt, .. } => {
                if let Some(entry) = self.conversation_inputs.get_mut(&attempt.identity.input_id) {
                    entry.projection.claimed_before_current = entry.ever_claimed;
                    entry.ever_claimed = true;
                    entry.projection.receipt.phase = ConversationInputPhase::AttemptStarted;
                    entry.projection.receipt.attempt = Some(attempt.attempt);
                    entry.projection.receipt.attempt_id = Some(attempt.attempt_id.clone());
                    entry.projection.receipt.turn_id = Some(attempt.turn_id.clone());
                    entry.projection.receipt.outcome = None;
                    entry.projection.receipt.drained = false;
                    entry.projection.receipt.reason = None;
                    entry.projection.receipt.duplicate = false;
                    entry.last_sequence = sequence;
                    entry.projection.active_attempt = Some(attempt.clone());
                }
                self.queue_order
                    .retain(|queued| queued != &attempt.identity.input_id);
                self.queue_order
                    .insert(0, attempt.identity.input_id.clone());
            }
            ConversationInputFact::TaskRunTargetBound {
                attempt, run_id, ..
            } => {
                if let Some(entry) = self.conversation_inputs.get_mut(&attempt.identity.input_id) {
                    let receipt = &entry.projection.receipt;
                    let exact = receipt.phase == ConversationInputPhase::AttemptStarted
                        && receipt.identity == attempt.identity
                        && receipt.attempt == Some(attempt.attempt)
                        && receipt.attempt_id.as_deref() == Some(attempt.attempt_id.as_str())
                        && receipt.turn_id.as_deref() == Some(attempt.turn_id.as_str())
                        && !entry.projection.task_target.as_ref().is_some_and(|target| {
                            target.run_id != *run_id
                                || target.turn_id != attempt.turn_id
                                || target.identity != attempt.identity
                        });
                    if exact {
                        entry.projection.task_target =
                            Some(crate::conversation_input::ConversationInputTaskTarget {
                                run_id: run_id.clone(),
                                turn_id: attempt.turn_id.clone(),
                                identity: attempt.identity.clone(),
                            });
                    } else {
                        entry.projection.receipt.phase = ConversationInputPhase::RecoveryRequired;
                        entry.projection.receipt.reason =
                            Some("原输入任务归属事实与原attempt冲突，禁止重投".to_string());
                    }
                    entry.last_sequence = sequence;
                }
            }
            ConversationInputFact::MailboxAccepted { attempt, .. } => {
                self.update_attempt_receipt(
                    sequence,
                    attempt,
                    ConversationInputPhase::MailboxAccepted,
                    None,
                    false,
                    None,
                );
            }
            ConversationInputFact::Drained { attempt, .. }
            | ConversationInputFact::DriverAccepted { attempt, .. } => {
                self.ensure_terminal_tombstone(
                    sequence,
                    &attempt.identity,
                    terminal_fact_self_contained,
                );
                self.update_attempt_receipt(
                    sequence,
                    attempt,
                    ConversationInputPhase::Drained,
                    None,
                    true,
                    None,
                );
                self.queue_order
                    .retain(|queued| queued != &attempt.identity.input_id);
                if let Some(entry) = self.conversation_inputs.get_mut(&attempt.identity.input_id) {
                    entry.terminal_fact_self_contained = terminal_fact_self_contained;
                }
            }
            ConversationInputFact::TurnSettled {
                attempt,
                outcome,
                drained,
                ..
            } => {
                if *drained {
                    self.ensure_terminal_tombstone(
                        sequence,
                        &attempt.identity,
                        terminal_fact_self_contained,
                    );
                }
                self.update_attempt_receipt(
                    sequence,
                    attempt,
                    ConversationInputPhase::TurnSettled,
                    Some(*outcome),
                    *drained,
                    None,
                );
                if *drained {
                    self.queue_order
                        .retain(|queued| queued != &attempt.identity.input_id);
                    if let Some(entry) =
                        self.conversation_inputs.get_mut(&attempt.identity.input_id)
                    {
                        entry.terminal_fact_self_contained = terminal_fact_self_contained;
                    }
                } else if !self.queue_order.contains(&attempt.identity.input_id) {
                    self.queue_order.push(attempt.identity.input_id.clone());
                }
            }
            ConversationInputFact::Deferred {
                attempt, reason, ..
            } => {
                self.update_attempt_receipt(
                    sequence,
                    attempt,
                    ConversationInputPhase::Deferred,
                    None,
                    false,
                    Some(reason.clone()),
                );
            }
            ConversationInputFact::RecoveryRequired {
                attempt,
                reason,
                drained,
                ..
            } => {
                self.update_attempt_receipt(
                    sequence,
                    attempt,
                    ConversationInputPhase::RecoveryRequired,
                    None,
                    *drained,
                    Some(reason.clone()),
                );
            }
            ConversationInputFact::RecoveryUncertain {
                identity, reason, ..
            } => {
                if let Some(entry) = self.conversation_inputs.get_mut(&identity.input_id)
                    // A terminal cancellation is authoritative: a delayed
                    // uncertainty fact from an older owner cannot turn it back
                    // into a blocking recovery item.
                    && entry.projection.receipt.phase != ConversationInputPhase::Cancelled
                {
                    entry.projection.receipt.phase = ConversationInputPhase::RecoveryRequired;
                    entry.projection.receipt.reason = Some(reason.clone());
                    entry.projection.receipt.duplicate = false;
                    entry.last_sequence = sequence;
                }
            }
            ConversationInputFact::Cancelled {
                identity,
                attempt,
                drained,
                reason,
                ..
            } => {
                self.ensure_terminal_tombstone(sequence, identity, terminal_fact_self_contained);
                if let Some(entry) = self.conversation_inputs.get_mut(&identity.input_id) {
                    entry.projection.receipt.phase = ConversationInputPhase::Cancelled;
                    entry.projection.receipt.outcome = Some(ConversationInputOutcome::Cancelled);
                    entry.projection.receipt.reason = reason.clone();
                    entry.projection.receipt.duplicate = false;
                    entry.projection.receipt.drained = *drained;
                    if let Some(attempt) = attempt {
                        entry.projection.receipt.attempt = Some(attempt.attempt);
                        entry.projection.receipt.attempt_id = Some(attempt.attempt_id.clone());
                        entry.projection.receipt.turn_id = Some(attempt.turn_id.clone());
                    }
                    entry.last_sequence = sequence;
                    entry.terminal_fact_self_contained = terminal_fact_self_contained;
                }
                self.queue_order
                    .retain(|queued| queued != &identity.input_id);
            }
            ConversationInputFact::Reordered { input_ids, .. } => {
                let mut next = Vec::with_capacity(self.queue_order.len());
                for input_id in input_ids {
                    if self.conversation_inputs.get(input_id).is_some_and(|entry| {
                        conversation_input_is_frontier(&entry.projection.receipt)
                    }) && !next.contains(input_id)
                    {
                        next.push(input_id.clone());
                    }
                }
                for input_id in &self.queue_order {
                    if self.conversation_inputs.get(input_id).is_some_and(|entry| {
                        conversation_input_is_frontier(&entry.projection.receipt)
                    }) && !next.contains(input_id)
                    {
                        next.push(input_id.clone());
                    }
                }
                self.queue_order = next;
            }
        }
        self.refresh_earliest();
    }

    fn update_attempt_receipt(
        &mut self,
        sequence: u64,
        attempt: &ConversationInputAttempt,
        phase: ConversationInputPhase,
        outcome: Option<ConversationInputOutcome>,
        drained: bool,
        reason: Option<String>,
    ) {
        if let Some(entry) = self.conversation_inputs.get_mut(&attempt.identity.input_id) {
            entry.ever_claimed = true;
            entry.projection.receipt.phase = phase;
            entry.projection.receipt.attempt = Some(attempt.attempt);
            entry.projection.receipt.attempt_id = Some(attempt.attempt_id.clone());
            entry.projection.receipt.turn_id = Some(attempt.turn_id.clone());
            entry.projection.receipt.outcome = outcome;
            entry.projection.receipt.drained = drained;
            entry.projection.receipt.reason = reason;
            entry.projection.receipt.duplicate = false;
            entry.last_sequence = sequence;
        }
    }

    fn ensure_terminal_tombstone(
        &mut self,
        sequence: u64,
        identity: &ConversationInputIdentity,
        terminal_fact_self_contained: bool,
    ) {
        self.conversation_inputs
            .entry(identity.input_id.clone())
            .or_insert_with(|| FoldedConversationInput {
                projection: ConversationInputProjection {
                    receipt: ConversationInputReceipt {
                        identity: identity.clone(),
                        phase: ConversationInputPhase::RecoveryRequired,
                        attempt: None,
                        attempt_id: None,
                        turn_id: None,
                        outcome: None,
                        drained: false,
                        reason: Some("recovered terminal tombstone".to_string()),
                        duplicate: false,
                        queue_revision: self.queue_revision,
                    },
                    payload: ConversationInputPayload {
                        channel_binding: None,
                        domain_plugin: None,
                        text: String::new(),
                        attachments: Vec::new(),
                        submitted_at_ms: 0,
                        payload_sha256: identity.payload_sha256.clone(),
                    },
                    task_target: None,
                    claimed_before_current: true,
                    active_attempt: None,
                },
                first_sequence: sequence,
                last_sequence: sequence,
                terminal_fact_self_contained,
                ever_claimed: true,
            });
    }
}

fn conversation_input_is_frontier(receipt: &ConversationInputReceipt) -> bool {
    !matches!(
        receipt.phase,
        ConversationInputPhase::Drained | ConversationInputPhase::Cancelled
    ) && !(receipt.phase == ConversationInputPhase::TurnSettled && receipt.drained)
}

fn conversation_input_pin_sequence(entry: &FoldedConversationInput) -> u64 {
    // A provider may replay after ACK loss. Preserve the original binding AND
    // its complete lifecycle; a pruned tombstone is not a never-claimed proof.
    if entry.projection.payload.channel_binding.is_some()
        || conversation_input_is_frontier(&entry.projection.receipt)
    {
        entry.first_sequence
    } else if entry.terminal_fact_self_contained {
        entry.last_sequence
    } else {
        entry.first_sequence
    }
}

fn conversation_input_order_from_authority(authority: &StreamAuthority) -> Vec<String> {
    authority
        .pins
        .queue_order
        .iter()
        .filter(|input_id| {
            authority
                .pins
                .conversation_inputs
                .get(*input_id)
                .is_some_and(|entry| conversation_input_is_frontier(&entry.projection.receipt))
        })
        .cloned()
        .collect()
}

fn normalized_conversation_input_projection(
    entry: &FoldedConversationInput,
    queue_revision: u64,
) -> ConversationInputProjection {
    let mut projection = entry.projection.clone();
    projection.receipt.queue_revision = queue_revision;
    projection
}

fn normalized_conversation_input_receipt(
    entry: &FoldedConversationInput,
    queue_revision: u64,
) -> ConversationInputReceipt {
    normalized_conversation_input_projection(entry, queue_revision).receipt
}

fn conversation_input_attempt_from_receipt(
    receipt: &ConversationInputReceipt,
) -> Option<ConversationInputAttempt> {
    Some(ConversationInputAttempt {
        identity: receipt.identity.clone(),
        attempt: receipt.attempt?,
        attempt_id: receipt.attempt_id.clone()?,
        turn_id: receipt.turn_id.clone()?,
        observation: Default::default(),
    })
}

fn conversation_input_fact_is_duplicate(
    current: &ConversationInputProjection,
    fact: &ConversationInputFact,
) -> bool {
    let receipt = &current.receipt;
    let exact_attempt = |attempt: &ConversationInputAttempt| {
        receipt.identity == attempt.identity
            && receipt.attempt == Some(attempt.attempt)
            && receipt.attempt_id.as_deref() == Some(attempt.attempt_id.as_str())
            && receipt.turn_id.as_deref() == Some(attempt.turn_id.as_str())
    };
    match fact {
        ConversationInputFact::Persisted { identity, payload } => {
            receipt.identity == *identity
                && current.payload.payload_sha256 == payload.payload_sha256
        }
        ConversationInputFact::AttemptStarted { attempt, .. } => {
            exact_attempt(attempt)
                && matches!(
                    receipt.phase,
                    ConversationInputPhase::AttemptStarted
                        | ConversationInputPhase::MailboxAccepted
                        | ConversationInputPhase::Drained
                        | ConversationInputPhase::TurnSettled
                        | ConversationInputPhase::RecoveryRequired
                )
        }
        ConversationInputFact::TaskRunTargetBound {
            attempt, run_id, ..
        } => {
            receipt.phase == ConversationInputPhase::AttemptStarted
                && exact_attempt(attempt)
                && current.task_target.as_ref().is_some_and(|target| {
                    target.run_id == *run_id
                        && target.turn_id == attempt.turn_id
                        && target.identity == attempt.identity
                })
        }
        ConversationInputFact::MailboxAccepted { attempt, .. } => {
            exact_attempt(attempt)
                && matches!(
                    receipt.phase,
                    ConversationInputPhase::MailboxAccepted
                        | ConversationInputPhase::Drained
                        | ConversationInputPhase::TurnSettled
                )
        }
        ConversationInputFact::Drained { attempt, .. }
        | ConversationInputFact::DriverAccepted { attempt, .. } => {
            exact_attempt(attempt) && receipt.drained
        }
        ConversationInputFact::TurnSettled {
            attempt,
            outcome,
            drained,
            ..
        } => {
            exact_attempt(attempt)
                && receipt.phase == ConversationInputPhase::TurnSettled
                && receipt.outcome == Some(*outcome)
                && receipt.drained == *drained
        }
        ConversationInputFact::Deferred {
            attempt, reason, ..
        } => {
            exact_attempt(attempt)
                && receipt.phase == ConversationInputPhase::Deferred
                && receipt.reason.as_deref() == Some(reason.as_str())
        }
        ConversationInputFact::RecoveryRequired {
            attempt,
            reason,
            drained,
            ..
        } => {
            exact_attempt(attempt)
                && receipt.phase == ConversationInputPhase::RecoveryRequired
                && receipt.reason.as_deref() == Some(reason.as_str())
                && receipt.drained == *drained
        }
        ConversationInputFact::RecoveryUncertain {
            identity, reason, ..
        } => {
            receipt.identity == *identity
                && receipt.phase == ConversationInputPhase::RecoveryRequired
                && receipt.reason.as_deref() == Some(reason.as_str())
        }
        ConversationInputFact::Cancelled {
            identity,
            attempt,
            drained,
            reason,
            ..
        } => {
            receipt.identity == *identity
                && receipt.phase == ConversationInputPhase::Cancelled
                && attempt.as_ref() == conversation_input_attempt_from_receipt(receipt).as_ref()
                && receipt.drained == *drained
                && receipt.reason == *reason
        }
        ConversationInputFact::Reordered { input_ids, .. } => {
            conversation_input_order_from_projection(current).as_slice() == input_ids.as_slice()
        }
    }
}

fn conversation_input_order_from_projection(current: &ConversationInputProjection) -> Vec<String> {
    vec![current.receipt.identity.input_id.clone()]
}

fn envelope_from_record(
    record: JournalRecord<PersistedChatEvent>,
    path: &Path,
    expected_stream_id: &str,
) -> Result<ChatEventEnvelope, ChatEventLogError> {
    validate_persisted_record(&record, path, Some(expected_stream_id))?;
    envelope_from_validated_record(record, path)
}

fn envelope_from_record_for_enumeration(
    record: JournalRecord<PersistedChatEvent>,
    path: &Path,
) -> Result<ChatEventEnvelope, ChatEventLogError> {
    validate_persisted_record(&record, path, None)?;
    let expected_directory = digest(record.event.stream_id.as_bytes());
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_directory.as_str()) {
        return Err(corrupt(
            path,
            "chat event directory does not match its persisted stream identity",
        ));
    }
    envelope_from_validated_record(record, path)
}

fn validate_persisted_record(
    record: &JournalRecord<PersistedChatEvent>,
    path: &Path,
    expected_stream_id: Option<&str>,
) -> Result<(), ChatEventLogError> {
    let persisted = record.event.as_ref();
    if persisted.schema_version != CHAT_EVENT_SCHEMA_VERSION {
        return Err(corrupt(
            path,
            format!("unsupported schema version {}", persisted.schema_version),
        ));
    }
    validate_driver_event(&persisted.payload).map_err(|error| corrupt(path, error.to_string()))?;
    validate_event_stream_identity(
        &persisted.workspace_id,
        persisted.conversation_id.as_deref(),
        &persisted.root_turn_id,
        &persisted.payload,
    )
    .map_err(|error| corrupt(path, error.to_string()))?;
    let derived_stream = stream_id(
        &persisted.workspace_id,
        persisted.conversation_id.as_deref(),
        &persisted.root_turn_id,
    )
    .map_err(|error| corrupt(path, error.to_string()))?;
    let (expected_turn_id, expected_message_id) =
        event_identity(&persisted.payload, &persisted.root_turn_id);
    if persisted.stream_id != derived_stream
        || expected_stream_id.is_some_and(|expected| persisted.stream_id != expected)
        || persisted.turn_id != expected_turn_id
        || persisted.message_id != expected_message_id
        || persisted.message_id != persisted.root_turn_id
    {
        return Err(corrupt(
            path,
            "persisted chat identity does not match its payload, directory, or stream",
        ));
    }
    Ok(())
}

fn envelope_from_validated_record(
    record: JournalRecord<PersistedChatEvent>,
    path: &Path,
) -> Result<ChatEventEnvelope, ChatEventLogError> {
    let sequence = record.sequence;
    let persisted = Arc::try_unwrap(record.event).map_err(|_| {
        corrupt(
            path,
            "framework journal record payload was unexpectedly shared during projection",
        )
    })?;
    let content_hash = envelope_content_hash(EnvelopeIntegrity {
        schema_version: CHAT_EVENT_SCHEMA_VERSION,
        sequence,
        stream_id: &persisted.stream_id,
        workspace_id: &persisted.workspace_id,
        conversation_id: persisted.conversation_id.as_deref(),
        root_turn_id: &persisted.root_turn_id,
        turn_id: &persisted.turn_id,
        message_id: &persisted.message_id,
        timestamp: persisted.timestamp,
        payload: &persisted.payload,
    })?;
    Ok(ChatEventEnvelope {
        schema_version: CHAT_EVENT_SCHEMA_VERSION,
        event_id: stable_event_id(&persisted.stream_id, sequence, &content_hash),
        content_hash,
        sequence,
        stream_id: persisted.stream_id,
        workspace_id: persisted.workspace_id,
        conversation_id: persisted.conversation_id,
        root_turn_id: persisted.root_turn_id,
        turn_id: persisted.turn_id,
        message_id: persisted.message_id,
        timestamp: persisted.timestamp,
        payload: persisted.payload,
    })
}

fn empty_replay() -> ChatEventReplay {
    ChatEventReplay {
        events: Vec::new(),
        retained_earliest_cursor: None,
        returned_earliest_cursor: None,
        latest_cursor: 0,
        truncated: false,
    }
}

fn corrupt(path: &Path, message: impl Into<String>) -> ChatEventLogError {
    ChatEventLogError::Corrupt {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn journal_error(path: &Path, error: impl std::fmt::Display) -> ChatEventLogError {
    corrupt(path, error.to_string())
}

fn lock_cached_stream(stream: &CachedStreamJournal) -> MutexGuard<'_, Option<StreamAuthority>> {
    stream.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("chat event stream lock was poisoned; recovering authority");
        poisoned.into_inner()
    })
}

fn should_maintain_retention(requested: FileDurability, status: &JournalDurabilityStatus) -> bool {
    matches!(requested, FileDurability::SyncData)
        && matches!(status, JournalDurabilityStatus::Confirmed)
}

fn should_mark_barrier_pending(
    requested: FileDurability,
    status: &JournalDurabilityStatus,
) -> bool {
    matches!(requested, FileDurability::SyncData)
        && matches!(status, JournalDurabilityStatus::Degraded { .. })
}

fn append_durability(event: &ChatDriverEvent) -> FileDurability {
    match event {
        ChatDriverEvent::Agent(envelope) => match &envelope.payload {
            echo_agent::agent::AgentEvent::ToolCall { .. }
            | echo_agent::agent::AgentEvent::ToolResult { .. }
            | echo_agent::agent::AgentEvent::FinalAnswer(_)
            | echo_agent::agent::AgentEvent::Cancelled
            | echo_agent::agent::AgentEvent::Error { .. }
            | echo_agent::agent::AgentEvent::ContextCompressed { .. } => FileDurability::SyncData,
            _ => FileDurability::Flush,
        },
        ChatDriverEvent::TurnStatus { status }
            if matches!(status.as_str(), "completed" | "failed" | "cancelled") =>
        {
            FileDurability::SyncData
        }
        ChatDriverEvent::Execution(event)
            if matches!(
                event.event,
                crate::tasks::task_runtime::types::RuntimeEventKind::Running
                    | crate::tasks::task_runtime::types::RuntimeEventKind::ThinkingStarted
                    | crate::tasks::task_runtime::types::RuntimeEventKind::ThinkingDelta
                    | crate::tasks::task_runtime::types::RuntimeEventKind::ThinkingEnded
                    | crate::tasks::task_runtime::types::RuntimeEventKind::TokenDelta
                    | crate::tasks::task_runtime::types::RuntimeEventKind::Usage
                    | crate::tasks::task_runtime::types::RuntimeEventKind::ToolOutput
                    | crate::tasks::task_runtime::types::RuntimeEventKind::Note
            ) =>
        {
            FileDurability::Flush
        }
        ChatDriverEvent::Execution(_)
        | ChatDriverEvent::ExecutionPath { .. }
        | ChatDriverEvent::TurnConfiguration { .. }
        | ChatDriverEvent::ExtensionReceipt(_)
        | ChatDriverEvent::Interrupt { .. }
        | ChatDriverEvent::CommandCellStarted { .. }
        | ChatDriverEvent::CommandCellSettled { .. }
        | ChatDriverEvent::CommandCellWatchReady { .. }
        | ChatDriverEvent::CommandCellWatchDeliveryStarted { .. }
        | ChatDriverEvent::CommandCellWatchAcknowledged { .. }
        | ChatDriverEvent::WeixinControlDelivery(_)
        | ChatDriverEvent::InputLifecycle(_)
        | ChatDriverEvent::ApprovalRequest { .. }
        | ChatDriverEvent::InputRequest { .. }
        | ChatDriverEvent::SelectionRequest { .. }
        | ChatDriverEvent::HitlRequestReleased { .. }
        | ChatDriverEvent::ContextCompressed { .. } => FileDurability::SyncData,
        ChatDriverEvent::TurnStatus { .. } => FileDurability::Flush,
    }
}

fn ensure_real_directory(path: &Path, create: bool) -> Result<bool, ChatEventLogError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound && !create => {
            return Ok(false);
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|source| ChatEventLogError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            fs::symlink_metadata(path).map_err(|source| ChatEventLogError::Io {
                path: path.to_path_buf(),
                source,
            })?
        }
        Err(source) => {
            return Err(ChatEventLogError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(corrupt(
            path,
            "chat event directory path is not a real directory",
        ));
    }
    Ok(true)
}

fn stream_id(
    workspace_id: &str,
    conversation_id: Option<&str>,
    root_turn_id: &str,
) -> Result<String, ChatEventLogError> {
    if workspace_id.trim().is_empty() || root_turn_id.trim().is_empty() {
        return Err(ChatEventLogError::InvalidIdentity(
            "workspace_id and root_turn_id must not be empty".to_string(),
        ));
    }
    if conversation_id.is_some_and(|value| value.trim().is_empty()) {
        return Err(ChatEventLogError::InvalidIdentity(
            "conversation_id must not be empty".to_string(),
        ));
    }
    match conversation_id {
        Some(conversation_id) => serde_json::to_string(&(workspace_id, conversation_id)),
        None => serde_json::to_string(&(workspace_id, root_turn_id)),
    }
    .map_err(|error| ChatEventLogError::Serialization(error.to_string()))
}

fn event_identity(event: &ChatDriverEvent, root_turn_id: &str) -> (String, String) {
    match event {
        ChatDriverEvent::ApprovalRequest {
            identity: Some(identity),
            ..
        }
        | ChatDriverEvent::InputRequest {
            identity: Some(identity),
            ..
        }
        | ChatDriverEvent::SelectionRequest {
            identity: Some(identity),
            ..
        }
        | ChatDriverEvent::HitlRequestReleased { identity } => (
            identity
                .scope
                .turn_id
                .clone()
                .unwrap_or_else(|| root_turn_id.to_string()),
            root_turn_id.to_string(),
        ),
        ChatDriverEvent::Agent(envelope) => (
            envelope.turn_id.as_str().to_string(),
            envelope
                .message_id
                .as_ref()
                .map(|message_id| message_id.as_str().to_string())
                .unwrap_or_else(|| root_turn_id.to_string()),
        ),
        ChatDriverEvent::Execution(event) => event
            .framework_event
            .as_ref()
            .map(|metadata| {
                (
                    metadata.turn_id.clone(),
                    metadata
                        .message_id
                        .clone()
                        .unwrap_or_else(|| root_turn_id.to_string()),
                )
            })
            .unwrap_or_else(|| (root_turn_id.to_string(), root_turn_id.to_string())),
        _ => (root_turn_id.to_string(), root_turn_id.to_string()),
    }
}

fn command_cell_watch_receipt_key(
    receipt: &crate::tasks::task_runtime::command_cells::CommandCellWatchReceipt,
) -> String {
    format!("{}:{}", receipt.execution_id, receipt.watch_generation)
}

fn command_cell_watch_ack_key(
    acknowledgement: &crate::tasks::task_runtime::command_cells::CommandCellWatchAcknowledgement,
) -> String {
    format!(
        "{}:{}",
        acknowledgement.execution_id, acknowledgement.watch_generation
    )
}

fn command_cell_watch_fact_key(event: &ChatDriverEvent) -> Option<String> {
    match event {
        ChatDriverEvent::CommandCellStarted { cell } => {
            Some(format!("cell_started:{}", cell.cell_id))
        }
        ChatDriverEvent::CommandCellSettled { cell } => {
            Some(format!("cell_settled:{}", cell.cell_id))
        }
        ChatDriverEvent::CommandCellWatchReady { result } => Some(format!(
            "ready:{}",
            command_cell_watch_receipt_key(&result.receipt)
        )),
        ChatDriverEvent::CommandCellWatchDeliveryStarted { acknowledgement } => Some(format!(
            "started:{}",
            command_cell_watch_ack_key(acknowledgement)
        )),
        ChatDriverEvent::CommandCellWatchAcknowledged { acknowledgement } => Some(format!(
            "ack:{}",
            command_cell_watch_ack_key(acknowledgement)
        )),
        _ => None,
    }
}

fn validate_event_stream_identity(
    workspace_id: &str,
    conversation_id: Option<&str>,
    root_turn_id: &str,
    event: &ChatDriverEvent,
) -> Result<(), ChatEventLogError> {
    match event {
        ChatDriverEvent::ApprovalRequest {
            identity: Some(identity),
            ..
        }
        | ChatDriverEvent::InputRequest {
            identity: Some(identity),
            ..
        }
        | ChatDriverEvent::SelectionRequest {
            identity: Some(identity),
            ..
        }
        | ChatDriverEvent::HitlRequestReleased { identity } => {
            let request_matches = match event {
                ChatDriverEvent::ApprovalRequest { request_id, .. } => {
                    request_id == &identity.request_id
                        && identity.kind == crate::hitl::HitlRequestKind::Approval
                }
                ChatDriverEvent::InputRequest { request_id, .. } => {
                    request_id == &identity.request_id
                        && identity.kind == crate::hitl::HitlRequestKind::Input
                }
                ChatDriverEvent::SelectionRequest { request_id, .. } => {
                    request_id == &identity.request_id
                        && identity.kind == crate::hitl::HitlRequestKind::Selection
                }
                ChatDriverEvent::HitlRequestReleased { .. } => true,
                _ => false,
            };
            if !request_matches
                || identity.request_id.trim().is_empty()
                || identity.scope.workspace_id.as_deref() != Some(workspace_id)
                || identity.scope.conversation_id.as_deref() != conversation_id
                || identity.scope.root_turn_id.as_deref() != Some(root_turn_id)
                || identity
                    .scope
                    .turn_id
                    .as_deref()
                    .is_none_or(|turn| turn.trim().is_empty())
                || identity.response_target.trim().is_empty()
            {
                return Err(ChatEventLogError::InvalidIdentity(
                    "HITL identity does not match the original journal owner".to_string(),
                ));
            }
        }
        ChatDriverEvent::Agent(envelope) => {
            let event_conversation_id = envelope
                .conversation_id
                .as_ref()
                .map(|identity| identity.as_str());
            if event_conversation_id != conversation_id {
                return Err(ChatEventLogError::InvalidIdentity(format!(
                    "framework envelope conversation {event_conversation_id:?} does not match journal stream {conversation_id:?}"
                )));
            }
        }
        ChatDriverEvent::Execution(execution) => {
            if execution.workspace_id != workspace_id
                || Some(execution.conversation_id.as_str()) != conversation_id
            {
                return Err(ChatEventLogError::InvalidIdentity(
                    "execution event address does not match journal stream".to_string(),
                ));
            }
            if let Some(metadata) = execution.framework_event.as_ref()
                && (metadata
                    .message_id
                    .as_deref()
                    .is_some_and(|id| id != root_turn_id)
                    || execution.subagent_run_id.as_deref() != Some(metadata.execution_id.as_str())
                    || execution.task_id.as_deref() != metadata.task_id.as_deref()
                    || execution.agent.as_deref() != Some(metadata.agent_name.as_str()))
            {
                return Err(ChatEventLogError::InvalidIdentity(
                    "framework Subagent metadata does not match its EKO execution event"
                        .to_string(),
                ));
            }
        }
        ChatDriverEvent::InputLifecycle(fact)
            if fact.identity().address.workspace_id != workspace_id
                || Some(fact.identity().address.conversation_id.as_str()) != conversation_id =>
        {
            return Err(ChatEventLogError::InvalidIdentity(
                "conversation input fact address does not match journal stream".to_string(),
            ));
        }
        ChatDriverEvent::CommandCellWatchReady { result }
            if result.receipt.workspace_id != workspace_id
                || Some(result.receipt.conversation_id.as_str()) != conversation_id
                || result.receipt.root_turn_id != root_turn_id =>
        {
            return Err(ChatEventLogError::InvalidIdentity(
                "command-cell-watch Ready address does not match journal stream".to_string(),
            ));
        }
        ChatDriverEvent::CommandCellWatchDeliveryStarted { acknowledgement }
        | ChatDriverEvent::CommandCellWatchAcknowledged { acknowledgement }
            if acknowledgement.workspace_id != workspace_id
                || Some(acknowledgement.conversation_id.as_str()) != conversation_id
                || acknowledgement.root_turn_id != root_turn_id =>
        {
            return Err(ChatEventLogError::InvalidIdentity(
                "command-cell-watch acknowledgement address does not match journal stream"
                    .to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn validate_driver_event(event: &ChatDriverEvent) -> Result<(), ChatEventLogError> {
    if let ChatDriverEvent::Agent(envelope) = event {
        if envelope.schema_version != echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION {
            return Err(ChatEventLogError::InvalidEvent(format!(
                "unsupported framework event schema version {}",
                envelope.schema_version
            )));
        }
        if envelope.sequence == 0
            || envelope.event_id.as_str().trim().is_empty()
            || envelope.content_hash.trim().is_empty()
            || envelope.stream_id.as_str().trim().is_empty()
            || envelope.turn_id.as_str().trim().is_empty()
        {
            return Err(ChatEventLogError::InvalidEvent(
                "framework event identity, hash, and sequence must be populated".to_string(),
            ));
        }
    }
    if let ChatDriverEvent::TurnStatus { status } = event
        && !matches!(
            status.as_str(),
            "idle"
                | "running"
                | "thinking"
                | "using_tool"
                | "waiting_approval"
                | "waiting_input"
                | "completed"
                | "failed"
                | "cancelled"
        )
    {
        return Err(ChatEventLogError::InvalidEvent(format!(
            "unknown turn status {status:?} for chat event schema {CHAT_EVENT_SCHEMA_VERSION}"
        )));
    }
    if let ChatDriverEvent::InputLifecycle(fact) = event {
        validate_conversation_input_event(fact)?;
    }
    match event {
        ChatDriverEvent::CommandCellStarted { cell }
            if cell.cell_id.trim().is_empty() || !cell.is_active() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "command-cell Started fact must have an active typed state".to_string(),
            ))
        }
        ChatDriverEvent::CommandCellSettled { cell }
            if cell.cell_id.trim().is_empty() || cell.is_active() || cell.finished_at.is_none() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "command-cell terminal fact must have a settled typed state".to_string(),
            ))
        }
        ChatDriverEvent::CommandCellWatchReady { result }
            if result.receipt.execution_id.trim().is_empty()
                || result.receipt.watch_generation == 0
                || !matches!(
                    result.receipt.state,
                    crate::tasks::task_runtime::command_cells::CommandCellWatchState::Settled
                        | crate::tasks::task_runtime::command_cells::CommandCellWatchState::Cancelled
                )
                || result.receipt.settled_at.is_none()
                || result.receipt.cell_id != result.cell.cell_id
                || result.cell.is_active() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "CommandCellWatch Ready fact requires exact receipt identity and terminal cell truth"
                    .to_string(),
            ))
        }
        ChatDriverEvent::CommandCellWatchDeliveryStarted { acknowledgement }
        | ChatDriverEvent::CommandCellWatchAcknowledged { acknowledgement }
            if acknowledgement.execution_id.trim().is_empty()
                || acknowledgement.watch_generation == 0
                || acknowledgement.workspace_id.trim().is_empty()
                || acknowledgement.conversation_id.trim().is_empty()
                || acknowledgement.root_turn_id.trim().is_empty()
                || acknowledgement.acknowledged_turn_id.trim().is_empty() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "CommandCellWatch acknowledgement identity is incomplete".to_string(),
            ))
        }
        ChatDriverEvent::ExtensionReceipt(receipt)
            if receipt.meta().request_id.trim().is_empty()
                || receipt.meta().operation_id.trim().is_empty() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "Extension receipt identity is incomplete".to_string(),
            ))
        }
        _ => Ok(()),
    }
}

fn validate_conversation_input_event(
    fact: &ConversationInputFact,
) -> Result<(), ChatEventLogError> {
    let identity = fact.identity();
    if identity.address.workspace_id.trim().is_empty()
        || identity.address.conversation_id.trim().is_empty()
        || identity.input_id.trim().is_empty()
        || identity.revision == 0
    {
        return Err(ChatEventLogError::InvalidEvent(
            "conversation input fact identity is incomplete".to_string(),
        ));
    }
    if let ConversationInputFact::Persisted { payload, .. } = fact {
        payload
            .validate_channel_digest(&identity.address)
            .map_err(|error| ChatEventLogError::InvalidEvent(error.to_string()))?;
    }
    match fact {
        ConversationInputFact::Persisted { payload, .. }
            if (payload.text.trim().is_empty() && payload.attachments.is_empty())
                || payload.payload_sha256.trim().is_empty() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "persisted conversation input payload is incomplete".to_string(),
            ))
        }
        ConversationInputFact::AttemptStarted { attempt, .. }
        | ConversationInputFact::TaskRunTargetBound { attempt, .. }
        | ConversationInputFact::DriverAccepted { attempt, .. }
        | ConversationInputFact::MailboxAccepted { attempt, .. }
        | ConversationInputFact::Drained { attempt, .. }
        | ConversationInputFact::TurnSettled { attempt, .. }
        | ConversationInputFact::Deferred { attempt, .. }
        | ConversationInputFact::RecoveryRequired { attempt, .. }
            if attempt.attempt == 0
                || attempt.attempt_id.trim().is_empty()
                || attempt.turn_id.trim().is_empty() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "conversation input attempt identity is incomplete".to_string(),
            ))
        }
        ConversationInputFact::TaskRunTargetBound { run_id, .. } if run_id.trim().is_empty() => {
            Err(ChatEventLogError::InvalidEvent(
                "原输入任务去向为空".to_string(),
            ))
        }
        ConversationInputFact::Deferred { reason, .. }
        | ConversationInputFact::RecoveryRequired { reason, .. }
        | ConversationInputFact::RecoveryUncertain { reason, .. }
            if reason.trim().is_empty() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "conversation input lifecycle reason must not be empty".to_string(),
            ))
        }
        ConversationInputFact::Reordered { input_ids, .. }
            if input_ids.is_empty()
                || input_ids.iter().any(|input_id| input_id.trim().is_empty())
                || has_duplicate_ids(input_ids) =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "typed conversation input reorder is invalid".to_string(),
            ))
        }
        ConversationInputFact::Cancelled {
            attempt: Some(attempt),
            ..
        } if attempt.attempt == 0
            || attempt.attempt_id.trim().is_empty()
            || attempt.turn_id.trim().is_empty() =>
        {
            Err(ChatEventLogError::InvalidEvent(
                "cancelled conversation input attempt identity is incomplete".to_string(),
            ))
        }
        _ => Ok(()),
    }
}

fn has_duplicate_ids(input_ids: &[String]) -> bool {
    let mut seen = HashSet::with_capacity(input_ids.len());
    input_ids.iter().any(|input_id| !seen.insert(input_id))
}

fn stable_event_id(stream_id: &str, sequence: u64, content_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CHAT_EVENT_SCHEMA_VERSION.to_be_bytes());
    hasher.update(stream_id.as_bytes());
    hasher.update(sequence.to_be_bytes());
    hasher.update(content_hash.as_bytes());
    format!("chat_evt_{:x}", hasher.finalize())
}

fn digest(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    format!("sha256_{:x}", hasher.finalize())
}

#[derive(Serialize)]
struct EnvelopeIntegrity<'a> {
    schema_version: u16,
    sequence: u64,
    stream_id: &'a str,
    workspace_id: &'a str,
    conversation_id: Option<&'a str>,
    root_turn_id: &'a str,
    turn_id: &'a str,
    message_id: &'a str,
    timestamp: DateTime<Utc>,
    payload: &'a ChatDriverEvent,
}

fn envelope_content_hash(integrity: EnvelopeIntegrity<'_>) -> Result<String, ChatEventLogError> {
    let encoded = echo_agent::utils::canonical_json::canonical_json_bytes(&integrity)
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
    Ok(digest(&encoded))
}

fn validate_unclaimed_channel_input(
    authority: &StreamAuthority,
    current: &ConversationInputProjection,
) -> Result<(), ConversationInputError> {
    let receipt = &current.receipt;
    let entry = authority
        .pins
        .conversation_inputs
        .get(&receipt.identity.input_id)
        .ok_or_else(|| ConversationInputError::StaleRevision {
            input_id: receipt.identity.input_id.clone(),
        })?;
    let retention = authority.journal.retention_metadata();
    if authority.barrier_pending
        || retention.cleanup_pending
        || retention.retained_floor > entry.first_sequence
        || entry.ever_claimed
        || receipt.phase != ConversationInputPhase::Persisted
        || receipt.attempt.is_some()
        || receipt.attempt_id.is_some()
        || receipt.turn_id.is_some()
        || receipt.drained
        || current.active_attempt.is_some()
        || current.payload.channel_binding.is_none()
        || receipt.identity.payload_sha256 != current.payload.payload_sha256
    {
        return Err(ConversationInputError::Validation(
            "channel input has no complete, confirmed never-claimed history".to_string(),
        ));
    }
    if let Some(crate::conversation_input::ChannelInputBinding {
        execution: crate::conversation_input::ChannelExecutionIntent::Resume { expected, .. },
        ..
    }) = current.payload.channel_binding.as_ref()
    {
        // A thin address conversion must be lossless for BOTH original resume
        // drivers. Actual TaskRun revision/policy validation stays with them.
        if expected.workspace_id != receipt.identity.address.workspace_id
            || expected.conversation_id != receipt.identity.address.conversation_id
        {
            return Err(ConversationInputError::Validation(
                "saved Resume intent does not match its original input address".to_string(),
            ));
        }
    }
    current
        .payload
        .validate_channel_digest(&receipt.identity.address)
}

// test-utils私有观测，产品默认构建不编译；跨blocking线程由原tracing dispatcher收集。
// 记录调用尝试及Result结算，不把它解释为OS syscall、物理I/O字节或优化收益。
#[cfg(feature = "test-utils")]
fn native_history_perf_io_event(
    authority: &StreamAuthority,
    operation: &'static str,
    stage: &'static str,
) {
    tracing::trace!(target: "eko::native_history::perf_io", path = %authority.journal.directory().display(), operation, stage);
    #[cfg(test)]
    native_history_perf_io::record(authority.journal.directory(), operation, stage);
}

#[cfg(all(test, feature = "test-utils"))]
mod native_history_perf_io {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Clone, Copy, Debug, Default, serde::Serialize)]
    pub(super) struct Count {
        pub(super) attempted: u64,
        pub(super) succeeded: u64,
        pub(super) failed: u64,
    }
    pub(super) type Counts = BTreeMap<&'static str, Count>;
    static ACTIVE: Mutex<Option<(PathBuf, Counts)>> = Mutex::new(None);

    pub(super) fn record(path: &Path, operation: &'static str, stage: &'static str) {
        let mut active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
        let Some((root, counts)) = active.as_mut() else {
            return;
        };
        if !path.starts_with(root) {
            return;
        }
        let count = counts.entry(operation).or_default();
        match stage {
            "attempt" => count.attempted = count.attempted.saturating_add(1),
            "success" => count.succeeded = count.succeeded.saturating_add(1),
            "failure" => count.failed = count.failed.saturating_add(1),
            _ => {}
        }
    }

    pub(super) struct Observation {
        root: PathBuf,
        active: bool,
    }
    impl Observation {
        pub(super) fn begin(root: PathBuf) -> Result<Self, String> {
            let mut active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
            if active.is_some() {
                return Err("native I/O observation already owned".into());
            }
            *active = Some((root.clone(), Counts::new()));
            Ok(Self { root, active: true })
        }
        pub(super) fn finish(mut self) -> Result<Counts, String> {
            let mut active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
            if active.as_ref().is_none_or(|(root, _)| *root != self.root) {
                return Err("native I/O observation owner changed".into());
            }
            let (_, counts) = active.take().ok_or("I/O observation disappeared")?;
            self.active = false;
            Ok(counts)
        }
    }
    impl Drop for Observation {
        fn drop(&mut self) {
            if self.active {
                let mut active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
                if active.as_ref().is_some_and(|(root, _)| *root == self.root) {
                    *active = None;
                }
            }
        }
    }
}
