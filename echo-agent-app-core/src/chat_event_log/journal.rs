impl ChatEventLog {
    pub async fn append_async(
        self: &Arc<Self>,
        workspace_id: String,
        conversation_id: Option<String>,
        root_turn_id: String,
        event: ChatDriverEvent,
    ) -> Result<ChatEventEnvelope, ChatEventLogError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.append(
                &workspace_id,
                conversation_id.as_deref(),
                &root_turn_id,
                event,
            )
        })
        .await
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?
    }

    pub async fn settle_all_started_command_cell_watch_deliveries_async(
        self: &Arc<Self>,
    ) -> Result<usize, ChatEventLogError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let recoveries = log.all_started_command_cell_watch_deliveries()?;
            let mut settled = 0_usize;
            for (workspace_id, conversation_id, root_turn_id, acknowledgement) in recoveries {
                log.append(
                    &workspace_id,
                    conversation_id.as_deref(),
                    &root_turn_id,
                    ChatDriverEvent::CommandCellWatchAcknowledged { acknowledgement },
                )?;
                settled = settled.saturating_add(1);
            }
            Ok(settled)
        })
        .await
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?
    }

    pub async fn pending_command_cell_watches_for_conversation_async(
        self: &Arc<Self>,
        workspace_id: String,
        conversation_id: String,
    ) -> Result<Vec<crate::tasks::task_runtime::command_cells::CommandCellWatchResult>, ChatEventLogError>
    {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.pending_command_cell_watches_for_conversation(&workspace_id, &conversation_id)
        })
        .await
        .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?
    }

    pub fn default_root() -> PathBuf {
        crate::data_root::user_data_path("chat-events")
    }

    pub fn at_default_root() -> Self {
        Self {
            root: Self::default_root(),
            retention: ChatEventRetention::default(),
            streams: DashMap::new(),
            stream_access: Mutex::new(VecDeque::new()),
            live_sinks: Mutex::new(HashMap::new()),
            #[cfg(test)]
            deletion_pause: None,
            #[cfg(test)]
            orphan_recovery_pause: None,
        }
    }

    pub fn open(
        root: impl Into<PathBuf>,
        retention: ChatEventRetention,
    ) -> Result<Self, ChatEventLogError> {
        if retention.segment_rollover_bytes == 0
            || retention.max_segments == 0
            || retention.max_replay_events == 0
        {
            return Err(ChatEventLogError::InvalidIdentity(
                "retention limits must be positive".to_string(),
            ));
        }
        let root = root.into();
        ensure_real_directory(&root, true)?;
        Ok(Self {
            root,
            retention,
            streams: DashMap::new(),
            stream_access: Mutex::new(VecDeque::new()),
            live_sinks: Mutex::new(HashMap::new()),
            #[cfg(test)]
            deletion_pause: None,
            #[cfg(test)]
            orphan_recovery_pause: None,
        })
    }

    pub fn append(
        &self,
        workspace_id: &str,
        conversation_id: Option<&str>,
        root_turn_id: &str,
        event: ChatDriverEvent,
    ) -> Result<ChatEventEnvelope, ChatEventLogError> {
        if matches!(&event, ChatDriverEvent::InputLifecycle(_)) {
            return Err(ChatEventLogError::InvalidEvent(
                "conversation input writes must use ConversationInputService".to_string(),
            ));
        }
        self.append_internal(workspace_id, conversation_id, root_turn_id, event)
    }

    fn append_internal(
        &self,
        workspace_id: &str,
        conversation_id: Option<&str>,
        root_turn_id: &str,
        event: ChatDriverEvent,
    ) -> Result<ChatEventEnvelope, ChatEventLogError> {
        validate_event_stream_identity(workspace_id, conversation_id, root_turn_id, &event)?;
        validate_driver_event(&event)?;
        if let ChatDriverEvent::InputLifecycle(fact) = &event
            && fact.identity().input_id != root_turn_id
        {
            return Err(ChatEventLogError::InvalidIdentity(
                "conversation input lifecycle identity does not match the journal root".to_string(),
            ));
        }
        let selected_stream_id = stream_id(workspace_id, conversation_id, root_turn_id)?;
        let (turn_id, message_id) = event_identity(&event, root_turn_id);
        if message_id != root_turn_id {
            return Err(ChatEventLogError::InvalidIdentity(
                "event root message does not match the selected journal turn".to_string(),
            ));
        }
        let path = self.stream_dir(&selected_stream_id);
        let cached = self
            .stream_journal(&selected_stream_id, true)?
            .ok_or_else(|| corrupt(&path, "chat event stream authority was not created"))?;
        let mut guard = lock_cached_stream(&cached);
        let authority = guard
            .as_mut()
            .ok_or_else(|| corrupt(&path, "chat event stream authority was removed"))?;
        if self.retry_pending_barrier(authority, &selected_stream_id) {
            self.maintain_retention(authority, &selected_stream_id);
        }

        let envelope = self.append_locked(
            authority,
            &selected_stream_id,
            &path,
            workspace_id,
            conversation_id,
            root_turn_id,
            turn_id,
            message_id,
            event,
        )?;
        drop(guard);
        drop(cached);
        self.evict_inactive_streams(None);
        Ok(envelope)
    }

    #[allow(clippy::too_many_arguments)]
    fn append_locked(
        &self,
        authority: &mut StreamAuthority,
        selected_stream_id: &str,
        path: &Path,
        workspace_id: &str,
        conversation_id: Option<&str>,
        root_turn_id: &str,
        turn_id: String,
        message_id: String,
        event: ChatDriverEvent,
    ) -> Result<ChatEventEnvelope, ChatEventLogError> {
        if let Some(fact_key) = command_cell_watch_fact_key(&event)
            && let Some(sequence) = authority.pins.command_cell_watch_facts.get(&fact_key).copied()
        {
            let record = authority
                .journal
                .replay_after(sequence.saturating_sub(1), 1)
                .map_err(|error| journal_error(path, error))?
                .into_iter()
                .next()
                .filter(|record| record.sequence == sequence)
                .ok_or_else(|| {
                    corrupt(
                        path,
                        format!("cached durable fact {fact_key} is missing at {sequence}"),
                    )
                })?;
            let expected = echo_agent::utils::canonical_json::canonical_json_bytes(&event)
                .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
            let actual =
                echo_agent::utils::canonical_json::canonical_json_bytes(&record.event.payload)
                    .map_err(|error| ChatEventLogError::Serialization(error.to_string()))?;
            return if expected == actual {
                envelope_from_record(record, path, selected_stream_id)
            } else {
                Err(ChatEventLogError::InvalidEvent(format!(
                    "conflicting durable fact for {fact_key}"
                )))
            };
        }

        let persisted = PersistedChatEvent {
            schema_version: CHAT_EVENT_SCHEMA_VERSION,
            stream_id: selected_stream_id.to_string(),
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.map(ToString::to_string),
            root_turn_id: root_turn_id.to_string(),
            turn_id,
            message_id,
            timestamp: Utc::now(),
            payload: event,
        };
        let durability = append_durability(&persisted.payload);
        let receipt = authority
            .journal
            .append_with_durability(persisted, durability)
            .map_err(|error| journal_error(path, error))?;
        if let JournalDurabilityStatus::Degraded { error } = &receipt.durability {
            tracing::warn!(stream_id = %selected_stream_id, sequence = receipt.record.sequence, %error, "chat event committed with degraded durability; append will not be retried");
        }
        authority
            .pins
            .apply(receipt.record.sequence, receipt.record.event.as_ref());
        let mut maintain_retention = should_maintain_retention(durability, &receipt.durability);
        if should_mark_barrier_pending(durability, &receipt.durability) {
            authority.barrier_pending = true;
            maintain_retention = self.retry_pending_barrier(authority, selected_stream_id);
        }
        let envelope = envelope_from_record(receipt.record, path, selected_stream_id)?;
        if maintain_retention {
            self.maintain_retention(authority, selected_stream_id);
        }
        Ok(envelope)
    }

    pub fn replay(
        &self,
        workspace_id: &str,
        conversation_id: Option<&str>,
        turn_id: &str,
        after_cursor: u64,
    ) -> Result<ChatEventReplay, ChatEventLogError> {
        let selected_stream_id = stream_id(workspace_id, conversation_id, turn_id)?;
        let Some(cached) = self.stream_journal(&selected_stream_id, false)? else {
            return Ok(empty_replay());
        };
        let mut guard = lock_cached_stream(&cached);
        let Some(authority) = guard.as_mut() else {
            return Ok(empty_replay());
        };
        if self.retry_pending_barrier(authority, &selected_stream_id) {
            self.maintain_retention(authority, &selected_stream_id);
        }
        let journal = &authority.journal;
        let latest_cursor = journal.last_sequence();
        let retained_floor = journal.retention_metadata().retained_floor;
        if latest_cursor == 0 || latest_cursor < retained_floor {
            return Ok(empty_replay());
        }
        let floor_cursor = retained_floor.saturating_sub(1);
        let requested_after = after_cursor.max(floor_cursor);
        let replay_limit = u64::try_from(self.retention.max_replay_events).unwrap_or(u64::MAX);
        let cap_after = latest_cursor.saturating_sub(replay_limit);
        let effective_after = requested_after.max(cap_after);
        let path = self.stream_dir(&selected_stream_id);
        let records = journal
            .replay_after(effective_after, self.retention.max_replay_events)
            .map_err(|error| journal_error(&path, error))?;
        let events = records
            .into_iter()
            .map(|record| envelope_from_record(record, &path, &selected_stream_id))
            .collect::<Result<Vec<_>, _>>()?;
        let replay = ChatEventReplay {
            retained_earliest_cursor: Some(retained_floor),
            returned_earliest_cursor: events.first().map(|event| event.sequence),
            latest_cursor,
            truncated: after_cursor < floor_cursor || requested_after < cap_after,
            events,
        };
        drop(guard);
        drop(cached);
        self.evict_inactive_streams(None);
        Ok(replay)
    }

    /// Close ordinary-Chat command cells whose process owner disappeared.
    ///
    /// TaskRun cells are recovered by `TaskRuntimeStore`; this scans only the
    /// product chat journal so a Chat turn without a formal run still receives
    /// one durable Interrupted terminal after an application restart.
    pub fn recover_orphan_command_cells(&self) -> Result<usize, ChatEventLogError> {
        struct Recovery {
            workspace_id: String,
            conversation_id: Option<String>,
            root_turn_id: String,
            cell: crate::tasks::task_runtime::types::BackgroundCellState,
        }

        let mut recoveries = Vec::new();
        for stream in self.enumerate_streams()? {
            let Some(cached) = self.stream_journal(&stream.stream_id, false)? else {
                continue;
            };
            let mut guard = lock_cached_stream(&cached);
            let Some(authority) = guard.as_mut() else {
                continue;
            };
            let active = authority
                .pins
                .active_cells
                .values()
                .copied()
                .collect::<Vec<_>>();
            for sequence in active {
                let record = authority
                    .journal
                    .replay_after(sequence.saturating_sub(1), 1)
                    .map_err(|error| journal_error(&stream.path, error))?
                    .into_iter()
                    .next()
                    .filter(|record| record.sequence == sequence)
                    .ok_or_else(|| {
                        corrupt(
                            &stream.path,
                            format!("active command cell is missing at {sequence}"),
                        )
                    })?;
                validate_persisted_record(&record, &stream.path, Some(&stream.stream_id))?;
                let ChatDriverEvent::CommandCellStarted { cell } = &record.event.payload else {
                    return Err(corrupt(
                        &stream.path,
                        format!("active command cell pin at {sequence} is not a Started fact"),
                    ));
                };
                let mut cell = cell.as_ref().clone();
                cell.phase = crate::tasks::task_runtime::types::BackgroundCellPhase::Failed;
                cell.terminal_cause = Some(
                    crate::tasks::task_runtime::types::BackgroundCellTerminalCause::Interrupted,
                );
                cell.terminal_message =
                    Some("command cell was interrupted by process restart".to_string());
                cell.exit_code = None;
                if cell.artifact_status
                    == crate::tasks::task_runtime::types::BackgroundCellArtifactStatus::Writing
                {
                    cell.artifact_status =
                        crate::tasks::task_runtime::types::BackgroundCellArtifactStatus::Failed;
                    cell.artifact_message = Some(
                        "artifact finalization was interrupted by process restart".to_string(),
                    );
                }
                cell.finished_at = Some(Utc::now());
                recoveries.push(Recovery {
                    workspace_id: stream.first.workspace_id.clone(),
                    conversation_id: stream.first.conversation_id.clone(),
                    root_turn_id: stream.first.root_turn_id.clone(),
                    cell,
                });
            }
        }

        let mut recovered = 0_usize;
        #[cfg(test)]
        if let Some(pause) = &self.orphan_recovery_pause {
            pause.0.wait();
            pause.1.wait();
        }
        for recovery in recoveries {
            let cell_id = recovery.cell.cell_id.clone();
            let appended = self.append(
                &recovery.workspace_id,
                recovery.conversation_id.as_deref(),
                &recovery.root_turn_id,
                ChatDriverEvent::CommandCellSettled {
                    cell: Box::new(recovery.cell),
                },
            );
            match appended {
                Ok(_) => recovered = recovered.saturating_add(1),
                Err(ChatEventLogError::InvalidEvent(_)) => {
                    let replay = self.replay(
                        &recovery.workspace_id,
                        recovery.conversation_id.as_deref(),
                        &recovery.root_turn_id,
                        0,
                    )?;
                    let terminal_exists = replay.events.iter().any(|event| {
                        matches!(
                            &event.payload,
                            ChatDriverEvent::CommandCellSettled { cell }
                                if cell.cell_id == cell_id && !cell.is_active()
                        )
                    });
                    if !terminal_exists {
                        return Err(ChatEventLogError::InvalidEvent(format!(
                            "orphan command cell {cell_id} conflicted without a terminal fact"
                        )));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(recovered)
    }

    pub fn reconcile_conversation_inputs_at_boot(&self) -> Result<usize, ChatEventLogError> {
        let mut recovered = 0_usize;
        for stream in self.enumerate_streams_isolated()? {
            let Some(conversation_id) = stream.first.conversation_id.clone() else {
                continue;
            };
            let address = ConversationInputAddress {
                workspace_id: stream.first.workspace_id.clone(),
                conversation_id,
            };
            let cached = match self.stream_journal(&stream.stream_id, false) {
                Ok(Some(cached)) => cached,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(stream_id = %stream.stream_id, %error, "conversation input boot recovery skipped an unavailable stream");
                    continue;
                }
            };
            let mut guard = lock_cached_stream(&cached);
            let Some(authority) = guard.as_mut() else {
                continue;
            };
            let candidates = authority
                .pins
                .conversation_inputs
                .values()
                .filter(|entry| {
                    matches!(
                        entry.projection.receipt.phase,
                        ConversationInputPhase::AttemptStarted
                            | ConversationInputPhase::MailboxAccepted
                            | ConversationInputPhase::Drained
                            | ConversationInputPhase::RecoveryRequired
                    )
                })
                .map(|entry| entry.projection.clone())
                .collect::<Vec<_>>();
            for projection in candidates {
                let Some(attempt) = conversation_input_attempt_from_receipt(&projection.receipt)
                else {
                    continue;
                };
                let fact = if projection.receipt.drained {
                    ConversationInputFact::TurnSettled {
                        attempt,
                        outcome: ConversationInputOutcome::Dropped,
                        drained: true,
                        settled_at_ms: echo_agent::utils::time::now_millis(),
                    }
                } else {
                    let reason =
                        "foreground input owner was lost during application restart".to_string();
                    ConversationInputFact::Cancelled {
                        identity: attempt.identity.clone(),
                        attempt: Some(attempt),
                        drained: projection.receipt.drained,
                        reason: Some(reason),
                        cancelled_at_ms: echo_agent::utils::time::now_millis(),
                    }
                };
                let input_id = fact.identity().input_id.clone();
                if let Err(error) = self.append_conversation_input_event_locked(
                    authority,
                    &stream.stream_id,
                    &stream.path,
                    &address,
                    &input_id,
                    ChatDriverEvent::InputLifecycle(Box::new(fact)),
                ) {
                    tracing::warn!(stream_id = %stream.stream_id, input_id = %input_id, %error, "conversation input boot recovery isolated a failed terminal append");
                    continue;
                }
                recovered = recovered.saturating_add(1);
            }
        }
        Ok(recovered)
    }

    pub fn pending_command_cell_watches(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        root_turn_id: &str,
    ) -> Result<Vec<crate::tasks::task_runtime::command_cells::CommandCellWatchResult>, ChatEventLogError>
    {
        let selected_stream_id = stream_id(workspace_id, Some(conversation_id), root_turn_id)?;
        let Some(cached) = self.stream_journal(&selected_stream_id, false)? else {
            return Ok(Vec::new());
        };
        let mut guard = lock_cached_stream(&cached);
        let Some(authority) = guard.as_mut() else {
            return Ok(Vec::new());
        };
        if self.retry_pending_barrier(authority, &selected_stream_id) {
            self.maintain_retention(authority, &selected_stream_id);
        }
        let path = self.stream_dir(&selected_stream_id);
        let pending = authority
            .pins
            .pending_command_cell_watches
            .iter()
            .map(|(key, sequence)| (key.clone(), *sequence))
            .collect::<BTreeMap<_, _>>();
        let mut results = Vec::with_capacity(pending.len());
        for (key, sequence) in pending {
            let record = authority
                .journal
                .replay_after(sequence.saturating_sub(1), 1)
                .map_err(|error| journal_error(&path, error))?
                .into_iter()
                .next()
                .filter(|record| record.sequence == sequence)
                .ok_or_else(|| {
                    corrupt(
                        &path,
                        format!("pending CommandCellWatch {key} is missing at {sequence}"),
                    )
                })?;
            let envelope = envelope_from_record(record, &path, &selected_stream_id)?;
            let ChatDriverEvent::CommandCellWatchReady { result } = envelope.payload else {
                return Err(corrupt(
                    &path,
                    format!("pending CommandCellWatch {key} does not point to a Ready fact"),
                ));
            };
            if command_cell_watch_receipt_key(&result.receipt) != key {
                return Err(corrupt(
                    &path,
                    format!("pending CommandCellWatch {key} points to a different receipt"),
                ));
            }
            results.push(*result);
        }
        drop(guard);
        drop(cached);
        self.evict_inactive_streams(None);
        Ok(results)
    }

    pub fn pending_command_cell_watches_for_conversation(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<Vec<crate::tasks::task_runtime::command_cells::CommandCellWatchResult>, ChatEventLogError>
    {
        let mut pending = Vec::new();
        // Enumeration is only a workspace/conversation filter; a corrupt
        // unrelated stream must not block this conversation's pending
        // watches. Strictness for the target stream itself is enforced by
        // pending_command_cell_watches when it replays that stream.
        for stream in self.enumerate_streams_isolated()? {
            if stream.first.workspace_id == workspace_id
                && stream.first.conversation_id.as_deref() == Some(conversation_id)
            {
                pending.extend(self.pending_command_cell_watches(
                    workspace_id,
                    conversation_id,
                    &stream.first.root_turn_id,
                )?);
            }
        }
        pending.sort_by(|left, right| {
            left.receipt
                .started_at
                .cmp(&right.receipt.started_at)
                .then_with(|| left.receipt.execution_id.cmp(&right.receipt.execution_id))
        });
        Ok(pending)
    }

    fn all_started_command_cell_watch_deliveries(
        &self,
    ) -> Result<Vec<StartedCommandCellWatchDelivery>, ChatEventLogError> {
        let mut started = Vec::new();
        // Boot recovery must stay per-stream isolated: one corrupt chat
        // directory may not strand every other conversation's started
        // CommandCellWatch in `unreconciled`.
        for stream in self.enumerate_streams_isolated()? {
            let Some(cached) = self.stream_journal(&stream.stream_id, false)? else {
                continue;
            };
            let guard = lock_cached_stream(&cached);
            let Some(authority) = guard.as_ref() else {
                continue;
            };
            started.extend(
                authority
                    .pins
                    .started_command_cell_watches
                    .values()
                    .map(|(_, acknowledgement)| {
                        (
                            stream.first.workspace_id.clone(),
                            stream.first.conversation_id.clone(),
                            stream.first.root_turn_id.clone(),
                            acknowledgement.clone(),
                        )
                    }),
            );
        }
        Ok(started)
    }

    fn with_conversation_input_authority<T>(
        &self,
        address: &ConversationInputAddress,
        create: bool,
        operation: impl FnOnce(&mut StreamAuthority, &str, &Path) -> Result<T, ConversationInputError>,
    ) -> Result<T, ConversationInputError> {
        let selected_stream_id = stream_id(
            &address.workspace_id,
            Some(&address.conversation_id),
            &address.conversation_id,
        )?;
        let path = self.stream_dir(&selected_stream_id);
        let Some(cached) = self.stream_journal(&selected_stream_id, create)? else {
            return Err(ConversationInputError::Validation(
                "conversation input authority is unavailable".to_string(),
            ));
        };
        let mut guard = lock_cached_stream(&cached);
        let authority = guard
            .as_mut()
            .ok_or_else(|| corrupt(&path, "conversation input authority was removed"))?;
        if self.retry_pending_barrier(authority, &selected_stream_id) {
            self.maintain_retention(authority, &selected_stream_id);
        }
        let result = operation(authority, &selected_stream_id, &path);
        drop(guard);
        drop(cached);
        self.evict_inactive_streams(None);
        result
    }

    fn append_conversation_input_event_locked(
        &self,
        authority: &mut StreamAuthority,
        selected_stream_id: &str,
        path: &Path,
        address: &ConversationInputAddress,
        root_turn_id: &str,
        event: ChatDriverEvent,
    ) -> Result<(), ConversationInputError> {
        validate_event_stream_identity(
            &address.workspace_id,
            Some(&address.conversation_id),
            root_turn_id,
            &event,
        )?;
        validate_driver_event(&event)?;
        let message_id = root_turn_id.to_string();
        self.append_locked(
            authority,
            selected_stream_id,
            path,
            &address.workspace_id,
            Some(&address.conversation_id),
            root_turn_id,
            message_id,
            root_turn_id.to_string(),
            event,
        )?;
        Ok(())
    }

    /// Idempotently persist one revisioned conversation input through the
    /// existing conversation stream and reducer.
    pub async fn submit_conversation_input(
        self: &Arc<Self>,
        address: ConversationInputAddress,
        input_id: String,
        payload: ConversationInputPayload,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.with_conversation_input_authority(&address, true, |authority, stream_id, path| {
                if let Some(existing) =
                    authority
                        .pins
                        .conversation_inputs
                        .get(&input_id)
                        .map(|entry| {
                            normalized_conversation_input_projection(
                                entry,
                                authority.pins.queue_revision,
                            )
                        })
                {
                    if existing.payload.payload_sha256 == payload.payload_sha256 {
                        let mut receipt = existing.receipt;
                        receipt.duplicate = true;
                        return Ok(receipt);
                    }
                    return Err(ConversationInputError::IdCollision {
                        input_id: input_id.clone(),
                    });
                }
                let identity = ConversationInputIdentity {
                    address: address.clone(),
                    input_id: input_id.clone(),
                    revision: 1,
                    payload_sha256: payload.payload_sha256.clone(),
                };
                log.append_conversation_input_event_locked(
                    authority,
                    stream_id,
                    path,
                    &address,
                    &input_id,
                    ChatDriverEvent::InputLifecycle(Box::new(ConversationInputFact::Persisted {
                        identity,
                        payload,
                    })),
                )?;
                authority
                    .pins
                    .conversation_inputs
                    .get(&input_id)
                    .map(|entry| {
                        normalized_conversation_input_receipt(entry, authority.pins.queue_revision)
                    })
                    .ok_or_else(|| {
                        ConversationInputError::Validation(
                            "persisted conversation input was not folded".to_string(),
                        )
                    })
            })
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    pub async fn conversation_input_frontier(
        self: &Arc<Self>,
        address: &ConversationInputAddress,
    ) -> Result<ConversationInputFrontier, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        let address = address.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.conversation_input_frontier_sync(&address)
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    pub async fn start_next_conversation_input(
        self: &Arc<Self>,
        address: &ConversationInputAddress,
        turn_id: String,
    ) -> Result<Option<ConversationInputProjection>, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        let address = address.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let selected_stream_id = stream_id(
                &address.workspace_id,
                Some(&address.conversation_id),
                &address.conversation_id,
            )?;
            if log.stream_journal(&selected_stream_id, false)?.is_none() {
                return Ok(None);
            }
            log.with_conversation_input_authority(&address, false, |authority, stream_id, path| {
                if authority.pins.conversation_inputs.values().any(|entry| {
                    conversation_input_is_frontier(&entry.projection.receipt)
                        && entry.projection.receipt.blocks_replay()
                }) {
                    return Ok(None);
                }
                let next = authority
                    .pins
                    .queue_order
                    .iter()
                    .filter_map(|input_id| authority.pins.conversation_inputs.get(input_id))
                    .next()
                    .map(|entry| {
                        normalized_conversation_input_projection(
                            entry,
                            authority.pins.queue_revision,
                        )
                    });
                let Some(next) = next else {
                    return Ok(None);
                };
                if !next.receipt.is_dispatchable() {
                    return Ok(None);
                }
                let attempt = next
                    .receipt
                    .attempt
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| {
                        ConversationInputError::Validation(
                            "conversation input attempt exhausted".to_string(),
                        )
                    })?;
                let attempt_identity = ConversationInputAttempt {
                    identity: next.receipt.identity.clone(),
                    attempt,
                    attempt_id: uuid::Uuid::new_v4().to_string(),
                    turn_id,
                    observation: Default::default(),
                };
                let input_id = attempt_identity.identity.input_id.clone();
                log.append_conversation_input_event_locked(
                    authority,
                    stream_id,
                    path,
                    &address,
                    &input_id,
                    ChatDriverEvent::InputLifecycle(Box::new(
                        ConversationInputFact::AttemptStarted {
                            attempt: attempt_identity,
                            started_at_ms: echo_agent::utils::time::now_millis(),
                        },
                    )),
                )?;
                Ok(authority
                    .pins
                    .conversation_inputs
                    .get(&input_id)
                    .map(|entry| {
                        normalized_conversation_input_projection(
                            entry,
                            authority.pins.queue_revision,
                        )
                    }))
            })
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    pub async fn start_selected_conversation_input(
        self: &Arc<Self>,
        identity: ConversationInputIdentity,
        expected_queue_revision: u64,
        turn_id: String,
    ) -> Result<ConversationInputProjection, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.with_conversation_input_authority(
                &identity.address,
                false,
                |authority, stream_id, path| {
                    if authority.pins.queue_revision != expected_queue_revision {
                        return Err(ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        });
                    }
                    if authority.pins.conversation_inputs.values().any(|entry| {
                        conversation_input_is_frontier(&entry.projection.receipt)
                            && entry.projection.receipt.blocks_replay()
                    }) {
                        return Err(ConversationInputError::NotDispatchable {
                            input_id: identity.input_id.clone(),
                        });
                    }
                    let current = authority
                        .pins
                        .conversation_inputs
                        .get(&identity.input_id)
                        .map(|entry| {
                            normalized_conversation_input_projection(
                                entry,
                                authority.pins.queue_revision,
                            )
                        })
                        .ok_or_else(|| ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        })?;
                    if current.receipt.identity != identity
                        || !authority.pins.queue_order.contains(&identity.input_id)
                    {
                        return Err(ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        });
                    }
                    if !conversation_input_is_frontier(&current.receipt)
                        || !current.receipt.is_dispatchable()
                    {
                        return Err(ConversationInputError::NotDispatchable {
                            input_id: identity.input_id.clone(),
                        });
                    }
                    let attempt = current
                        .receipt
                        .attempt
                        .unwrap_or(0)
                        .checked_add(1)
                        .ok_or_else(|| {
                            ConversationInputError::Validation(
                                "conversation input attempt exhausted".to_string(),
                            )
                        })?;
                    let attempt_identity = ConversationInputAttempt {
                        identity: identity.clone(),
                        attempt,
                        attempt_id: uuid::Uuid::new_v4().to_string(),
                        turn_id,
                        observation: Default::default(),
                    };
                    log.append_conversation_input_event_locked(
                        authority,
                        stream_id,
                        path,
                        &identity.address,
                        &identity.input_id,
                        ChatDriverEvent::InputLifecycle(Box::new(
                            ConversationInputFact::AttemptStarted {
                                attempt: attempt_identity,
                                started_at_ms: echo_agent::utils::time::now_millis(),
                            },
                        )),
                    )?;
                    authority
                        .pins
                        .conversation_inputs
                        .get(&identity.input_id)
                        .map(|entry| {
                            normalized_conversation_input_projection(
                                entry,
                                authority.pins.queue_revision,
                            )
                        })
                        .ok_or_else(|| ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        })
                },
            )
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    pub async fn settle_conversation_input_turn(
        self: &Arc<Self>,
        address: &ConversationInputAddress,
        turn_id: &str,
        outcome: ConversationInputOutcome,
    ) -> Result<Vec<ConversationInputReceipt>, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        let address = address.clone();
        let turn_id = turn_id.to_string();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let selected_stream_id = stream_id(
                &address.workspace_id,
                Some(&address.conversation_id),
                &address.conversation_id,
            )?;
            if log.stream_journal(&selected_stream_id, false)?.is_none() {
                return Ok(Vec::new());
            }
            log.with_conversation_input_authority(&address, false, |authority, stream_id, path| {
                let mut candidates = authority
                    .pins
                    .conversation_inputs
                    .values()
                    .filter(|entry| {
                        entry.projection.receipt.turn_id.as_deref() == Some(turn_id.as_str())
                            && matches!(
                                entry.projection.receipt.phase,
                                ConversationInputPhase::AttemptStarted
                                    | ConversationInputPhase::MailboxAccepted
                                    | ConversationInputPhase::Drained
                                    | ConversationInputPhase::RecoveryRequired
                            )
                    })
                    .map(|entry| entry.projection.clone())
                    .collect::<Vec<_>>();
                candidates.sort_by(|left, right| {
                    left.receipt
                        .identity
                        .input_id
                        .cmp(&right.receipt.identity.input_id)
                });
                let mut settled_ids = Vec::with_capacity(candidates.len());
                for current in candidates {
                    let attempt = conversation_input_attempt_from_receipt(&current.receipt)
                        .ok_or_else(|| ConversationInputError::StaleAttempt {
                            input_id: current.receipt.identity.input_id.clone(),
                        })?;
                    let terminal_drained = current.receipt.drained;
                    let fact = if current.receipt.phase == ConversationInputPhase::RecoveryRequired
                        || attempt.observation.failed()
                    {
                        ConversationInputFact::Cancelled {
                            identity: current.receipt.identity.clone(),
                            attempt: Some(attempt),
                            drained: terminal_drained,
                            reason: current.receipt.reason.clone(),
                            cancelled_at_ms: echo_agent::utils::time::now_millis(),
                        }
                    } else {
                        ConversationInputFact::TurnSettled {
                            attempt,
                            outcome,
                            drained: terminal_drained,
                            settled_at_ms: echo_agent::utils::time::now_millis(),
                        }
                    };
                    let mut validation_current = current.clone();
                    validation_current.receipt.drained = terminal_drained;
                    log.validate_conversation_input_fact(&validation_current, &fact)?;
                    let input_id = current.receipt.identity.input_id.clone();
                    log.append_conversation_input_event_locked(
                        authority,
                        stream_id,
                        path,
                        &address,
                        &input_id,
                        ChatDriverEvent::InputLifecycle(Box::new(fact)),
                    )?;
                    settled_ids.push(input_id);
                }
                let queue_revision = authority.pins.queue_revision;
                Ok(settled_ids
                    .into_iter()
                    .filter_map(|input_id| {
                        authority
                            .pins
                            .conversation_inputs
                            .get(&input_id)
                            .map(|entry| {
                                normalized_conversation_input_receipt(entry, queue_revision)
                            })
                    })
                    .collect())
            })
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    pub async fn settle_conversation_input_attempt(
        self: &Arc<Self>,
        attempt: &ConversationInputAttempt,
        outcome: ConversationInputOutcome,
        drained: bool,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        let attempt = attempt.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.with_conversation_input_authority(
                &attempt.identity.address,
                false,
                |authority, stream_id, path| {
                    let current = authority
                        .pins
                        .conversation_inputs
                        .get(&attempt.identity.input_id)
                        .map(|entry| {
                            normalized_conversation_input_projection(
                                entry,
                                authority.pins.queue_revision,
                            )
                        })
                        .ok_or_else(|| ConversationInputError::StaleAttempt {
                            input_id: attempt.identity.input_id.clone(),
                        })?;
                    if current.receipt.identity != attempt.identity
                        || current.receipt.attempt != Some(attempt.attempt)
                        || current.receipt.attempt_id.as_deref()
                            != Some(attempt.attempt_id.as_str())
                        || current.receipt.turn_id.as_deref() != Some(attempt.turn_id.as_str())
                    {
                        return Err(ConversationInputError::StaleAttempt {
                            input_id: attempt.identity.input_id.clone(),
                        });
                    }
                    let effective_drained =
                        drained || current.receipt.drained || attempt.observation.drained();
                    let fact = if current.receipt.phase == ConversationInputPhase::RecoveryRequired
                        || attempt.observation.failed()
                    {
                        ConversationInputFact::Cancelled {
                            identity: attempt.identity.clone(),
                            attempt: Some(attempt.clone()),
                            drained: effective_drained,
                            reason: current.receipt.reason.clone().or_else(|| {
                                attempt.observation.failed().then(|| {
                                    "input receipt persistence failed before terminal projection"
                                        .to_string()
                                })
                            }),
                            cancelled_at_ms: echo_agent::utils::time::now_millis(),
                        }
                    } else {
                        ConversationInputFact::TurnSettled {
                            attempt: attempt.clone(),
                            outcome,
                            drained: effective_drained,
                            settled_at_ms: echo_agent::utils::time::now_millis(),
                        }
                    };
                    if conversation_input_fact_is_duplicate(&current, &fact) {
                        let mut receipt = current.receipt;
                        receipt.duplicate = true;
                        return Ok(receipt);
                    }
                    log.validate_conversation_input_fact(&current, &fact)?;
                    log.append_conversation_input_event_locked(
                        authority,
                        stream_id,
                        path,
                        &attempt.identity.address,
                        &attempt.identity.input_id,
                        ChatDriverEvent::InputLifecycle(Box::new(fact)),
                    )?;
                    authority
                        .pins
                        .conversation_inputs
                        .get(&attempt.identity.input_id)
                        .map(|entry| {
                            normalized_conversation_input_receipt(
                                entry,
                                authority.pins.queue_revision,
                            )
                        })
                        .ok_or_else(|| ConversationInputError::StaleAttempt {
                            input_id: attempt.identity.input_id.clone(),
                        })
                },
            )
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    pub async fn append_conversation_input_fact(
        self: &Arc<Self>,
        fact: ConversationInputFact,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let identity = fact.identity().clone();
            log.with_conversation_input_authority(
                &identity.address,
                false,
                |authority, stream_id, path| {
                    let current = authority
                        .pins
                        .conversation_inputs
                        .get(&identity.input_id)
                        .map(|entry| {
                            normalized_conversation_input_projection(
                                entry,
                                authority.pins.queue_revision,
                            )
                        })
                        .ok_or_else(|| ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        })?;
                    if conversation_input_fact_is_duplicate(&current, &fact) {
                        let mut receipt = current.receipt;
                        receipt.duplicate = true;
                        return Ok(receipt);
                    }
                    log.validate_conversation_input_fact(&current, &fact)?;
                    log.append_conversation_input_event_locked(
                        authority,
                        stream_id,
                        path,
                        &identity.address,
                        &identity.input_id,
                        ChatDriverEvent::InputLifecycle(Box::new(fact)),
                    )?;
                    authority
                        .pins
                        .conversation_inputs
                        .get(&identity.input_id)
                        .map(|entry| {
                            normalized_conversation_input_receipt(
                                entry,
                                authority.pins.queue_revision,
                            )
                        })
                        .ok_or_else(|| ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        })
                },
            )
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    pub async fn cancel_conversation_input(
        self: &Arc<Self>,
        identity: ConversationInputIdentity,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.with_conversation_input_authority(
                &identity.address,
                false,
                |authority, stream_id, path| {
                    let current = authority
                        .pins
                        .conversation_inputs
                        .get(&identity.input_id)
                        .map(|entry| {
                            normalized_conversation_input_projection(
                                entry,
                                authority.pins.queue_revision,
                            )
                        })
                        .ok_or_else(|| ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        })?;
                    if current.receipt.identity != identity {
                        return Err(ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        });
                    }
                    if current.receipt.phase == ConversationInputPhase::Cancelled {
                        let mut receipt = current.receipt;
                        receipt.duplicate = true;
                        return Ok(receipt);
                    }
                    if !current.receipt.is_dispatchable()
                        && current.receipt.phase != ConversationInputPhase::RecoveryRequired
                    {
                        return Err(ConversationInputError::NotDispatchable {
                            input_id: identity.input_id.clone(),
                        });
                    }
                    let attempt = conversation_input_attempt_from_receipt(&current.receipt);
                    log.append_conversation_input_event_locked(
                        authority,
                        stream_id,
                        path,
                        &identity.address,
                        &identity.input_id,
                        ChatDriverEvent::InputLifecycle(Box::new(
                            ConversationInputFact::Cancelled {
                                identity: identity.clone(),
                                attempt,
                                drained: current.receipt.drained,
                                reason: current.receipt.reason.clone(),
                                cancelled_at_ms: echo_agent::utils::time::now_millis(),
                            },
                        )),
                    )?;
                    authority
                        .pins
                        .conversation_inputs
                        .get(&identity.input_id)
                        .map(|entry| {
                            normalized_conversation_input_receipt(
                                entry,
                                authority.pins.queue_revision,
                            )
                        })
                        .ok_or_else(|| ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        })
                },
            )
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    pub async fn reorder_conversation_inputs(
        self: &Arc<Self>,
        address: &ConversationInputAddress,
        expected_queue_revision: u64,
        input_ids: Vec<String>,
    ) -> Result<u64, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        let address = address.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.with_conversation_input_authority(&address, false, |authority, stream_id, path| {
                let current_ids = conversation_input_order_from_authority(authority);
                if authority.pins.queue_revision != expected_queue_revision {
                    return Err(ConversationInputError::StaleRevision {
                        input_id: "queue-order".to_string(),
                    });
                }
                if has_duplicate_ids(&input_ids)
                    || input_ids.len() != current_ids.len()
                    || input_ids
                        .iter()
                        .any(|input_id| !current_ids.contains(input_id))
                {
                    return Err(ConversationInputError::Validation(
                        "reorder must contain every frontier input exactly once".to_string(),
                    ));
                }
                let anchor_id = input_ids.first().ok_or_else(|| {
                    ConversationInputError::Validation(
                        "reorder requires an input anchor".to_string(),
                    )
                })?;
                let anchor = authority
                    .pins
                    .conversation_inputs
                    .get(anchor_id)
                    .map(|entry| entry.projection.receipt.identity.clone())
                    .ok_or_else(|| ConversationInputError::StaleRevision {
                        input_id: "queue-order".to_string(),
                    })?;
                let root_turn_id = format!("queue-order:{}", uuid::Uuid::new_v4());
                log.append_conversation_input_event_locked(
                    authority,
                    stream_id,
                    path,
                    &address,
                    &root_turn_id,
                    ChatDriverEvent::InputLifecycle(Box::new(ConversationInputFact::Reordered {
                        anchor,
                        input_ids,
                        reordered_at_ms: echo_agent::utils::time::now_millis(),
                    })),
                )?;
                Ok(authority.pins.queue_revision)
            })
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    fn conversation_input_frontier_sync(
        &self,
        address: &ConversationInputAddress,
    ) -> Result<ConversationInputFrontier, ConversationInputError> {
        let selected_stream_id = stream_id(
            &address.workspace_id,
            Some(&address.conversation_id),
            &address.conversation_id,
        )?;
        let Some(cached) = self.stream_journal(&selected_stream_id, false)? else {
            return Ok(ConversationInputFrontier {
                queue_revision: 0,
                items: Vec::new(),
            });
        };
        let guard = lock_cached_stream(&cached);
        let Some(authority) = guard.as_ref() else {
            return Ok(ConversationInputFrontier {
                queue_revision: 0,
                items: Vec::new(),
            });
        };
        let items = authority
            .pins
            .queue_order
            .iter()
            .filter_map(|input_id| authority.pins.conversation_inputs.get(input_id))
            .filter(|entry| conversation_input_is_frontier(&entry.projection.receipt))
            .map(|entry| {
                normalized_conversation_input_projection(entry, authority.pins.queue_revision)
            })
            .collect();
        Ok(ConversationInputFrontier {
            queue_revision: authority.pins.queue_revision,
            items,
        })
    }

    fn validate_conversation_input_fact(
        &self,
        current: &ConversationInputProjection,
        fact: &ConversationInputFact,
    ) -> Result<(), ConversationInputError> {
        let identity = fact.identity();
        if current.receipt.identity != *identity {
            return Err(ConversationInputError::StaleRevision {
                input_id: identity.input_id.clone(),
            });
        }
        if let ConversationInputFact::Cancelled {
            attempt, drained, ..
        } = fact
        {
            if attempt.as_ref()
                != conversation_input_attempt_from_receipt(&current.receipt).as_ref()
            {
                return Err(ConversationInputError::StaleAttempt {
                    input_id: identity.input_id.clone(),
                });
            }
            if *drained != current.receipt.drained
                && current.receipt.phase != ConversationInputPhase::RecoveryRequired
            {
                return Err(ConversationInputError::StaleAttempt {
                    input_id: identity.input_id.clone(),
                });
            }
            if current.receipt.is_dispatchable()
                || matches!(
                    current.receipt.phase,
                    ConversationInputPhase::AttemptStarted
                        | ConversationInputPhase::MailboxAccepted
                        | ConversationInputPhase::Drained
                        | ConversationInputPhase::RecoveryRequired
                )
            {
                return Ok(());
            }
            return Err(ConversationInputError::NotDispatchable {
                input_id: identity.input_id.clone(),
            });
        }
        if let ConversationInputFact::Reordered { input_ids, .. } = fact {
            if has_duplicate_ids(input_ids) || input_ids.is_empty() {
                return Err(ConversationInputError::Validation(
                    "typed reorder must contain unique inputs".to_string(),
                ));
            }
            return Ok(());
        }
        let attempt = match fact {
            ConversationInputFact::AttemptStarted { attempt, .. }
            | ConversationInputFact::MailboxAccepted { attempt, .. }
            | ConversationInputFact::Drained { attempt, .. }
            | ConversationInputFact::TurnSettled { attempt, .. }
            | ConversationInputFact::Deferred { attempt, .. }
            | ConversationInputFact::RecoveryRequired { attempt, .. } => attempt,
            ConversationInputFact::Persisted { .. }
            | ConversationInputFact::Reordered { .. }
            | ConversationInputFact::Cancelled { .. } => {
                return Ok(());
            }
        };
        if current.receipt.attempt != Some(attempt.attempt)
            || current.receipt.attempt_id.as_deref() != Some(attempt.attempt_id.as_str())
            || current.receipt.turn_id.as_deref() != Some(attempt.turn_id.as_str())
        {
            return Err(ConversationInputError::StaleAttempt {
                input_id: identity.input_id.clone(),
            });
        }
        let valid = match fact {
            ConversationInputFact::MailboxAccepted { .. } => {
                current.receipt.phase == ConversationInputPhase::AttemptStarted
            }
            ConversationInputFact::Drained { .. } => {
                current.receipt.phase == ConversationInputPhase::MailboxAccepted
            }
            ConversationInputFact::TurnSettled { drained, .. } => {
                ((*drained
                    && matches!(
                        current.receipt.phase,
                        ConversationInputPhase::MailboxAccepted
                            | ConversationInputPhase::Drained
                            | ConversationInputPhase::RecoveryRequired
                    ))
                    || (!*drained
                        && matches!(
                            current.receipt.phase,
                            ConversationInputPhase::AttemptStarted
                                | ConversationInputPhase::MailboxAccepted
                        )))
                    && *drained == current.receipt.drained
            }
            ConversationInputFact::Deferred { .. } => {
                current.receipt.phase == ConversationInputPhase::AttemptStarted
            }
            ConversationInputFact::RecoveryRequired { drained, .. } => {
                matches!(
                    current.receipt.phase,
                    ConversationInputPhase::AttemptStarted
                        | ConversationInputPhase::MailboxAccepted
                        | ConversationInputPhase::Drained
                ) && (!current.receipt.drained || *drained)
            }
            ConversationInputFact::AttemptStarted { .. } => current.receipt.is_dispatchable(),
            ConversationInputFact::Persisted { .. }
            | ConversationInputFact::Reordered { .. }
            | ConversationInputFact::Cancelled { .. } => true,
        };
        if valid {
            Ok(())
        } else {
            Err(ConversationInputError::StaleAttempt {
                input_id: identity.input_id.clone(),
            })
        }
    }

    pub fn remove_conversation(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<(), ChatEventLogError> {
        if workspace_id.trim().is_empty() || conversation_id.trim().is_empty() {
            return Err(ChatEventLogError::InvalidIdentity(
                "workspace_id and conversation_id must not be empty".to_string(),
            ));
        }
        if !ensure_real_directory(&self.root, false)? {
            return Ok(());
        }
        let selected_stream_id = stream_id(workspace_id, Some(conversation_id), conversation_id)?;
        let path = self.stream_dir(&selected_stream_id);
        if ensure_real_directory(&path, false)? {
            let _validated = self.stream_journal(&selected_stream_id, false)?;
            self.remove_stream(&selected_stream_id, &path)?;
        }
        Ok(())
    }

    pub fn remove_workspace(&self, workspace_id: &str) -> Result<(), ChatEventLogError> {
        if workspace_id.trim().is_empty() {
            return Err(ChatEventLogError::InvalidIdentity(
                "workspace_id must not be empty".to_string(),
            ));
        }
        if !ensure_real_directory(&self.root, false)? {
            return Ok(());
        }
        for stream in self.enumerate_streams()? {
            if stream.first.workspace_id == workspace_id {
                self.remove_stream(&stream.stream_id, &stream.path)?;
            }
        }
        Ok(())
    }

    fn maintain_retention(&self, authority: &mut StreamAuthority, stream_id: &str) {
        let metadata = authority.journal.retention_metadata();
        let segments = authority.journal.segments();
        if segments.len() <= self.retention.max_segments && !metadata.cleanup_pending {
            return;
        }
        let natural_keep = segments
            .get(segments.len().saturating_sub(self.retention.max_segments))
            .map(|segment| segment.start_sequence)
            .unwrap_or(metadata.retained_floor);
        let keep_from = authority
            .pins
            .earliest()
            .map_or(natural_keep, |pin| natural_keep.min(pin));
        match authority.journal.prune_closed_segments_before(keep_from) {
            Ok(receipt) => {
                authority.pins.discard_before(receipt.retained_floor);
                if let JournalPruneCommitStatus::Degraded { error } = receipt.commit {
                    tracing::warn!(%error, %stream_id, retained_floor = receipt.retained_floor, "chat event retention marker committed with a degraded barrier");
                }
                if let JournalPhysicalCleanupStatus::Degraded { error } = receipt.cleanup {
                    tracing::warn!(%error, %stream_id, retained_floor = receipt.retained_floor, "chat event retention cleanup remains pending");
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, %stream_id, "chat event retention remains pending after a committed safe point")
            }
        }
    }

    fn retry_pending_barrier(&self, authority: &mut StreamAuthority, stream_id: &str) -> bool {
        if !authority.barrier_pending {
            return false;
        }
        match authority.journal.sync_data() {
            Ok(()) => {
                authority.barrier_pending = false;
                true
            }
            Err(error) => {
                tracing::warn!(error = %error, %stream_id, "chat event durability barrier remains pending; committed event will not be retried");
                false
            }
        }
    }

    fn touch_stream(&self, stream_id: &str) {
        let mut access = self
            .stream_access
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        access.retain(|cached| cached != stream_id);
        access.push_back(stream_id.to_string());
    }

    fn evict_inactive_streams(&self, protected_stream: Option<&str>) {
        let mut access = self
            .stream_access
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut attempts = access.len();
        while access.len() > MAX_CACHED_STREAMS && attempts > 0 {
            attempts = attempts.saturating_sub(1);
            let Some(candidate) = access.pop_front() else {
                break;
            };
            if protected_stream == Some(candidate.as_str()) {
                access.push_back(candidate);
                continue;
            }
            let Some(cached) = self.streams.get(&candidate) else {
                continue;
            };
            let can_evict = cached.value().try_lock().is_ok_and(|authority| {
                authority
                    .as_ref()
                    .is_none_or(|authority| !authority.barrier_pending)
            });
            drop(cached);
            if can_evict {
                self.streams.remove(&candidate);
            } else {
                access.push_back(candidate);
            }
        }
    }

    fn forget_stream(&self, stream_id: &str) {
        self.stream_access
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|cached| cached != stream_id);
    }

    #[cfg(test)]
    fn with_deletion_pause(mut self, pause: Arc<(std::sync::Barrier, std::sync::Barrier)>) -> Self {
        self.deletion_pause = Some(pause);
        self
    }

    #[cfg(test)]
    fn with_orphan_recovery_pause(
        mut self,
        pause: Arc<(std::sync::Barrier, std::sync::Barrier)>,
    ) -> Self {
        self.orphan_recovery_pause = Some(pause);
        self
    }

    fn stream_journal(
        &self,
        stream_id: &str,
        create: bool,
    ) -> Result<Option<CachedStreamJournal>, ChatEventLogError> {
        if !ensure_real_directory(&self.root, create)? {
            return Ok(None);
        }
        let stream_dir = self.stream_dir(stream_id);
        if !ensure_real_directory(&stream_dir, create)? {
            return Ok(None);
        }
        if let Some(existing) = self.streams.get(stream_id) {
            let cached = Arc::clone(existing.value());
            drop(existing);
            let mut guard = lock_cached_stream(&cached);
            match guard.as_ref() {
                Some(authority) => {
                    validate_authority_config(authority, stream_id, self.retention, &stream_dir)?;
                    validate_authority_storage(authority, &stream_dir)?;
                }
                None => {
                    *guard = Some(open_stream_authority(
                        &stream_dir,
                        stream_id,
                        self.retention,
                    )?);
                }
            }
            drop(guard);
            self.touch_stream(stream_id);
            self.evict_inactive_streams(Some(stream_id));
            return Ok(Some(cached));
        }
        let canonical = fs::canonicalize(&stream_dir).map_err(|source| ChatEventLogError::Io {
            path: stream_dir.clone(),
            source,
        })?;
        let shared = {
            let mut registry = stream_authority_registry().lock().map_err(|error| {
                corrupt(&stream_dir, format!("stream registry poisoned: {error}"))
            })?;
            if registry.len() > MAX_REGISTRY_ENTRIES_BEFORE_PRUNE {
                registry.retain(|_, authority| authority.strong_count() > 0);
            }
            if let Some(shared) = registry.get(&canonical).and_then(Weak::upgrade) {
                shared
            } else {
                let shared = Arc::new(Mutex::new(None));
                registry.insert(canonical, Arc::downgrade(&shared));
                shared
            }
        };
        {
            let mut guard = lock_cached_stream(&shared);
            match guard.as_ref() {
                Some(authority) => {
                    validate_authority_config(authority, stream_id, self.retention, &stream_dir)?;
                    validate_authority_storage(authority, &stream_dir)?;
                }
                None => {
                    *guard = Some(open_stream_authority(
                        &stream_dir,
                        stream_id,
                        self.retention,
                    )?);
                }
            }
        }
        let entry = self
            .streams
            .entry(stream_id.to_string())
            .or_insert_with(|| Arc::clone(&shared));
        let cached = Arc::clone(entry.value());
        drop(entry);
        self.touch_stream(stream_id);
        self.evict_inactive_streams(Some(stream_id));
        Ok(Some(cached))
    }

    fn enumerate_streams(&self) -> Result<Vec<EnumeratedStream>, ChatEventLogError> {
        if !ensure_real_directory(&self.root, false)? {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&self.root).map_err(|source| ChatEventLogError::Io {
            path: self.root.clone(),
            source,
        })?;
        let mut streams = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ChatEventLogError::Io {
                path: self.root.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| ChatEventLogError::Io {
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() {
                return Err(corrupt(&path, "chat event stream must not be a symlink"));
            }
            if !metadata.is_dir() {
                continue;
            }
            let journal = StreamJournal::open(
                &path,
                self.retention.segment_rollover_bytes,
                FileDurability::Flush,
            )
            .map_err(|error| journal_error(&path, error))?;
            let floor = journal.retention_metadata().retained_floor;
            let first = journal
                .replay_after(floor.saturating_sub(1), 1)
                .map_err(|error| journal_error(&path, error))?
                .into_iter()
                .next()
                .map(|record| envelope_from_record_for_enumeration(record, &path))
                .transpose()?;
            if let Some(first) = first {
                streams.push(EnumeratedStream {
                    stream_id: first.stream_id.clone(),
                    path,
                    first,
                });
            }
        }
        Ok(streams)
    }

    fn enumerate_streams_isolated(&self) -> Result<Vec<EnumeratedStream>, ChatEventLogError> {
        if !ensure_real_directory(&self.root, false)? {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&self.root).map_err(|source| ChatEventLogError::Io {
            path: self.root.clone(),
            source,
        })?;
        let mut streams = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(%error, "chat input boot recovery skipped unreadable directory entry");
                    continue;
                }
            };
            let path = entry.path();
            let inspected = (|| -> Result<Option<EnumeratedStream>, ChatEventLogError> {
                let metadata =
                    fs::symlink_metadata(&path).map_err(|source| ChatEventLogError::Io {
                        path: path.clone(),
                        source,
                    })?;
                if metadata.file_type().is_symlink() {
                    return Err(corrupt(&path, "chat event stream must not be a symlink"));
                }
                if !metadata.is_dir() {
                    return Ok(None);
                }
                let journal = StreamJournal::open(
                    &path,
                    self.retention.segment_rollover_bytes,
                    FileDurability::Flush,
                )
                .map_err(|error| journal_error(&path, error))?;
                let floor = journal.retention_metadata().retained_floor;
                let first = journal
                    .replay_after(floor.saturating_sub(1), 1)
                    .map_err(|error| journal_error(&path, error))?
                    .into_iter()
                    .next()
                    .map(|record| envelope_from_record_for_enumeration(record, &path))
                    .transpose()?;
                Ok(first.map(|first| EnumeratedStream {
                    stream_id: first.stream_id.clone(),
                    path: path.clone(),
                    first,
                }))
            })();
            match inspected {
                Ok(Some(stream)) => streams.push(stream),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "chat input boot recovery isolated a corrupt stream");
                }
            }
        }
        Ok(streams)
    }

    fn remove_stream(&self, stream_id: &str, path: &Path) -> Result<(), ChatEventLogError> {
        self.forget_stream(stream_id);
        let canonical = fs::canonicalize(path).map_err(|source| ChatEventLogError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let local = self.streams.remove(stream_id).map(|(_, cached)| cached);
        let registered = stream_authority_registry()
            .lock()
            .map_err(|error| corrupt(path, format!("stream registry poisoned: {error}")))?
            .get(&canonical)
            .and_then(Weak::upgrade);
        if let Some(cached) = local.or(registered) {
            let mut guard = lock_cached_stream(&cached);
            drop(guard.take());
            #[cfg(test)]
            if let Some(pause) = &self.deletion_pause {
                pause.0.wait();
                pause.1.wait();
            }
            let result = match fs::remove_dir_all(path) {
                Ok(()) => Ok(()),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(ChatEventLogError::Io {
                    path: path.to_path_buf(),
                    source,
                }),
            };
            drop(guard);
            if Arc::strong_count(&cached) == 1 {
                stream_authority_registry()
                    .lock()
                    .map_err(|error| corrupt(path, format!("stream registry poisoned: {error}")))?
                    .remove(&canonical);
            }
            return result;
        }
        fs::remove_dir_all(path).map_err(|source| ChatEventLogError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn stream_dir(&self, stream_id: &str) -> PathBuf {
        self.root.join(digest(stream_id.as_bytes()))
    }
}
