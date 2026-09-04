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
            self.command_cell_watch_facts.entry(fact_key).or_insert(sequence);
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
            .chain(self.started_command_cell_watches.values().map(|(sequence, _)| sequence))
            .chain(self.active_cells.values())
            .chain(conversation_input_earliest.iter())
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
                            payload: payload.clone(),
                            active_attempt: None,
                        },
                        first_sequence: sequence,
                        last_sequence: sequence,
                        terminal_fact_self_contained: false,
                    },
                );
                self.queue_order
                    .retain(|queued| queued != &identity.input_id);
                self.queue_order.push(identity.input_id.clone());
            }
            ConversationInputFact::AttemptStarted { attempt, .. } => {
                if let Some(entry) = self.conversation_inputs.get_mut(&attempt.identity.input_id) {
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
            ConversationInputFact::Drained { attempt, .. } => {
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
                        text: String::new(),
                        attachments: Vec::new(),
                        submitted_at_ms: 0,
                        payload_sha256: identity.payload_sha256.clone(),
                    },
                    active_attempt: None,
                },
                first_sequence: sequence,
                last_sequence: sequence,
                terminal_fact_self_contained,
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
    if conversation_input_is_frontier(&entry.projection.receipt) {
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
        ConversationInputFact::MailboxAccepted { attempt, .. } => {
            exact_attempt(attempt)
                && matches!(
                    receipt.phase,
                    ConversationInputPhase::MailboxAccepted
                        | ConversationInputPhase::Drained
                        | ConversationInputPhase::TurnSettled
                )
        }
        ConversationInputFact::Drained { attempt, .. } => exact_attempt(attempt) && receipt.drained,
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
        | ChatDriverEvent::InputLifecycle(_)
        | ChatDriverEvent::ApprovalRequest { .. }
        | ChatDriverEvent::InputRequest { .. }
        | ChatDriverEvent::SelectionRequest { .. }
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
        ChatDriverEvent::CommandCellWatchReady { result } => {
            Some(format!("ready:{}", command_cell_watch_receipt_key(&result.receipt)))
        }
        ChatDriverEvent::CommandCellWatchDeliveryStarted { acknowledgement } => {
            Some(format!("started:{}", command_cell_watch_ack_key(acknowledgement)))
        }
        ChatDriverEvent::CommandCellWatchAcknowledged { acknowledgement } => {
            Some(format!("ack:{}", command_cell_watch_ack_key(acknowledgement)))
        }
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
                && (metadata.message_id.as_deref().is_some_and(|id| id != root_turn_id)
                    || execution.subagent_run_id.as_deref()
                        != Some(metadata.execution_id.as_str())
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
        ConversationInputFact::Deferred { reason, .. }
        | ConversationInputFact::RecoveryRequired { reason, .. }
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
