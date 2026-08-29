impl AgentRouter {
    pub fn at_default_root() -> Arc<Self> {
        Arc::new(Self::new(crate::data_root::user_data_path("agent-router")))
    }

    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            inboxes: Arc::new(AgentInboxRegistry::default()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Discover inbox targets from the router-owned target manifests.
    ///
    /// This is metadata discovery only: delivery phases and message history
    /// remain in each target journal. The manifest lets a cold process expose
    /// `agent_list` without creating a second address or status store.
    pub async fn list_targets(&self) -> Result<Vec<AgentAddress>, AgentRouterError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let inbox_root = root.join("inboxes");
            let workspaces = match std::fs::read_dir(&inbox_root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(source) => {
                    return Err(AgentRouterError::Io {
                        path: inbox_root,
                        source,
                    });
                }
            };
            let mut targets = Vec::new();
            for workspace_entry in workspaces {
                let workspace_entry = workspace_entry.map_err(|source| AgentRouterError::Io {
                    path: inbox_root.clone(),
                    source,
                })?;
                let workspace_path = workspace_entry.path();
                if !workspace_path.is_dir() {
                    continue;
                }
                let conversations =
                    std::fs::read_dir(&workspace_path).map_err(|source| AgentRouterError::Io {
                        path: workspace_path.clone(),
                        source,
                    })?;
                for conversation_entry in conversations {
                    let conversation_entry =
                        conversation_entry.map_err(|source| AgentRouterError::Io {
                            path: workspace_path.clone(),
                            source,
                        })?;
                    let manifest = conversation_entry.path().join("target.json");
                    let bytes = match std::fs::read(&manifest) {
                        Ok(bytes) => bytes,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(source) => {
                            return Err(AgentRouterError::Io {
                                path: manifest,
                                source,
                            });
                        }
                    };
                    let target =
                        serde_json::from_slice::<AgentAddress>(&bytes).map_err(|error| {
                            AgentRouterError::Corrupt {
                                path: manifest,
                                message: error.to_string(),
                            }
                        })?;
                    target.validate()?;
                    targets.push(target);
                }
            }
            targets.sort_by(|left, right| {
                left.workspace_id
                    .as_str()
                    .cmp(right.workspace_id.as_str())
                    .then_with(|| left.conversation_id.cmp(&right.conversation_id))
            });
            targets.dedup();
            Ok(targets)
        })
        .await
        .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    /// Check whether an inbox target has a persisted manifest without opening
    /// its journal. Read-only inspect/wait adapters use this to reject unknown
    /// addresses instead of creating phantom inboxes as a side effect.
    pub async fn target_exists(&self, target: &AgentAddress) -> Result<bool, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let target = target.clone();
        tokio::task::spawn_blocking(move || {
            let manifest = inbox_dir(&root, &target).join("target.json");
            match std::fs::metadata(&manifest) {
                Ok(metadata) => Ok(metadata.is_file()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(source) => Err(AgentRouterError::Io {
                    path: manifest,
                    source,
                }),
            }
        })
        .await
        .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub fn begin_target_retirement(
        &self,
        target: AgentAddress,
    ) -> Result<AgentRouterRetirementGuard, AgentRouterError> {
        target.validate()?;
        {
            let _lifecycle = self
                .inboxes
                .lifecycle
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            if self
                .inboxes
                .retiring_workspaces
                .contains_key(&target.workspace_id)
                || self
                    .inboxes
                    .retiring_targets
                    .insert(target.clone(), ())
                    .is_some()
            {
                return Err(AgentRouterError::Retiring {
                    workspace_id: target.workspace_id.to_string(),
                    conversation_id: Some(target.conversation_id),
                });
            }
        }
        let marker = Arc::new(AgentRouterRetirementMarker {
            registry: Arc::clone(&self.inboxes),
            target: Some(target.clone()),
            workspace_id: None,
        });
        let guard = AgentRouterRetirementGuard {
            _marker: marker,
            root: self.root.clone(),
            inboxes: Arc::clone(&self.inboxes),
            scope: AgentRouterRetirementScope::Target(target),
        };
        Ok(guard)
    }

    pub async fn retire_target(
        &self,
        target: AgentAddress,
    ) -> Result<AgentRouterRetirementGuard, AgentRouterError> {
        let guard = self.begin_target_retirement(target)?;
        guard.purge().await?;
        Ok(guard)
    }

    pub fn begin_workspace_retirement(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<AgentRouterRetirementGuard, AgentRouterError> {
        {
            let _lifecycle = self
                .inboxes
                .lifecycle
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            if self.inboxes.retiring_workspaces.contains_key(&workspace_id)
                || self
                    .inboxes
                    .retiring_targets
                    .iter()
                    .any(|entry| entry.key().workspace_id == workspace_id)
            {
                return Err(AgentRouterError::Retiring {
                    workspace_id: workspace_id.to_string(),
                    conversation_id: None,
                });
            }
            self.inboxes
                .retiring_workspaces
                .insert(workspace_id.clone(), ());
        }
        let marker = Arc::new(AgentRouterRetirementMarker {
            registry: Arc::clone(&self.inboxes),
            target: None,
            workspace_id: Some(workspace_id.clone()),
        });
        let guard = AgentRouterRetirementGuard {
            _marker: marker,
            root: self.root.clone(),
            inboxes: Arc::clone(&self.inboxes),
            scope: AgentRouterRetirementScope::Workspace(workspace_id),
        };
        Ok(guard)
    }

    pub async fn retire_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<AgentRouterRetirementGuard, AgentRouterError> {
        let guard = self.begin_workspace_retirement(workspace_id)?;
        guard.purge().await?;
        Ok(guard)
    }

    pub async fn list_groups(&self) -> Result<Vec<AgentGroup>, AgentRouterError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || list_groups_sync(&root))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn create_group(
        &self,
        name: impl Into<String>,
        leader: AgentAddress,
        members: Vec<AgentGroupMember>,
    ) -> Result<AgentGroup, AgentRouterError> {
        let now = Utc::now();
        let group = AgentGroup {
            group_id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            leader,
            members,
            created_at: now,
            updated_at: now,
        };
        group.validate()?;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || create_group_sync(&root, group))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn update_group(
        &self,
        group_id: impl Into<String>,
        name: impl Into<String>,
        leader: AgentAddress,
        members: Vec<AgentGroupMember>,
    ) -> Result<AgentGroup, AgentRouterError> {
        let group_id = group_id.into();
        let name = name.into();
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            update_group_sync(&root, &group_id, name, leader, members)
        })
        .await
        .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn delete_group(&self, group_id: &str) -> Result<bool, AgentRouterError> {
        if group_id.trim().is_empty() {
            return Err(AgentRouterError::Validation(
                "Agent group id must not be empty".to_string(),
            ));
        }
        let root = self.root.clone();
        let group_id = group_id.to_string();
        tokio::task::spawn_blocking(move || delete_group_sync(&root, &group_id))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    /// Persist a message once within the retained target inbox window.
    /// Repeating a retained `message_id` returns the original acceptance;
    /// identities evicted by either terminal retention bound may be admitted
    /// again immediately after eviction.
    pub async fn enqueue(
        &self,
        message: AgentMessage,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        message.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        tokio::task::spawn_blocking(move || enqueue_sync(&root, &inboxes, message))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn pending(
        &self,
        target: &AgentAddress,
    ) -> Result<Vec<AgentMessage>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || pending_sync(&root, &inboxes, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn claim_next(
        &self,
        target: &AgentAddress,
    ) -> Result<Option<AgentDeliveryClaim>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || claim_next_sync(&root, &inboxes, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn next_attempt_at(
        &self,
        target: &AgentAddress,
    ) -> Result<Option<DateTime<Utc>>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || next_attempt_at_sync(&root, &inboxes, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    /// Return the exact non-terminal attempt whose input already reached model
    /// context. Cold recovery must reconcile this attempt against transcript
    /// facts and must never create a new claim that could replay side effects.
    pub async fn in_flight_claim(
        &self,
        target: &AgentAddress,
    ) -> Result<Option<AgentDeliveryInFlight>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || in_flight_claim_sync(&root, &inboxes, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    pub async fn defer(
        &self,
        claim: &AgentDeliveryClaim,
        reason: impl Into<String>,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        let next_attempt_at = retry_deadline(claim.attempt);
        self.settle_claim(
            claim,
            ClaimSettlement::Deferred {
                reason: reason.into(),
                next_attempt_at,
            },
        )
        .await
    }

    pub async fn begin_injection(
        &self,
        claim: &AgentDeliveryClaim,
        turn_id: impl Into<String>,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        self.settle_claim(
            claim,
            ClaimSettlement::EffectStarted {
                turn_id: turn_id.into(),
            },
        )
        .await
    }

    pub(crate) async fn mailbox_accepted(
        &self,
        claim: &AgentDeliveryClaim,
        turn_id: impl Into<String>,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        self.settle_claim(
            claim,
            ClaimSettlement::MailboxAccepted {
                turn_id: turn_id.into(),
            },
        )
        .await
    }

    pub(crate) async fn drained(
        &self,
        claim: &AgentDeliveryClaim,
        turn_id: impl Into<String>,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        self.settle_claim(
            claim,
            ClaimSettlement::Drained {
                turn_id: turn_id.into(),
            },
        )
        .await
    }

    // These fields are the terminal fact itself; keeping them explicit avoids
    // a second mutable builder or status authority at call sites.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn turn_settled(
        &self,
        claim: &AgentDeliveryClaim,
        turn_id: Option<String>,
        outcome: AgentDeliveryOutcome,
        drained: bool,
        reason: Option<String>,
        retryable: bool,
        reply_message_id: Option<String>,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        let next_attempt_at = retryable.then(|| retry_deadline(claim.attempt));
        self.settle_claim(
            claim,
            ClaimSettlement::TurnSettled {
                turn_id,
                outcome,
                drained: Some(drained),
                reason,
                retryable,
                next_attempt_at,
                reply_message_id,
            },
        )
        .await
    }

    /// Return the retained terminal window followed by the complete frontier.
    pub async fn records(
        &self,
        target: &AgentAddress,
    ) -> Result<Vec<AgentDeliveryRecord>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || records_sync(&root, &inboxes, &target))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    /// Return the authoritative event cursor for one exact inbox target.
    ///
    /// The cursor is the journal sequence, not a derived delivery phase. It is
    /// intentionally exposed as a read-only primitive so application adapters
    /// can implement bounded wait without owning a second inbox or status
    /// reducer.
    pub async fn event_cursor(&self, target: &AgentAddress) -> Result<u64, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        tokio::task::spawn_blocking(move || {
            let authority = authority_for(&root, &inboxes, &target)?;
            let guard = authority
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            let state = guard.as_ref().ok_or_else(|| AgentRouterError::Corrupt {
                path: authority.directory.clone(),
                message: "Agent inbox authority is closed".to_string(),
            })?;
            Ok(state.journal.last_sequence())
        })
        .await
        .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    #[cfg(test)]
    pub(crate) async fn event_phases_for_test(
        &self,
        target: &AgentAddress,
        message_id: &str,
    ) -> Result<Vec<&'static str>, AgentRouterError> {
        target.validate()?;
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let target = target.clone();
        let message_id = message_id.to_string();
        tokio::task::spawn_blocking(move || {
            let authority = authority_for(&root, &inboxes, &target)?;
            let guard = authority
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            let state = guard.as_ref().ok_or_else(|| AgentRouterError::Corrupt {
                path: authority.directory.clone(),
                message: "Agent inbox authority is closed".to_string(),
            })?;
            let mut after = 0;
            let mut phases = Vec::new();
            loop {
                let records = state
                    .journal
                    .replay_after(after, 256)
                    .map_err(|error| journal_error(&authority.directory, error))?;
                if records.is_empty() {
                    break;
                }
                for record in records {
                    after = record.sequence;
                    let (event_message_id, phase) = match record.event.as_ref() {
                        AgentInboxEvent::Persisted { message, .. } => {
                            (message.message_id.as_str(), "persisted")
                        }
                        AgentInboxEvent::Claimed { message_id, .. } => {
                            (message_id.as_str(), "claimed")
                        }
                        AgentInboxEvent::EffectStarted { message_id, .. } => {
                            (message_id.as_str(), "effect_started")
                        }
                        AgentInboxEvent::MailboxAccepted { message_id, .. } => {
                            (message_id.as_str(), "mailbox_accepted")
                        }
                        AgentInboxEvent::Drained { message_id, .. } => {
                            (message_id.as_str(), "drained")
                        }
                        AgentInboxEvent::Deferred { message_id, .. } => {
                            (message_id.as_str(), "deferred")
                        }
                        AgentInboxEvent::TurnSettled { message_id, .. } => {
                            (message_id.as_str(), "turn_settled")
                        }
                    };
                    if event_message_id == message_id {
                        phases.push(phase);
                    }
                }
            }
            Ok(phases)
        })
        .await
        .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }

    async fn settle_claim(
        &self,
        claim: &AgentDeliveryClaim,
        settlement: ClaimSettlement,
    ) -> Result<AgentDeliveryReceipt, AgentRouterError> {
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let claim = claim.clone();
        tokio::task::spawn_blocking(move || settle_claim_sync(&root, &inboxes, &claim, settlement))
            .await
            .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }
}
