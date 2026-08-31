use echo_agent::state::journal::JournalDurabilityStatus;

impl AgentInboxAuthority {
    fn open(root: &Path, target: &AgentAddress) -> Result<Arc<Self>, AgentRouterError> {
        let inbox = inbox_dir(root, target);
        let directory = inbox.join("journal");
        let checkpoint_path = inbox.join("projection.checkpoint.json");
        let state = Self::open_state(&directory, &checkpoint_path)?;
        let manifest = inbox.join("target.json");
        let encoded = serde_json::to_vec(target).map_err(|error| AgentRouterError::Corrupt {
            path: manifest.clone(),
            message: error.to_string(),
        })?;
        echo_agent::utils::fs::atomic_write(&manifest, &encoded).map_err(|source| {
            AgentRouterError::Io {
                path: manifest,
                source,
            }
        })?;
        Ok(Arc::new(Self {
            directory,
            checkpoint_path,
            expected_target: target.clone(),
            operation: StdMutex::new(()),
            state: StdMutex::new(Some(state)),
        }))
    }

    fn open_state(
        directory: &Path,
        checkpoint_path: &Path,
    ) -> Result<AgentInboxAuthorityState, AgentRouterError> {
        let config = echo_agent::delivery::DeliveryLedgerConfig {
            terminal_retention: INBOX_TERMINAL_RETENTION,
            terminal_retention_bytes: INBOX_TERMINAL_RETENTION_BYTES,
        };
        let journal = Arc::new(
            SegmentedFileEventJournal::<FrameworkDeliveryEvent>::open(
                directory,
                INBOX_SEGMENT_BYTES,
                FileDurability::SyncData,
            )
            .map_err(|error| journal_error(directory, error))?,
        );
        let checkpoints = Arc::new(FileCheckpointStore::<FrameworkDeliveryProjection>::open(
            checkpoint_path,
        ));
        let ledger = FrameworkDeliveryLedger::new(
            Arc::clone(&journal),
            checkpoints as Arc<dyn CheckpointStore<FrameworkDeliveryProjection>>,
            config,
            INBOX_CHECKPOINT_EVERY,
        );
        ledger
            .recover()
            .map_err(|error| journal_error(directory, error))?;
        Ok(AgentInboxAuthorityState {
            framework: AgentFrameworkState {
                journal,
                ledger,
                checkpoint_path: checkpoint_path.to_path_buf(),
                durability_debt: None,
            },
        })
    }

    fn with_projection<T>(
        &self,
        operation: impl FnOnce(&FrameworkDeliveryProjection) -> Result<T, AgentRouterError>,
    ) -> Result<T, AgentRouterError> {
        let guard = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        let state = guard.as_ref().ok_or_else(|| AgentRouterError::Corrupt {
            path: self.directory.clone(),
            message: "Agent inbox authority is closed".to_string(),
        })?;
        state.framework.ledger.with_projection(|projection| {
            self.validate_routes(projection)?;
            operation(projection)
        })
    }

    fn validate_routes(
        &self,
        projection: &FrameworkDeliveryProjection,
    ) -> Result<(), AgentRouterError> {
        for record in projection.records() {
            if record.route != self.expected_target {
                return Err(AgentRouterError::Corrupt {
                    path: self.directory.clone(),
                    message: format!(
                        "message {} targets another Agent address",
                        record.message_id
                    ),
                });
            }
        }
        Ok(())
    }

    /// Access the framework ledger after checking this authority's route
    /// boundary. Callers can prepare semantic lifecycle events without
    /// reaching into the reducer or projection implementation.
    fn with_ledger<T>(
        &self,
        operation: impl FnOnce(&FrameworkDeliveryLedger) -> Result<T, AgentRouterError>,
    ) -> Result<T, AgentRouterError> {
        let guard = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        let state = guard.as_ref().ok_or_else(|| AgentRouterError::Corrupt {
            path: self.directory.clone(),
            message: "Agent inbox authority is closed".to_string(),
        })?;
        state
            .framework
            .ledger
            .with_projection(|projection| self.validate_routes(projection))?;
        operation(&state.framework.ledger)
    }

    fn lock_operation(&self) -> Result<std::sync::MutexGuard<'_, ()>, AgentRouterError> {
        self.operation
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)
    }

    fn append(&self, event: FrameworkDeliveryEvent) -> Result<JournalDurabilityStatus, AgentRouterError> {
        let prepared =
            PreparedJournalBatch::new(vec![event]).map_err(|error| AgentRouterError::Corrupt {
                path: self.directory.clone(),
                message: error.to_string(),
            })?;
        let batch_id = prepared.batch_id().to_string();
        let mut prepared = Some(prepared);
        let mut attempts = 0_usize;
        let mut guard = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        loop {
            attempts = attempts.saturating_add(1);
            let state = guard.as_mut().ok_or_else(|| AgentRouterError::Corrupt {
                path: self.directory.clone(),
                message: "Agent inbox authority is closed".to_string(),
            })?;
            Self::retry_durability_debt(state, &self.directory);
            let batch = prepared.take().ok_or_else(|| AgentRouterError::Corrupt {
                path: self.directory.clone(),
                message: "prepared Agent delivery batch ownership was lost".to_string(),
            })?;
            let mut receipt = match state
                .framework
                .ledger
                .apply_prepared_with(batch, |batch| {
                    state.framework.journal.append_batch(batch)
                }) {
                Ok(receipt) => receipt,
                Err(DeliveryLedgerError::Apply(error))
                    if matches!(
                        error.as_ref(),
                        CheckpointedApplyError::Journal(
                            JournalBatchAppendError::NotCommitted { .. }
                        )
                    ) && attempts < MAX_INBOX_APPEND_ATTEMPTS =>
                {
                    let batch = error.into_prepared().ok_or_else(|| AgentRouterError::Corrupt {
                        path: self.directory.clone(),
                        message: "framework delivery append lost retry batch".to_string(),
                    })?;
                    prepared = Some(batch);
                    continue;
                }
                Err(DeliveryLedgerError::Apply(error))
                    if matches!(
                        error.as_ref(),
                        CheckpointedApplyError::Journal(
                            JournalBatchAppendError::NotCommitted { .. }
                        )
                    ) =>
                {
                    return Err(AgentRouterError::AppendNotCommitted {
                        batch_id,
                        attempts,
                        detail: error.to_string(),
                    });
                }
                Err(DeliveryLedgerError::Apply(error)) if error.requires_reopen() => {
                    let detail = error.to_string();
                    let batch = error.into_prepared().ok_or_else(|| {
                        AgentRouterError::AppendOutcomeUnknown {
                            batch_id: batch_id.clone(),
                            detail: "framework delivery journal did not return prepared batch"
                                .to_string(),
                        }
                    })?;
                    let stale = guard.take();
                    drop(stale);
                    let reopened = Self::open_state(
                        &self.directory,
                        &self.checkpoint_path,
                    )
                    .map_err(|error| AgentRouterError::AppendOutcomeUnknown {
                        batch_id: batch_id.clone(),
                        detail: format!("{detail}; verified reopen failed: {error}"),
                    })?;
                    match reopened.framework.ledger.lookup_batch(&batch).map_err(|error| {
                        AgentRouterError::AppendOutcomeUnknown {
                            batch_id: batch_id.clone(),
                            detail: format!("{detail}; lookup failed: {error}"),
                        }
                    })? {
                        JournalBatchLookup::AlreadyCommitted(receipt) => {
                            *guard = Some(reopened);
                            return Ok(receipt.durability().clone());
                        }
                        JournalBatchLookup::Absent if attempts < MAX_INBOX_APPEND_ATTEMPTS => {
                            *guard = Some(reopened);
                            prepared = Some(batch);
                            continue;
                        }
                        JournalBatchLookup::Absent => {
                            return Err(AgentRouterError::AppendOutcomeUnknown {
                                batch_id,
                                detail: format!(
                                    "{detail}; batch remained absent after {attempts} attempts"
                                ),
                            });
                        }
                        JournalBatchLookup::Conflict { error } => {
                            return Err(AgentRouterError::AppendIdentityConflict {
                                batch_id,
                                detail: error,
                            });
                        }
                    }
                }
                Err(DeliveryLedgerError::Apply(error)) => {
                    return Err(AgentRouterError::AppendIdentityConflict {
                        batch_id,
                        detail: error.to_string(),
                    });
                }
                Err(DeliveryLedgerError::InvalidEvent { error, .. })
                | Err(DeliveryLedgerError::InvalidBatch { error, .. }) => {
                    return Err(AgentRouterError::Corrupt {
                        path: self.directory.clone(),
                        message: error,
                    });
                }
                Err(DeliveryLedgerError::CommittedInvariant { error, .. }) => {
                    let stale = guard.take();
                    drop(stale);
                    return Err(AgentRouterError::AppendOutcomeUnknown {
                        batch_id,
                        detail: error,
                    });
                }
            };
            match &receipt.journal {
                JournalDurabilityStatus::Confirmed => state.framework.durability_debt = None,
                JournalDurabilityStatus::Unconfirmed => {
                    state.framework.durability_debt = Some(format!(
                        "Agent delivery batch {} has unconfirmed durability",
                        receipt.batch_id
                    ));
                }
                JournalDurabilityStatus::Degraded { error } => {
                    state.framework.durability_debt = Some(error.clone());
                }
            }
            Self::retry_durability_debt(state, &self.directory);
            receipt.journal = state
                .framework
                .durability_debt
                .clone()
                .map_or(JournalDurabilityStatus::Confirmed, |error| {
                    JournalDurabilityStatus::Degraded { error }
                });
            if let CheckpointApplyStatus::Degraded { error } = &receipt.checkpoint {
                tracing::warn!(path = %state.framework.checkpoint_path.display(), %error, "Agent delivery checkpoint write is degraded; authoritative ledger remains committed");
            }
            Self::maintain_retention(state, &self.directory);
            return Ok(receipt.journal);
        }
    }

    fn retry_durability_debt(state: &mut AgentInboxAuthorityState, directory: &Path) {
        if state.framework.durability_debt.is_some() {
            match state.framework.journal.sync_data() {
                Ok(()) => state.framework.durability_debt = None,
                Err(error) => {
                    state.framework.durability_debt = Some(error.to_string());
                    tracing::warn!(path = %directory.display(), %error, "Agent inbox durability debt remains pending");
                }
            }
        }
    }

    fn maintain_retention(state: &mut AgentInboxAuthorityState, directory: &Path) {
        let segments = state.framework.journal.segments();
        if segments.len() <= INBOX_MAX_SEGMENTS {
            return;
        }
        if let Err(error) = state.framework.ledger.checkpoint() {
            tracing::warn!(path = %directory.display(), %error, "Agent inbox checkpoint compaction is degraded");
            return;
        }
        let keep_from = segments
            .get(segments.len().saturating_sub(INBOX_MAX_SEGMENTS))
            .map(|segment| segment.start_sequence)
            .unwrap_or(1);
        if let Err(error) = state
            .framework
            .journal
            .prune_closed_segments_before(keep_from)
        {
            tracing::warn!(path = %directory.display(), %error, "Agent inbox segment cleanup remains pending");
        }
    }

    fn close(&self) -> Result<(), AgentRouterError> {
        let _operation = self.lock_operation()?;
        let stale = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?
            .take();
        drop(stale);
        Ok(())
    }
}
