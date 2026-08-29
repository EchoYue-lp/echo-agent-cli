enum ClaimSettlement {
    EffectStarted {
        turn_id: String,
    },
    MailboxAccepted {
        turn_id: String,
    },
    Drained {
        turn_id: String,
    },
    Deferred {
        reason: String,
        next_attempt_at: DateTime<Utc>,
    },
    TurnSettled {
        turn_id: Option<String>,
        outcome: AgentDeliveryOutcome,
        drained: Option<bool>,
        reason: Option<String>,
        retryable: bool,
        next_attempt_at: Option<DateTime<Utc>>,
        reply_message_id: Option<String>,
    },
}

impl AgentInboxAuthority {
    fn open(root: &Path, target: &AgentAddress) -> Result<Arc<Self>, AgentRouterError> {
        let inbox = inbox_dir(root, target);
        let directory = inbox.join("journal");
        let checkpoint_path = inbox.join("projection.checkpoint.json");
        let state = Self::open_state(&directory, &checkpoint_path, target)?;
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
        target: &AgentAddress,
    ) -> Result<AgentInboxAuthorityState, AgentRouterError> {
        let journal = Arc::new(
            SegmentedFileEventJournal::open(
                directory,
                INBOX_SEGMENT_BYTES,
                FileDurability::SyncData,
            )
            .map_err(|error| journal_error(directory, error))?,
        );
        let checkpoints = Arc::new(FileCheckpointStore::open(checkpoint_path));
        let reducer = CheckpointedReducer::new(
            Arc::clone(&journal),
            checkpoints as Arc<dyn CheckpointStore<AgentInboxProjection>>,
            INBOX_CHECKPOINT_EVERY,
        );
        reducer
            .recover()
            .map_err(|error| journal_error(directory, error))?;
        reducer.with_state(|projection| projection.validate(directory, target))?;
        Ok(AgentInboxAuthorityState {
            journal,
            reducer,
            durability_debt: None,
        })
    }

    fn with_projection<T>(
        &self,
        operation: impl FnOnce(&AgentInboxProjection) -> Result<T, AgentRouterError>,
    ) -> Result<T, AgentRouterError> {
        let guard = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        let state = guard.as_ref().ok_or_else(|| AgentRouterError::Corrupt {
            path: self.directory.clone(),
            message: "Agent inbox authority is closed".to_string(),
        })?;
        state.reducer.with_state(|projection| operation(projection))
    }

    fn lock_operation(&self) -> Result<std::sync::MutexGuard<'_, ()>, AgentRouterError> {
        self.operation
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)
    }

    fn append(&self, event: AgentInboxEvent) -> Result<JournalDurabilityStatus, AgentRouterError> {
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
                message: "prepared Agent inbox batch ownership was lost".to_string(),
            })?;
            let mut receipt = match state.reducer.apply_batch(batch) {
                Ok(receipt) => receipt,
                Err(CheckpointedApplyError::Journal(JournalBatchAppendError::NotCommitted {
                    batch,
                    error,
                })) if attempts < MAX_INBOX_APPEND_ATTEMPTS => {
                    prepared = Some(batch);
                    tracing::warn!(%batch_id, attempts, %error, "retrying uncommitted Agent inbox batch");
                    continue;
                }
                Err(CheckpointedApplyError::Journal(JournalBatchAppendError::NotCommitted {
                    error,
                    ..
                })) => {
                    return Err(AgentRouterError::AppendNotCommitted {
                        batch_id,
                        attempts,
                        detail: error,
                    });
                }
                Err(CheckpointedApplyError::Journal(error)) if error.requires_reopen() => {
                    let detail = error.to_string();
                    let batch = error.into_prepared().ok_or_else(|| {
                        AgentRouterError::AppendOutcomeUnknown {
                            batch_id: batch_id.clone(),
                            detail: "journal did not return prepared batch ownership".to_string(),
                        }
                    })?;
                    let stale = guard.take();
                    drop(stale);
                    let reopened = Self::open_state(
                        &self.directory,
                        &self.checkpoint_path,
                        &self.expected_target,
                    )
                    .map_err(|error| {
                        AgentRouterError::AppendOutcomeUnknown {
                            batch_id: batch_id.clone(),
                            detail: format!("{detail}; verified reopen failed: {error}"),
                        }
                    })?;
                    match reopened.journal.lookup_batch(&batch).map_err(|error| {
                        AgentRouterError::AppendOutcomeUnknown {
                            batch_id: batch_id.clone(),
                            detail: format!("{detail}; lookup failed: {error}"),
                        }
                    })? {
                        JournalBatchLookup::AlreadyCommitted(_) => {
                            *guard = Some(reopened);
                            prepared = Some(batch);
                            continue;
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
                Err(CheckpointedApplyError::Journal(error)) => {
                    return Err(AgentRouterError::AppendIdentityConflict {
                        batch_id,
                        detail: error.to_string(),
                    });
                }
                Err(CheckpointedApplyError::CommittedInvariant { error, .. }) => {
                    let stale = guard.take();
                    drop(stale);
                    return Err(AgentRouterError::AppendOutcomeUnknown {
                        batch_id,
                        detail: error,
                    });
                }
                Err(CheckpointedApplyError::Prepare(error)) => {
                    return Err(AgentRouterError::Corrupt {
                        path: self.directory.clone(),
                        message: error.to_string(),
                    });
                }
            };
            state
                .reducer
                .with_state(|projection| projection.ensure_incremental_valid(&self.directory))?;
            match &receipt.journal {
                JournalDurabilityStatus::Confirmed => state.durability_debt = None,
                JournalDurabilityStatus::Unconfirmed => {
                    state.durability_debt = Some(format!(
                        "Agent inbox batch {} has unconfirmed durability",
                        receipt.batch_id
                    ));
                }
                JournalDurabilityStatus::Degraded { error } => {
                    state.durability_debt = Some(error.clone());
                }
            }
            Self::retry_durability_debt(state, &self.directory);
            receipt.journal = state
                .durability_debt
                .clone()
                .map_or(JournalDurabilityStatus::Confirmed, |error| {
                    JournalDurabilityStatus::Degraded { error }
                });
            if let CheckpointApplyStatus::Degraded { error } = &receipt.checkpoint {
                tracing::warn!(path = %self.checkpoint_path.display(), %error, "Agent inbox checkpoint write is degraded; authoritative journal remains committed");
            }
            Self::maintain_retention(state, &self.directory);
            return Ok(receipt.journal);
        }
    }

    fn retry_durability_debt(state: &mut AgentInboxAuthorityState, directory: &Path) {
        if state.durability_debt.is_some() {
            match state.journal.sync_data() {
                Ok(()) => state.durability_debt = None,
                Err(error) => {
                    state.durability_debt = Some(error.to_string());
                    tracing::warn!(path = %directory.display(), %error, "Agent inbox durability debt remains pending");
                }
            }
        }
    }

    fn maintain_retention(state: &mut AgentInboxAuthorityState, directory: &Path) {
        let segments = state.journal.segments();
        if segments.len() <= INBOX_MAX_SEGMENTS {
            return;
        }
        if let Err(error) = state.reducer.checkpoint() {
            tracing::warn!(path = %directory.display(), %error, "Agent inbox checkpoint compaction is degraded");
            return;
        }
        let keep_from = segments
            .get(segments.len().saturating_sub(INBOX_MAX_SEGMENTS))
            .map(|segment| segment.start_sequence)
            .unwrap_or(1);
        if let Err(error) = state.journal.prune_closed_segments_before(keep_from) {
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
