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
    ) -> Result<
        Vec<crate::tasks::task_runtime::command_cells::CommandCellWatchResult>,
        ChatEventLogError,
    > {
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
        if matches!(
            &event,
            ChatDriverEvent::InputLifecycle(_) | ChatDriverEvent::WeixinControlDelivery(_)
        ) {
            return Err(ChatEventLogError::InvalidEvent(
                "durable ingress/control writes must use their original owner".to_string(),
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
            && let Some(sequence) = authority
                .pins
                .command_cell_watch_facts
                .get(&fact_key)
                .copied()
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

    /// Full original stream discovery, not the 128-stream cache or a bounded
    /// transcript replay. Corrupt streams are isolated by the original query.
    #[cfg(any(test, feature = "channels"))]
    pub(crate) async fn channel_input_bindings(
        self: &Arc<Self>,
    ) -> Result<(Vec<ConversationInputProjection>, bool), ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let (streams, complete) = log.enumerate_streams_isolated_with_status()?;
            Ok::<_, ConversationInputError>(
                log.channel_input_bindings_from_streams(streams, complete),
            )
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    #[cfg(any(test, feature = "channels"))]
    fn channel_input_bindings_from_streams(
        &self,
        streams: Vec<EnumeratedStream>,
        mut complete: bool,
    ) -> (Vec<ConversationInputProjection>, bool) {
        let mut inputs = Vec::new();
        for stream in streams {
            // Discovery is not an absence proof if an original stream vanishes
            // or fails after enumeration. Preserve good streams independently.
            let cached = match self.stream_journal(&stream.stream_id, false) {
                Ok(Some(cached)) => cached,
                Ok(None) => {
                    complete = false;
                    continue;
                }
                Err(error) => {
                    complete = false;
                    tracing::warn!(stream_id = %stream.stream_id, %error, "channel input discovery isolated an unavailable authority");
                    continue;
                }
            };
            let mut guard = lock_cached_stream(&cached);
            let Some(authority) = guard.as_mut() else {
                complete = false;
                continue;
            };
            for entry in authority.pins.conversation_inputs.values() {
                if entry.projection.payload.channel_binding.is_some() {
                    inputs.push(normalized_conversation_input_projection(
                        entry,
                        authority.pins.queue_revision,
                    ));
                }
            }
        }
        (inputs, complete)
    }

    #[cfg(any(test, feature = "channels"))]
    pub(crate) async fn prove_unclaimed_channel_input(
        self: &Arc<Self>,
        identity: ConversationInputIdentity,
    ) -> Result<ConversationInputProjection, ConversationInputError> {
        let permit = PROCESS_CHAT_EVENT_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| ConversationInputError::Validation(error.to_string()))?;
        let log = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            log.with_conversation_input_authority(&identity.address, false, |authority, _, _| {
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
                    .filter(|current| current.receipt.identity == identity)
                    .ok_or_else(|| ConversationInputError::StaleRevision {
                        input_id: identity.input_id.clone(),
                    })?;
                validate_unclaimed_channel_input(authority, &current)?;
                Ok(current)
            })
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
    }

    #[cfg(feature = "channels")]
    pub(crate) async fn mark_channel_input_uncertain(
        self: &Arc<Self>,
        identity: ConversationInputIdentity,
        reason: String,
    ) -> Result<(), ConversationInputError> {
        self.mark_channel_input_uncertain_checked(identity, None, reason)
            .await
    }

    #[cfg(any(test, feature = "channels"))]
    pub(crate) async fn mark_channel_input_uncertain_checked(
        self: &Arc<Self>,
        identity: ConversationInputIdentity,
        expected_queue_revision: Option<u64>,
        reason: String,
    ) -> Result<(), ConversationInputError> {
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
                    if expected_queue_revision
                        .is_some_and(|expected| authority.pins.queue_revision != expected)
                    {
                        // A live original claimant may have advanced since the
                        // recovery snapshot. Never overwrite that owner's fact.
                        return Ok(());
                    }
                    let current = authority
                        .pins
                        .conversation_inputs
                        .get(&identity.input_id)
                        .ok_or_else(|| ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        })?;
                    if current.projection.receipt.identity != identity {
                        return Err(ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        });
                    }
                    log.append_conversation_input_event_locked(
                        authority,
                        stream_id,
                        path,
                        &identity.address,
                        &identity.input_id,
                        ChatDriverEvent::InputLifecycle(Box::new(
                            ConversationInputFact::RecoveryUncertain {
                                identity: identity.clone(),
                                reason,
                                detected_at_ms: echo_agent::utils::time::now_millis(),
                            },
                        )),
                    )
                },
            )
        })
        .await
        .map_err(|error| ConversationInputError::Validation(error.to_string()))?
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
                let fact = if projection.payload.channel_binding.is_some() {
                    ConversationInputFact::RecoveryUncertain {
                        identity: projection.receipt.identity.clone(),
                        reason: "previous channel execution owner is unavailable after restart"
                            .to_string(),
                        detected_at_ms: echo_agent::utils::time::now_millis(),
                    }
                } else if projection.receipt.drained {
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
    ) -> Result<
        Vec<crate::tasks::task_runtime::command_cells::CommandCellWatchResult>,
        ChatEventLogError,
    > {
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
    ) -> Result<
        Vec<crate::tasks::task_runtime::command_cells::CommandCellWatchResult>,
        ChatEventLogError,
    > {
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
            started.extend(authority.pins.started_command_cell_watches.values().map(
                |(_, acknowledgement)| {
                    (
                        stream.first.workspace_id.clone(),
                        stream.first.conversation_id.clone(),
                        stream.first.root_turn_id.clone(),
                        acknowledgement.clone(),
                    )
                },
            ));
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
        if authority.barrier_pending {
            return Err(ConversationInputError::Validation(
                "conversation input durability is unconfirmed".to_string(),
            ));
        }
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
                payload.validate_channel_digest(&address)?;
                if authority.barrier_pending {
                    return Err(ConversationInputError::Validation(
                        "conversation input durability is unconfirmed".to_string(),
                    ));
                }
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
                if next.task_target.as_ref().is_some_and(|target| {
                    target.turn_id != turn_id || target.identity != next.receipt.identity
                }) {
                    return Err(ConversationInputError::Validation(
                        "原输入已绑定准确任务；新轮次或编辑版本不能自动改绑".to_string(),
                    ));
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
        self.start_selected_conversation_input_checked(
            identity,
            expected_queue_revision,
            turn_id,
            false,
            None,
        )
        .await
    }

    pub(crate) async fn start_selected_conversation_input_checked(
        self: &Arc<Self>,
        identity: ConversationInputIdentity,
        expected_queue_revision: u64,
        turn_id: String,
        recovered_channel: bool,
        service_cancel: Option<tokio_util::sync::CancellationToken>,
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
                    if service_cancel
                        .as_ref()
                        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
                    {
                        return Err(ConversationInputError::Validation(
                            "channel service closed before original atomic claim".to_string(),
                        ));
                    }
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
                    if recovered_channel {
                        validate_unclaimed_channel_input(authority, &current)?;
                    }
                    if current.task_target.as_ref().is_some_and(|target| {
                        target.turn_id != turn_id || target.identity != current.receipt.identity
                    }) {
                        return Err(ConversationInputError::Validation(
                            "原输入已绑定准确任务；新轮次或编辑版本不能自动改绑".to_string(),
                        ));
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
        self.append_conversation_input_fact_inner(fact, None).await
    }

    pub async fn append_conversation_input_fact_expected_queue_revision(
        self: &Arc<Self>,
        fact: ConversationInputFact,
        expected_queue_revision: u64,
    ) -> Result<ConversationInputReceipt, ConversationInputError> {
        self.append_conversation_input_fact_inner(fact, Some(expected_queue_revision))
            .await
    }

    async fn append_conversation_input_fact_inner(
        self: &Arc<Self>,
        fact: ConversationInputFact,
        expected_queue_revision: Option<u64>,
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
                    if expected_queue_revision
                        .is_some_and(|expected| authority.pins.queue_revision != expected)
                    {
                        return Err(ConversationInputError::StaleRevision {
                            input_id: identity.input_id.clone(),
                        });
                    }
                    if matches!(&fact, ConversationInputFact::TaskRunTargetBound { .. })
                        && authority.barrier_pending
                    {
                        return Err(ConversationInputError::Validation(
                            "原输入去向绑定尚未确认持久化，未准许投递".to_string(),
                        ));
                    }
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
        if let ConversationInputFact::Persisted {
            identity: next,
            payload,
        } = fact
        {
            if next.input_id == current.receipt.identity.input_id
                && next.address == current.receipt.identity.address
                && next.revision == current.receipt.identity.revision.saturating_add(1)
                && next.payload_sha256 == payload.payload_sha256
                && next.payload_sha256 != current.payload.payload_sha256
                // Channel payloads carry the original account, context and
                // recovery intent. A GUI edit cannot erase that routing data.
                && current.payload.channel_binding.is_none()
                && payload.channel_binding.is_none()
                && current.receipt.is_dispatchable()
                && current.active_attempt.is_none()
            {
                return Ok(());
            }
        }
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
            | ConversationInputFact::TaskRunTargetBound { attempt, .. }
            | ConversationInputFact::DriverAccepted { attempt, .. }
            | ConversationInputFact::MailboxAccepted { attempt, .. }
            | ConversationInputFact::Drained { attempt, .. }
            | ConversationInputFact::TurnSettled { attempt, .. }
            | ConversationInputFact::Deferred { attempt, .. }
            | ConversationInputFact::RecoveryRequired { attempt, .. } => attempt,
            ConversationInputFact::Persisted { .. }
            | ConversationInputFact::Reordered { .. }
            | ConversationInputFact::RecoveryUncertain { .. }
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
        if let ConversationInputFact::TaskRunTargetBound { run_id, .. } = fact {
            if current.claimed_before_current && current.task_target.is_none() {
                return Err(ConversationInputError::Validation(
                    "旧输入已有claim而任务去向未知，保留待恢复，禁止改绑".to_string(),
                ));
            }
            if current.task_target.as_ref().is_some_and(|target| {
                target.run_id != *run_id
                    || target.turn_id != attempt.turn_id
                    || target.identity != attempt.identity
            }) {
                return Err(ConversationInputError::Validation(
                    "原输入已绑定其它任务、轮次或源版本，保留待恢复".to_string(),
                ));
            }
            if run_id.trim().is_empty() {
                return Err(ConversationInputError::Validation(
                    "原输入任务去向为空".to_string(),
                ));
            }
        }
        let valid = match fact {
            ConversationInputFact::TaskRunTargetBound { .. } => {
                current.receipt.phase == ConversationInputPhase::AttemptStarted
            }
            ConversationInputFact::DriverAccepted { .. }
            | ConversationInputFact::MailboxAccepted { .. } => {
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
            | ConversationInputFact::RecoveryUncertain { .. }
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
                let stream = EnumeratedStream {
                    stream_id: first.stream_id.clone(),
                    path,
                    first,
                };
                streams.push(stream);
            }
        }
        Ok(streams)
    }

    fn enumerate_streams_isolated(&self) -> Result<Vec<EnumeratedStream>, ChatEventLogError> {
        self.enumerate_streams_isolated_with_status()
            .map(|(streams, _complete)| streams)
    }

    fn enumerate_streams_isolated_with_status(
        &self,
    ) -> Result<(Vec<EnumeratedStream>, bool), ChatEventLogError> {
        if !ensure_real_directory(&self.root, false)? {
            return Ok((Vec::new(), true));
        }
        let entries = fs::read_dir(&self.root).map_err(|source| ChatEventLogError::Io {
            path: self.root.clone(),
            source,
        })?;
        let mut streams = Vec::new();
        let mut complete = true;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    complete = false;
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
                    complete = false;
                    tracing::warn!(path = %path.display(), %error, "chat input boot recovery isolated a corrupt stream");
                }
            }
        }
        Ok((streams, complete))
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

// Typed view over the SAME ordinary-chat physical journal. The frame identity,
// sequence, commit status and durability are mapped from the actual append;
// in-memory checkpoints are only a disposable reducer cache.
#[cfg(any(test, feature = "channels"))]
type WeixinControlEvent = echo_agent::state::delivery::DeliveryEvent<String, String>;
#[cfg(any(test, feature = "channels"))]
type WeixinControlClaim = echo_agent::state::delivery::DeliveryClaim<String, String>;

#[cfg(any(test, feature = "channels"))]
struct WeixinControlJournal<'a> {
    journal: &'a StreamJournal,
    workspace: &'a str,
    conversation: &'a str,
    root: &'a str,
    stream: &'a str,
}

#[cfg(any(test, feature = "channels"))]
impl WeixinControlJournal<'_> {
    fn physical_batch(
        &self,
        batch: &echo_agent::state::journal::PreparedJournalBatch<WeixinControlEvent>,
    ) -> echo_agent::error::Result<
        echo_agent::state::journal::PreparedJournalBatch<PersistedChatEvent>,
    > {
        use echo_agent::state::delivery::DeliveryEvent;
        let events = batch
            .events()
            .iter()
            .map(|event| {
                let timestamp = match event.as_ref() {
                    DeliveryEvent::Persisted { persisted_at, .. } => *persisted_at,
                    DeliveryEvent::Claimed { claimed_at, .. } => *claimed_at,
                    DeliveryEvent::EffectStarted { started_at, .. } => *started_at,
                    DeliveryEvent::MailboxAccepted { accepted_at, .. } => *accepted_at,
                    DeliveryEvent::Drained { drained_at, .. } => *drained_at,
                    DeliveryEvent::Deferred { deferred_at, .. } => *deferred_at,
                    DeliveryEvent::TurnSettled { settled_at, .. } => *settled_at,
                };
                PersistedChatEvent {
                    schema_version: CHAT_EVENT_SCHEMA_VERSION,
                    stream_id: self.stream.to_string(),
                    workspace_id: self.workspace.to_string(),
                    conversation_id: Some(self.conversation.to_string()),
                    root_turn_id: self.root.to_string(),
                    turn_id: self.root.to_string(),
                    message_id: self.root.to_string(),
                    timestamp,
                    payload: ChatDriverEvent::WeixinControlDelivery(Box::new(
                        event.as_ref().clone(),
                    )),
                }
            })
            .collect();
        echo_agent::state::journal::PreparedJournalBatch::with_identity(batch.batch_id(), events)
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))
    }

    fn typed_record(
        &self,
        record: JournalRecord<PersistedChatEvent>,
    ) -> echo_agent::error::Result<JournalRecord<WeixinControlEvent>> {
        if record.event.workspace_id != self.workspace
            || record.event.conversation_id.as_deref() != Some(self.conversation)
            || record.event.root_turn_id != self.root
            || record.event.stream_id != self.stream
        {
            return Err(echo_agent::error::ReactError::Other(
                "control journal scope mismatch".to_string(),
            ));
        }
        let ChatDriverEvent::WeixinControlDelivery(event) = &record.event.payload else {
            return Err(echo_agent::error::ReactError::Other(
                "control journal contains another event kind".to_string(),
            ));
        };
        Ok(JournalRecord::from_parts(
            record.batch_id(),
            record.sequence,
            event.as_ref().clone(),
        ))
    }

    fn typed_receipt(
        &self,
        receipt: echo_agent::state::journal::JournalBatchAppendReceipt<PersistedChatEvent>,
    ) -> echo_agent::error::Result<
        echo_agent::state::journal::JournalBatchAppendReceipt<WeixinControlEvent>,
    > {
        let records = receipt
            .records()
            .iter()
            .cloned()
            .map(|record| self.typed_record(record))
            .collect::<echo_agent::error::Result<Vec<_>>>()?;
        echo_agent::state::journal::JournalBatchAppendReceipt::from_parts(
            receipt.journal_identity().clone(),
            receipt.batch_id(),
            records,
            receipt.durability().clone(),
            receipt.commit_status(),
        )
        .map_err(echo_agent::error::ReactError::Other)
    }
}

#[cfg(any(test, feature = "channels"))]
impl EventJournal<WeixinControlEvent> for WeixinControlJournal<'_> {
    fn journal_identity(&self) -> &echo_agent::state::journal::JournalIdentity {
        self.journal.journal_identity()
    }
    fn append_batch(
        &self,
        batch: echo_agent::state::journal::PreparedJournalBatch<WeixinControlEvent>,
    ) -> echo_agent::state::journal::JournalBatchAppendResult<WeixinControlEvent> {
        use echo_agent::state::journal::JournalBatchAppendError as Failure;
        let physical = self
            .physical_batch(&batch)
            .map_err(|error| Failure::not_committed(batch.clone(), error.to_string()))?;
        let receipt = self
            .journal
            .append_batch_with_durability(physical, FileDurability::SyncData)
            .map_err(|error| match error {
                Failure::NotCommitted { error, .. } => Failure::not_committed(batch.clone(), error),
                Failure::OutcomeUnknown { error, .. } => {
                    Failure::outcome_unknown(batch.clone(), error)
                }
                Failure::IdentityConflict {
                    existing_first_sequence,
                    existing_record_count,
                    error,
                    ..
                } => Failure::identity_conflict(
                    batch.clone(),
                    existing_first_sequence,
                    existing_record_count,
                    error,
                ),
                Failure::PreparedMutation { error, .. } => {
                    Failure::prepared_mutation(batch.clone(), error)
                }
                Failure::AuthorityPoisoned { error, .. } => {
                    Failure::authority_poisoned(batch.clone(), error)
                }
            })?;
        self.typed_receipt(receipt)
            .map_err(|error| Failure::outcome_unknown(batch, error.to_string()))
    }
    fn lookup_batch(
        &self,
        batch: &echo_agent::state::journal::PreparedJournalBatch<WeixinControlEvent>,
    ) -> echo_agent::error::Result<echo_agent::state::journal::JournalBatchLookup<WeixinControlEvent>>
    {
        use echo_agent::state::journal::JournalBatchLookup;
        Ok(
            match self.journal.lookup_batch(&self.physical_batch(batch)?)? {
                JournalBatchLookup::Absent => JournalBatchLookup::Absent,
                JournalBatchLookup::Conflict { error } => JournalBatchLookup::Conflict { error },
                JournalBatchLookup::AlreadyCommitted(receipt) => {
                    JournalBatchLookup::AlreadyCommitted(self.typed_receipt(receipt)?)
                }
            },
        )
    }
    fn next_sequence(&self) -> u64 {
        self.journal.next_sequence()
    }
    fn last_sequence(&self) -> u64 {
        self.journal.last_sequence()
    }
    fn retained_floor(&self) -> u64 {
        self.journal.retained_floor()
    }
    fn replay_after(
        &self,
        after: u64,
        limit: usize,
    ) -> echo_agent::error::Result<Vec<JournalRecord<WeixinControlEvent>>> {
        self.journal
            .replay_after(after, limit)?
            .into_iter()
            .map(|record| self.typed_record(record))
            .collect()
    }
}

#[cfg(any(test, feature = "channels"))]
pub(crate) enum WeixinControlAdmission {
    Execute(WeixinControlLease),
    Handled,
    TerminalRejected,
    RecoveryRequired,
    InFlight,
}

/// The live owner is kept until the original control response producer settles.
/// Drop never claims success; the next replay persists OutcomeUnknown.
#[cfg(any(test, feature = "channels"))]
enum WeixinControlTerminal {
    Handled,
    Rejected,
    RecoveryRequired,
}

#[cfg(any(test, feature = "channels"))]
pub(crate) struct WeixinControlLease {
    cached: CachedStreamJournal,
    workspace: String,
    conversation: String,
    root: String,
    stream: String,
    claim: WeixinControlClaim,
}

#[cfg(any(test, feature = "channels"))]
impl Drop for WeixinControlLease {
    fn drop(&mut self) {
        if let Some(authority) = lock_cached_stream(&self.cached).as_mut() {
            authority.weixin_control_active = false;
        }
    }
}

#[cfg(any(test, feature = "channels"))]
fn control_error(error: impl std::fmt::Display) -> ChatEventLogError {
    ChatEventLogError::InvalidEvent(format!("Weixin control journal: {error}"))
}

#[cfg(any(test, feature = "channels"))]
fn require_control_durability(
    receipt: echo_agent::state::journal::ApplyReceipt,
) -> Result<(), ChatEventLogError> {
    match receipt.journal {
        JournalDurabilityStatus::Confirmed => Ok(()),
        JournalDurabilityStatus::Unconfirmed | JournalDurabilityStatus::Degraded { .. } => Err(
            control_error("durability barrier incomplete; effect was not admitted"),
        ),
    }
}

#[cfg(any(test, feature = "channels"))]
fn with_control_ledger<T>(
    authority: &mut StreamAuthority,
    workspace: &str,
    conversation: &str,
    root: &str,
    stream: &str,
    operation: impl FnOnce(
        &echo_agent::state::delivery::DeliveryLedger<WeixinControlJournal<'_>, String, String>,
    ) -> Result<T, ChatEventLogError>,
) -> Result<T, ChatEventLogError> {
    use echo_agent::state::delivery::{DeliveryLedger, DeliveryLedgerConfig};
    use echo_agent::state::journal::MemoryCheckpointStore;
    // Recovered or degraded facts are not an ACK until their original physical
    // authority confirms a barrier. This does not append/retry any effect.
    authority.journal.sync_data().map_err(control_error)?;
    authority.barrier_pending = false;
    let before = authority.journal.last_sequence();
    let result = {
        let journal = Arc::new(WeixinControlJournal {
            journal: &authority.journal,
            workspace,
            conversation,
            root,
            stream,
        });
        // 同一会话物理 journal 混有 DATA、普通 turn 与其它控制 root。
        // 复用 TaskRuntime 微信投影模式：只折叠准确 root 的控制事实，checkpoint
        // 标记同一次锁内读取的物理 head，不能把过滤后的最后一条当作物理 head。
        use echo_agent::state::delivery::DeliveryLedgerProjection;
        use echo_agent::state::journal::{CheckpointStore, EventReducer};
        if authority.journal.retained_floor() > 1 {
            // 保留原 fresh ledger 对缺失历史前缀的拒绝，不凭当前可见后缀授权重放。
            return Err(control_error("控制投影缺少原始前缀，不能由后缀重建"));
        }
        let physical_head = authority.journal.last_sequence();
        let mut projection = DeliveryLedgerProjection::<String, String>::default();
        let mut cursor = 0;
        while cursor < physical_head {
            let records = authority
                .journal
                .replay_after(cursor, REPLAY_BATCH_SIZE)
                .map_err(control_error)?;
            if records.is_empty() {
                return Err(control_error("控制投影未读取到已冻结的原 journal head"));
            }
            for record in records {
                if record.sequence <= cursor || record.sequence > physical_head {
                    return Err(control_error("控制投影的物理游标不连续推进"));
                }
                cursor = record.sequence;
                // 先复用原整条记录校验，不能把其它 root 的损坏事实当作无关数据过滤。
                validate_persisted_record(&record, authority.journal.directory(), Some(stream))
                    .map_err(control_error)?;
                if record.event.workspace_id != workspace
                    || record.event.conversation_id.as_deref() != Some(conversation)
                    || record.event.stream_id != stream
                {
                    return Err(control_error("control journal scope mismatch"));
                }
                if record.event.root_turn_id == root {
                    // 当前 root 继续使用原严格类型/身份校验；其它合法 root 不属于此投影。
                    projection.apply_record(&journal.typed_record(record).map_err(control_error)?);
                }
            }
        }
        let config = DeliveryLedgerConfig::default();
        projection.validate(config).map_err(control_error)?;
        let checkpoints = Arc::new(MemoryCheckpointStore::new());
        checkpoints
            .save(journal.journal_identity(), &projection, physical_head)
            .map_err(control_error)?;
        let ledger = DeliveryLedger::new(journal, checkpoints, config, 16);
        ledger.recover().map_err(control_error)?;
        operation(&ledger)
    };
    // Fold actual committed records even if a later durability/checkpoint step
    // failed. A failed caller must never erase a committed effect-start fact.
    for record in authority
        .journal
        .replay_after(before, 16)
        .map_err(control_error)?
    {
        authority.pins.apply(record.sequence, record.event.as_ref());
    }
    if result.is_err() && authority.journal.last_sequence() > before {
        authority.barrier_pending = true;
    }
    result
}

impl ChatEventLog {
    #[cfg(any(test, feature = "channels"))]
    pub(crate) fn claim_weixin_control(
        &self,
        workspace: &str,
        conversation: &str,
        root: &str,
        payload_hash: &str,
    ) -> Result<WeixinControlAdmission, ChatEventLogError> {
        use echo_agent::state::delivery::{
            DeliveryClaim, DeliveryEnvelope, DeliveryOutcome, DeliverySettlement,
        };
        let stream = stream_id(workspace, Some(conversation), root)?;
        let cached = self
            .stream_journal(&stream, true)?
            .ok_or_else(|| control_error("missing stream"))?;
        let mut guard = lock_cached_stream(&cached);
        let authority = guard
            .as_mut()
            .ok_or_else(|| control_error("removed stream"))?;
        if authority.weixin_control_active {
            return Ok(WeixinControlAdmission::InFlight);
        }
        let admission = with_control_ledger(
            authority,
            workspace,
            conversation,
            root,
            &stream,
            |ledger| {
                if let Some(record) =
                    ledger.with_projection(|projection| projection.record(root).cloned())
                {
                    if record.payload != payload_hash || record.route != conversation {
                        return Err(control_error(
                            "provider message identity reused with a different control payload",
                        ));
                    }
                    if record.outcome == Some(DeliveryOutcome::Completed) {
                        return Ok(Err(WeixinControlTerminal::Handled));
                    }
                    if record.outcome == Some(DeliveryOutcome::Dropped) {
                        return Ok(Err(WeixinControlTerminal::Rejected));
                    }
                    if record.outcome == Some(DeliveryOutcome::OutcomeUnknown) {
                        return Ok(Err(WeixinControlTerminal::RecoveryRequired));
                    }
                    if record.effect_started_at.is_some() {
                        let claim = DeliveryClaim {
                            message_id: record.message_id,
                            route: record.route,
                            payload: record.payload,
                            attempt_id: record
                                .attempt_id
                                .ok_or_else(|| control_error("effect has no attempt"))?,
                            attempt: record.attempt,
                            claimed_at: record
                                .claimed_at
                                .ok_or_else(|| control_error("effect has no claim time"))?,
                        };
                        require_control_durability(ledger.settle(&claim, DeliverySettlement::terminal(
                        Some(root.to_string()), DeliveryOutcome::OutcomeUnknown, None,
                        Some("Original control owner ended without a durable result; explicit recovery is required".to_string()), None,
                    )).map_err(control_error)?)?;
                        return Ok(Err(WeixinControlTerminal::RecoveryRequired));
                    }
                } else {
                    require_control_durability(
                        ledger
                            .enqueue(DeliveryEnvelope::new(
                                root,
                                conversation.to_string(),
                                payload_hash.to_string(),
                            ))
                            .map_err(control_error)?,
                    )?;
                }
                let claim = ledger
                    .claim_next()
                    .map_err(control_error)?
                    .ok_or_else(|| control_error("control cannot be claimed"))?;
                require_control_durability(
                    ledger.begin_effect(&claim, root).map_err(control_error)?,
                )?;
                Ok(Ok(claim))
            },
        )?;
        match admission {
            Err(WeixinControlTerminal::Handled) => Ok(WeixinControlAdmission::Handled),
            Err(WeixinControlTerminal::Rejected) => Ok(WeixinControlAdmission::TerminalRejected),
            Err(WeixinControlTerminal::RecoveryRequired) => {
                Ok(WeixinControlAdmission::RecoveryRequired)
            }
            Ok(claim) => {
                authority.weixin_control_active = true;
                drop(guard);
                Ok(WeixinControlAdmission::Execute(WeixinControlLease {
                    cached,
                    workspace: workspace.to_string(),
                    conversation: conversation.to_string(),
                    root: root.to_string(),
                    stream,
                    claim,
                }))
            }
        }
    }
}

#[cfg(any(test, feature = "channels"))]
impl WeixinControlLease {
    pub(crate) fn complete(&self) -> Result<(), ChatEventLogError> {
        self.finish(
            echo_agent::state::delivery::DeliveryOutcome::Completed,
            None,
        )
    }

    pub(crate) fn reject_with_reason(&self, reason: String) -> Result<(), ChatEventLogError> {
        self.finish(
            echo_agent::state::delivery::DeliveryOutcome::Dropped,
            Some(reason),
        )
    }

    pub(crate) fn recovery_required(&self, reason: String) -> Result<(), ChatEventLogError> {
        self.finish(
            echo_agent::state::delivery::DeliveryOutcome::OutcomeUnknown,
            Some(reason),
        )
    }

    /// Only the original effect owner may prove that no business operation
    /// was admitted. The existing ledger's Deferred transition clears the
    /// conservative effect marker and permits the SAME message/payload retry.
    pub(crate) fn defer(&self, reason: String) -> Result<(), ChatEventLogError> {
        let mut guard = lock_cached_stream(&self.cached);
        let authority = guard
            .as_mut()
            .ok_or_else(|| control_error("removed active control stream"))?;
        with_control_ledger(
            authority,
            &self.workspace,
            &self.conversation,
            &self.root,
            &self.stream,
            |ledger| {
                require_control_durability(
                    ledger
                        .defer(&self.claim, reason, chrono::Utc::now())
                        .map_err(control_error)?,
                )
            },
        )
    }

    fn finish(
        &self,
        outcome: echo_agent::state::delivery::DeliveryOutcome,
        reason: Option<String>,
    ) -> Result<(), ChatEventLogError> {
        use echo_agent::state::delivery::DeliverySettlement;
        let mut guard = lock_cached_stream(&self.cached);
        let authority = guard
            .as_mut()
            .ok_or_else(|| control_error("removed active control stream"))?;
        with_control_ledger(
            authority,
            &self.workspace,
            &self.conversation,
            &self.root,
            &self.stream,
            |ledger| {
                require_control_durability(
                    ledger
                        .settle(
                            &self.claim,
                            DeliverySettlement::terminal(
                                Some(self.root.clone()),
                                outcome,
                                None,
                                reason,
                                None,
                            ),
                        )
                        .map_err(control_error)?,
                )
            },
        )
    }
}

#[cfg(test)]
mod weixin_control_tests {
    use super::*;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn original_control_is_not_executed_twice_while_active_or_after_completion() -> TestResult {
        let root = tempfile::tempdir()?;
        let log = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        let WeixinControlAdmission::Execute(lease) =
            log.claim_weixin_control("workspace", "account-peer", "control:1", "payload-hash")?
        else {
            return Err("first request did not own execution".into());
        };
        assert!(matches!(
            log.claim_weixin_control("workspace", "account-peer", "control:1", "payload-hash")?,
            WeixinControlAdmission::InFlight
        ));
        lease.complete()?;
        drop(lease);
        assert!(matches!(
            log.claim_weixin_control("workspace", "account-peer", "control:1", "payload-hash")?,
            WeixinControlAdmission::Handled
        ));
        assert!(
            log.claim_weixin_control(
                "workspace",
                "account-peer",
                "control:1",
                "different-payload"
            )
            .is_err()
        );
        drop(log);
        let reopened = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        assert!(matches!(
            reopened.claim_weixin_control(
                "workspace",
                "account-peer",
                "control:1",
                "payload-hash"
            )?,
            WeixinControlAdmission::Handled
        ));
        Ok(())
    }

    #[test]
    fn original_no_admission_proof_survives_restart_and_retries_only_same_payload() -> TestResult {
        let root = tempfile::tempdir()?;
        let log = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        let WeixinControlAdmission::Execute(lease) = log.claim_weixin_control(
            "workspace",
            "account-peer",
            "control:deferred",
            "original-payload",
        )?
        else {
            return Err("first request did not own execution".into());
        };
        lease.defer("Original foreground owner reported Busy before admission".to_string())?;
        drop(lease);
        drop(log);
        let reopened = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        assert!(
            reopened
                .claim_weixin_control(
                    "workspace",
                    "account-peer",
                    "control:deferred",
                    "conflicting-payload",
                )
                .is_err()
        );
        let WeixinControlAdmission::Execute(retry) = reopened.claim_weixin_control(
            "workspace",
            "account-peer",
            "control:deferred",
            "original-payload",
        )?
        else {
            return Err(
                "same original payload was not retryable after durable no-admission".into(),
            );
        };
        retry.complete()?;
        drop(retry);
        assert!(matches!(
            reopened.claim_weixin_control(
                "workspace",
                "account-peer",
                "control:deferred",
                "original-payload",
            )?,
            WeixinControlAdmission::Handled
        ));
        Ok(())
    }

    #[test]
    fn original_unknown_result_is_durable_before_recovery_observation() -> TestResult {
        let root = tempfile::tempdir()?;
        let log = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        let WeixinControlAdmission::Execute(lease) = log.claim_weixin_control(
            "workspace",
            "account-peer",
            "control:unknown",
            "original-payload",
        )?
        else {
            return Err("first request did not own execution".into());
        };
        lease.recovery_required(
            "Original cancellation owner could not confirm settlement".to_string(),
        )?;
        let replay = log.replay("workspace", Some("account-peer"), "control:unknown", 0)?;
        assert!(replay.events.iter().any(|event| matches!(&event.payload,
        ChatDriverEvent::WeixinControlDelivery(fact)
        if matches!(fact.as_ref(), echo_agent::state::delivery::DeliveryEvent::TurnSettled {
            outcome: echo_agent::state::delivery::DeliveryOutcome::OutcomeUnknown, ..
        }))));
        drop(lease);
        drop(log);
        let reopened = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        assert!(matches!(
            reopened.claim_weixin_control(
                "workspace",
                "account-peer",
                "control:unknown",
                "original-payload",
            )?,
            WeixinControlAdmission::RecoveryRequired
        ));
        Ok(())
    }

    #[test]
    fn rejected_control_stays_rejected_on_provider_replay() -> TestResult {
        let root = tempfile::tempdir()?;
        let log = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        let WeixinControlAdmission::Execute(lease) = log.claim_weixin_control(
            "workspace",
            "account-peer",
            "control:rejected",
            "payload-hash",
        )?
        else {
            return Err("first request did not own execution".into());
        };
        lease.reject_with_reason("original provider rejected the control".to_string())?;
        drop(lease);
        drop(log);
        let reopened = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        assert!(matches!(
            reopened.claim_weixin_control(
                "workspace",
                "account-peer",
                "control:rejected",
                "payload-hash"
            )?,
            WeixinControlAdmission::TerminalRejected
        ));
        Ok(())
    }

    #[test]
    fn dropped_effect_owner_requires_durable_recovery_without_replay() -> TestResult {
        let root = tempfile::tempdir()?;
        let log = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        let WeixinControlAdmission::Execute(lease) =
            log.claim_weixin_control("workspace", "account-peer", "control:2", "payload-hash")?
        else {
            return Err("first request did not own execution".into());
        };
        // Simulate cancellation after effect start / before the completion append.
        drop(lease);
        drop(log);
        let reopened = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        assert!(matches!(
            reopened.claim_weixin_control(
                "workspace",
                "account-peer",
                "control:2",
                "payload-hash"
            )?,
            WeixinControlAdmission::RecoveryRequired
        ));
        let replay = reopened.replay("workspace", Some("account-peer"), "control:2", 0)?;
        assert!(replay.events.iter().any(|event| matches!(&event.payload,
        ChatDriverEvent::WeixinControlDelivery(fact)
        if matches!(fact.as_ref(), echo_agent::state::delivery::DeliveryEvent::TurnSettled {
            outcome: echo_agent::state::delivery::DeliveryOutcome::OutcomeUnknown, ..
        }))));
        assert!(matches!(
            reopened.claim_weixin_control(
                "workspace",
                "account-peer",
                "control:2",
                "payload-hash"
            )?,
            WeixinControlAdmission::RecoveryRequired
        ));
        Ok(())
    }

    #[tokio::test]
    async fn mixed_data_prefix_and_control_roots_keep_the_same_physical_authority() -> TestResult {
        use crate::conversation_input::{
            ChannelExecutionIntent, ChannelInputBinding, FrozenChannelContext,
        };
        let root = tempfile::tempdir()?;
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        let address = ConversationInputAddress {
            workspace_id: "global".to_string(),
            conversation_id: "account-peer".to_string(),
        };
        for ordinal in 0..40 {
            let input = format!("input-{ordinal}");
            let payload = ConversationInputPayload::new_for_channel(
                &address,
                format!("原输入 {ordinal}"),
                Vec::new(),
                None,
                ChannelInputBinding {
                    version: 1,
                    channel_id: "weixin".to_string(),
                    account_id: "account".to_string(),
                    credential_generation: "original-generation".to_string(),
                    peer_id: "peer".to_string(),
                    provider_message_id: format!("provider-{ordinal}"),
                    workspace_generation: "global".to_string(),
                    context: FrozenChannelContext::Absent,
                    execution: ChannelExecutionIntent::Chat,
                },
            )?;
            log.submit_conversation_input(address.clone(), input, payload)
                .await?;
        }
        let physical = stream_id("global", Some("account-peer"), "control:first")?;
        assert_eq!(
            physical,
            stream_id("global", Some("account-peer"), "input-0")?
        );
        let WeixinControlAdmission::Execute(first) =
            log.claim_weixin_control("global", "account-peer", "control:first", "digest:first")?
        else {
            return Err("首个控制没有实际 claim".into());
        };
        // 同一物理 stream 在控制效果后仍可能写普通 turn，不能制造 reducer sequence gap。
        log.append(
            "global",
            Some("account-peer"),
            "ordinary-turn",
            ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
        )?;
        first.complete()?;
        drop(first);
        let WeixinControlAdmission::Execute(second) =
            log.claim_weixin_control("global", "account-peer", "control:second", "digest:second")?
        else {
            return Err("第二控制被其它 root 的记录遮蔽".into());
        };
        second.reject_with_reason("原控制明确拒绝".to_string())?;
        drop(second);
        assert_eq!(
            log.conversation_input_frontier(&address).await?.items.len(),
            40
        );
        drop(log);

        let reopened = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        assert!(matches!(
            reopened.claim_weixin_control(
                "global",
                "account-peer",
                "control:first",
                "digest:first"
            )?,
            WeixinControlAdmission::Handled
        ));
        assert!(matches!(
            reopened.claim_weixin_control(
                "global",
                "account-peer",
                "control:second",
                "digest:second"
            )?,
            WeixinControlAdmission::TerminalRejected
        ));
        assert!(
            reopened
                .claim_weixin_control("global", "account-peer", "control:first", "changed-digest")
                .is_err()
        );
        let history = reopened.replay("global", Some("account-peer"), "control:first", 0)?;
        assert_eq!(
            history
                .events
                .iter()
                .filter(|event| matches!(&event.payload, ChatDriverEvent::InputLifecycle(_)))
                .count(),
            40
        );
        assert!(
            history
                .events
                .iter()
                .any(|event| event.root_turn_id == "ordinary-turn")
        );
        Ok(())
    }

    #[test]
    fn same_control_root_with_another_event_kind_still_fails_before_effect() -> TestResult {
        let root = tempfile::tempdir()?;
        let log = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        log.append(
            "workspace",
            Some("account-peer"),
            "control:collision",
            ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            },
        )?;
        let before = log.replay("workspace", Some("account-peer"), "control:collision", 0)?;
        let error = match log.claim_weixin_control(
            "workspace",
            "account-peer",
            "control:collision",
            "digest",
        ) {
            Err(error) => error,
            Ok(_) => return Err("同 root 的非控制事件被错误当成控制历史".into()),
        };
        assert!(
            error
                .to_string()
                .contains("control journal contains another event kind")
        );
        let after = log.replay("workspace", Some("account-peer"), "control:collision", 0)?;
        assert_eq!(before.events.len(), after.events.len());
        assert!(
            !after
                .events
                .iter()
                .any(|event| matches!(&event.payload, ChatDriverEvent::WeixinControlDelivery(_)))
        );
        Ok(())
    }

    #[test]
    fn adapter_preserves_physical_batch_identity_and_scope() -> TestResult {
        use echo_agent::state::delivery::{DeliveryEnvelope, DeliveryEvent};
        use echo_agent::state::journal::{JournalBatchLookup, PreparedJournalBatch};
        let root = tempfile::tempdir()?;
        let log = ChatEventLog::open(root.path(), ChatEventRetention::default())?;
        let stream = stream_id("workspace", Some("account-peer"), "control:3")?;
        let cached = log.stream_journal(&stream, true)?.ok_or("missing stream")?;
        let guard = lock_cached_stream(&cached);
        let authority = guard.as_ref().ok_or("missing authority")?;
        let adapter = WeixinControlJournal {
            journal: &authority.journal,
            workspace: "workspace",
            conversation: "account-peer",
            root: "control:3",
            stream: &stream,
        };
        let batch = PreparedJournalBatch::new(vec![DeliveryEvent::Persisted {
            envelope: DeliveryEnvelope::new(
                "control:3",
                "account-peer".to_string(),
                "digest-only".to_string(),
            ),
            persisted_at: Utc::now(),
        }])?;
        let expected_batch_id = batch.batch_id().to_string();
        let receipt = adapter.append_batch(batch.clone())?;
        assert_eq!(receipt.batch_id(), expected_batch_id.as_str());
        assert_eq!(
            receipt.journal_identity(),
            authority.journal.journal_identity()
        );
        assert!(batch.matches_receipt(&receipt)?);
        let physical = authority.journal.replay_after(0, 1)?;
        assert_eq!(
            physical.first().ok_or("missing physical event")?.batch_id(),
            expected_batch_id.as_str()
        );
        assert!(matches!(
            adapter.lookup_batch(&batch)?,
            JournalBatchLookup::AlreadyCommitted(_)
        ));
        let wrong_scope = WeixinControlJournal {
            journal: &authority.journal,
            workspace: "another-workspace",
            conversation: "account-peer",
            root: "control:3",
            stream: &stream,
        };
        assert!(wrong_scope.replay_after(0, 1).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod channel_recovery_tests {
    use super::*;
    use crate::conversation_input::{
        ChannelExecutionIntent, ChannelInputBinding, FrozenChannelContext,
    };

    fn address(conversation: &str) -> ConversationInputAddress {
        ConversationInputAddress {
            workspace_id: "global".into(),
            conversation_id: conversation.into(),
        }
    }
    fn payload(
        address: &ConversationInputAddress,
        provider: &str,
    ) -> Result<ConversationInputPayload, ConversationInputError> {
        ConversationInputPayload::new_for_channel(
            address,
            "原输入🙂".into(),
            Vec::new(),
            None,
            ChannelInputBinding {
                version: 1,
                channel_id: "weixin".into(),
                account_id: "account".into(),
                credential_generation: "generation".into(),
                peer_id: "peer".into(),
                provider_message_id: provider.into(),
                workspace_generation: "global".into(),
                context: FrozenChannelContext::Absent,
                execution: ChannelExecutionIntent::Chat,
            },
        )
    }

    #[tokio::test]
    async fn stream_removed_after_discovery_is_not_a_complete_negative_lookup()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        let good = address("surviving");
        let gone = address("removed");
        let saved = log
            .submit_conversation_input(
                good.clone(),
                "good".into(),
                payload(&good, "provider-good")?,
            )
            .await?;
        log.submit_conversation_input(
            gone.clone(),
            "gone".into(),
            payload(&gone, "provider-gone")?,
        )
        .await?;
        let (streams, complete) = log.enumerate_streams_isolated_with_status()?;
        assert!(complete);
        log.remove_conversation(&gone.workspace_id, &gone.conversation_id)?;
        let (items, complete) = log.channel_input_bindings_from_streams(streams, complete);
        assert!(
            !complete,
            "lost source authority cannot prove provider input absence"
        );
        assert_eq!(items.len(), 1);
        assert_eq!(
            items.first().map(|input| &input.receipt.identity),
            Some(&saved.identity)
        );
        log.prove_unclaimed_channel_input(saved.identity).await?;
        Ok(())
    }

    #[tokio::test]
    async fn clean_original_persisted_input_reopens_and_claims_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let target = address("conversation");
        let identity = {
            let log = Arc::new(ChatEventLog::open(
                root.path(),
                ChatEventRetention::default(),
            )?);
            log.submit_conversation_input(
                target.clone(),
                "original-input".into(),
                payload(&target, "provider")?,
            )
            .await?
            .identity
        };
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        let proven = log.prove_unclaimed_channel_input(identity.clone()).await?;
        assert_eq!(proven.receipt.phase, ConversationInputPhase::Persisted);
        let claimed = log
            .start_selected_conversation_input_checked(
                identity.clone(),
                proven.receipt.queue_revision,
                "real-original-turn".into(),
                true,
                None,
            )
            .await?;
        assert_eq!(claimed.receipt.attempt, Some(1));
        assert!(
            log.prove_unclaimed_channel_input(identity.clone())
                .await
                .is_err()
        );
        assert!(
            log.start_selected_conversation_input_checked(
                identity,
                proven.receipt.queue_revision,
                "duplicate-turn".into(),
                true,
                None
            )
            .await
            .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn live_original_deferred_retry_is_distinct_from_lost_route_recovery()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        let service = crate::conversation_input::ConversationInputService::new(log.clone());
        let target = address("same-live-owner");
        let saved = log
            .submit_conversation_input(
                target.clone(),
                "input".into(),
                payload(&target, "provider")?,
            )
            .await?;
        let first = log
            .start_selected_conversation_input_checked(
                saved.identity.clone(),
                saved.queue_revision,
                "first-admission".into(),
                false,
                None,
            )
            .await?;
        let deferred = service
            .deferred(
                first.active_attempt.ok_or("original attempt missing")?,
                "original live mailbox confirmed pre-effect deferral".into(),
            )
            .await?;
        assert!(
            log.start_selected_conversation_input_checked(
                saved.identity.clone(),
                deferred.queue_revision,
                "forbidden-recovered-retry".into(),
                true,
                None
            )
            .await
            .is_err()
        );
        let live = log
            .start_selected_conversation_input_checked(
                saved.identity,
                deferred.queue_revision,
                "same-original-owner-retry".into(),
                false,
                None,
            )
            .await?;
        assert_eq!(live.receipt.attempt, Some(2));
        Ok(())
    }

    #[tokio::test]
    async fn deferred_after_actual_claim_is_not_never_started_on_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let target = address("deferred");
        let identity = {
            let log = Arc::new(ChatEventLog::open(
                root.path(),
                ChatEventRetention::default(),
            )?);
            let service = crate::conversation_input::ConversationInputService::new(log.clone());
            let saved = log
                .submit_conversation_input(
                    target.clone(),
                    "input".into(),
                    payload(&target, "provider")?,
                )
                .await?;
            let claimed = service
                .dispatch_selected(
                    saved.identity.clone(),
                    saved.queue_revision,
                    "original-turn".into(),
                )
                .await?;
            let attempt = claimed.active_attempt.ok_or("original attempt missing")?;
            service
                .deferred(attempt, "actual previous admission was attempted".into())
                .await?;
            saved.identity
        };
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        assert!(log.prove_unclaimed_channel_input(identity).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn stale_boot_observation_does_not_overwrite_a_new_live_claim()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        let target = address("boot-live-race");
        let saved = log
            .submit_conversation_input(
                target.clone(),
                "input".into(),
                payload(&target, "provider")?,
            )
            .await?;
        let claimed = log
            .start_selected_conversation_input_checked(
                saved.identity.clone(),
                saved.queue_revision,
                "current-owner".into(),
                true,
                None,
            )
            .await?;
        log.mark_channel_input_uncertain_checked(
            saved.identity.clone(),
            Some(saved.queue_revision),
            "stale boot snapshot".into(),
        )
        .await?;
        let (items, complete) = log.channel_input_bindings().await?;
        assert!(complete);
        let current = items
            .iter()
            .find(|input| input.receipt.identity == saved.identity)
            .ok_or("current input missing")?;
        assert_eq!(
            current.receipt.phase,
            ConversationInputPhase::AttemptStarted
        );
        assert_eq!(current.receipt.attempt_id, claimed.receipt.attempt_id);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_service_cannot_acquire_original_atomic_claim()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        let target = address("cancelled");
        let saved = log
            .submit_conversation_input(
                target.clone(),
                "input".into(),
                payload(&target, "provider")?,
            )
            .await?;
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        assert!(
            log.start_selected_conversation_input_checked(
                saved.identity.clone(),
                saved.queue_revision,
                "must-not-start".into(),
                true,
                Some(cancel)
            )
            .await
            .is_err()
        );
        assert_eq!(
            log.prove_unclaimed_channel_input(saved.identity)
                .await?
                .receipt
                .attempt,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn uncommitted_torn_suffix_keeps_verified_persisted_input_recoverable()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Write;
        let root = tempfile::tempdir()?;
        let target = address("torn");
        let (identity, active_path) = {
            let log = Arc::new(ChatEventLog::open(
                root.path(),
                ChatEventRetention::default(),
            )?);
            let saved = log
                .submit_conversation_input(
                    target.clone(),
                    "input".into(),
                    payload(&target, "provider")?,
                )
                .await?;
            let selected = stream_id("global", Some("torn"), "torn")?;
            let cached = log
                .stream_journal(&selected, false)?
                .ok_or("stream missing")?;
            let guard = lock_cached_stream(&cached);
            let active = guard
                .as_ref()
                .ok_or("authority missing")?
                .journal
                .segments()
                .into_iter()
                .last()
                .ok_or("active segment missing")?
                .path;
            (saved.identity, active)
        };
        let mut file = fs::OpenOptions::new().append(true).open(active_path)?;
        file.write_all(b"{torn-original-tail")?;
        file.sync_data()?;
        drop(file);
        {
            let log = Arc::new(ChatEventLog::open(
                root.path(),
                ChatEventRetention::default(),
            )?);
            let _ = log.enumerate_streams_isolated()?;
            let proven = log.prove_unclaimed_channel_input(identity.clone()).await?;
            assert_eq!(proven.receipt.phase, ConversationInputPhase::Persisted);
        }
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        let proven = log.prove_unclaimed_channel_input(identity.clone()).await?;
        let claimed = log
            .start_selected_conversation_input_checked(
                identity.clone(),
                proven.receipt.queue_revision,
                "one-recovered-driver".into(),
                true,
                None,
            )
            .await?;
        assert_eq!(claimed.receipt.attempt, Some(1));
        assert!(log.prove_unclaimed_channel_input(identity).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn complete_corrupt_tail_is_isolated_without_hiding_independent_safe_data()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Write;
        let root = tempfile::tempdir()?;
        let good = address("good-prefix");
        let bad = address("bad-complete-tail");
        let (good_identity, bad_identity, active_path) = {
            let log = Arc::new(ChatEventLog::open(
                root.path(),
                ChatEventRetention::default(),
            )?);
            let good_saved = log
                .submit_conversation_input(
                    good.clone(),
                    "good".into(),
                    payload(&good, "provider-good")?,
                )
                .await?;
            let bad_saved = log
                .submit_conversation_input(
                    bad.clone(),
                    "bad".into(),
                    payload(&bad, "provider-bad")?,
                )
                .await?;
            let selected = stream_id("global", Some(&bad.conversation_id), &bad.conversation_id)?;
            let cached = log
                .stream_journal(&selected, false)?
                .ok_or("bad stream missing")?;
            let guard = lock_cached_stream(&cached);
            let active_path = guard
                .as_ref()
                .ok_or("bad authority missing")?
                .journal
                .segments()
                .into_iter()
                .last()
                .ok_or("bad active segment missing")?
                .path;
            (good_saved.identity, bad_saved.identity, active_path)
        };
        let mut file = fs::OpenOptions::new().append(true).open(active_path)?;
        // A complete newline-terminated bad frame is NOT an incomplete append.
        file.write_all(b"{complete-corrupt-frame}\n")?;
        file.sync_data()?;
        drop(file);
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        let (inputs, complete) = log.channel_input_bindings().await?;
        assert!(
            !complete,
            "corruption is not a clean provider-identity absence"
        );
        assert_eq!(inputs.len(), 1);
        assert_eq!(
            inputs.first().map(|input| &input.receipt.identity),
            Some(&good_identity)
        );
        log.prove_unclaimed_channel_input(good_identity).await?;
        assert!(
            log.prove_unclaimed_channel_input(bad_identity)
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn full_original_discovery_exceeds_stream_cache_and_replay_batch()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        {
            let log = Arc::new(ChatEventLog::open(
                root.path(),
                ChatEventRetention::default(),
            )?);
            for ordinal in 0..140_u32 {
                let target = address(&format!("conversation-{ordinal}"));
                log.submit_conversation_input(
                    target.clone(),
                    format!("input-{ordinal}"),
                    payload(&target, &format!("provider-{ordinal}"))?,
                )
                .await?;
            }
            for _ in 0..4200_u32 {
                log.append(
                    "global",
                    Some("conversation-0"),
                    "history",
                    ChatDriverEvent::TurnStatus {
                        status: "running".into(),
                    },
                )?;
            }
        }
        let log = Arc::new(ChatEventLog::open(
            root.path(),
            ChatEventRetention::default(),
        )?);
        let (inputs, complete) = log.channel_input_bindings().await?;
        assert!(complete);
        assert_eq!(inputs.len(), 140);
        for input in inputs {
            log.prove_unclaimed_channel_input(input.receipt.identity)
                .await?;
        }
        Ok(())
    }
}
