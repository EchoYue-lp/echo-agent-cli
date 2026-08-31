#[derive(Debug, thiserror::Error)]
pub enum AgentRouterError {
    #[error("invalid Agent message: {0}")]
    Validation(String),
    #[error("Agent message id '{message_id}' already identifies different content")]
    IdCollision { message_id: String },
    #[error("Agent router I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Agent router data is corrupt at {path}: {message}")]
    Corrupt { path: PathBuf, message: String },
    #[error("Agent router task failed: {0}")]
    Task(String),
    #[error("Agent delivery claim '{attempt_id}' is stale for message '{message_id}'")]
    StaleClaim {
        message_id: String,
        attempt_id: String,
    },
    #[error("Agent delivery supervisor is shutting down")]
    ShuttingDown,
    #[error("Agent delivery supervisor requires an active Tokio runtime: {0}")]
    RuntimeUnavailable(String),
    #[error("Agent delivery supervisor state is unavailable")]
    StateUnavailable,
    #[error("Agent group '{0}' does not exist")]
    GroupNotFound(String),
    #[error(
        "Agent inbox is retiring for workspace '{workspace_id}' conversation {conversation_id:?}"
    )]
    Retiring {
        workspace_id: String,
        conversation_id: Option<String>,
    },
    #[error(
        "Agent inbox batch '{batch_id}' was not committed after {attempts} attempt(s): {detail}"
    )]
    AppendNotCommitted {
        batch_id: String,
        attempts: usize,
        detail: String,
    },
    #[error("Agent inbox batch '{batch_id}' has an unresolved commit outcome: {detail}")]
    AppendOutcomeUnknown { batch_id: String, detail: String },
    #[error("Agent inbox batch '{batch_id}' conflicts with persisted identity: {detail}")]
    AppendIdentityConflict { batch_id: String, detail: String },
}

type FrameworkDeliveryEvent =
    echo_agent::delivery::DeliveryEvent<AgentAddress, AgentMessage>;
type FrameworkDeliveryProjection =
    echo_agent::delivery::DeliveryLedgerProjection<AgentAddress, AgentMessage>;
type FrameworkDeliveryJournal =
    echo_agent::state::journal::SegmentedFileEventJournal<FrameworkDeliveryEvent>;
type FrameworkDeliveryLedger = echo_agent::delivery::DeliveryLedger<
    FrameworkDeliveryJournal,
    AgentAddress,
    AgentMessage,
>;

struct AgentInboxAuthorityState {
    framework: AgentFrameworkState,
}

struct AgentFrameworkState {
    journal: Arc<FrameworkDeliveryJournal>,
    ledger: FrameworkDeliveryLedger,
    checkpoint_path: PathBuf,
    durability_debt: Option<String>,
}

struct AgentInboxAuthority {
    directory: PathBuf,
    checkpoint_path: PathBuf,
    expected_target: AgentAddress,
    operation: StdMutex<()>,
    state: StdMutex<Option<AgentInboxAuthorityState>>,
}

pub struct AgentRouterRetirementGuard {
    _marker: Arc<AgentRouterRetirementMarker>,
    root: PathBuf,
    inboxes: Arc<AgentInboxRegistry>,
    scope: AgentRouterRetirementScope,
}

#[derive(Clone)]
enum AgentRouterRetirementScope {
    Target(AgentAddress),
    Workspace(WorkspaceId),
}

struct AgentRouterRetirementMarker {
    registry: Arc<AgentInboxRegistry>,
    target: Option<AgentAddress>,
    workspace_id: Option<WorkspaceId>,
}

impl Drop for AgentRouterRetirementMarker {
    fn drop(&mut self) {
        let _lifecycle = self
            .registry
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(target) = self.target.take() {
            self.registry.retiring_targets.remove(&target);
        }
        if let Some(workspace_id) = self.workspace_id.take() {
            self.registry.retiring_workspaces.remove(&workspace_id);
        }
    }
}

impl AgentRouterRetirementGuard {
    pub async fn purge(&self) -> Result<(), AgentRouterError> {
        let root = self.root.clone();
        let inboxes = Arc::clone(&self.inboxes);
        let scope = self.scope.clone();
        tokio::task::spawn_blocking(move || match scope {
            AgentRouterRetirementScope::Target(target) => {
                retire_target_sync(&root, &inboxes, &target)
            }
            AgentRouterRetirementScope::Workspace(workspace_id) => {
                retire_workspace_sync(&root, &inboxes, &workspace_id)
            }
        })
        .await
        .map_err(|error| AgentRouterError::Task(error.to_string()))?
    }
}

/// File-backed durable inbox owner.
pub struct AgentRouter {
    root: PathBuf,
    inboxes: Arc<AgentInboxRegistry>,
}

#[derive(Default)]
struct AgentInboxRegistry {
    lifecycle: StdMutex<()>,
    authorities: DashMap<AgentAddress, Arc<AgentInboxAuthority>>,
    retiring_targets: DashMap<AgentAddress, ()>,
    retiring_workspaces: DashMap<WorkspaceId, ()>,
}

#[derive(Default)]
struct AgentDeliverySupervisorState {
    active: HashMap<AgentAddress, u64>,
    dirty: HashMap<AgentAddress, u64>,
    next_driver_generation: u64,
    drivers: tokio::task::JoinSet<()>,
    driver_targets: HashMap<tokio::task::Id, AgentAddress>,
    driver_failures: Vec<String>,
    retiring_targets: HashSet<AgentAddress>,
    retiring_workspaces: HashSet<WorkspaceId>,
    shutting_down: bool,
}

struct AgentDeliveryDriverGuard {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    target: AgentAddress,
    generation: u64,
    recover: Arc<dyn Fn(AgentAddress) + Send + Sync>,
}

pub(crate) struct AgentDeliveryDriverCycle {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    target: AgentAddress,
    generation: u64,
}

impl AgentDeliveryDriverCycle {
    /// Complete one drain cycle for this exact driver generation. `true` means
    /// an enqueue raced the cycle and the same owner must inspect the target
    /// again before releasing it.
    pub(crate) fn complete(&self) -> Result<bool, AgentRouterError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        if state.active.get(&self.target) != Some(&self.generation) {
            return Ok(false);
        }
        if state.dirty.get(&self.target) == Some(&self.generation) && !state.shutting_down {
            state.dirty.remove(&self.target);
            return Ok(true);
        }
        state.active.remove(&self.target);
        if state.dirty.get(&self.target) == Some(&self.generation) {
            state.dirty.remove(&self.target);
        }
        self.idle.notify_waiters();
        Ok(false)
    }
}

impl Drop for AgentDeliveryDriverGuard {
    fn drop(&mut self) {
        let mut recover = false;
        if let Ok(mut state) = self.state.lock()
            && state.active.get(&self.target) == Some(&self.generation)
        {
            state.active.remove(&self.target);
            if state.dirty.get(&self.target) == Some(&self.generation) {
                state.dirty.remove(&self.target);
                recover = !state.shutting_down;
            }
        }
        if recover {
            (self.recover)(self.target.clone());
        }
        self.idle.notify_waiters();
    }
}

pub struct AgentDeliveryRetirementGuard {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    target: AgentAddress,
}

pub struct AgentDeliveryWorkspaceRetirementGuard {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    workspace_id: WorkspaceId,
}

impl Drop for AgentDeliveryRetirementGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.retiring_targets.remove(&self.target);
        }
        self.idle.notify_waiters();
    }
}

impl Drop for AgentDeliveryWorkspaceRetirementGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.retiring_workspaces.remove(&self.workspace_id);
        }
        self.idle.notify_waiters();
    }
}

/// Application-owned lifetime manager for asynchronous inbox delivery.
/// It owns task lifetimes only; Agent execution remains in `drive_chat`.
pub struct AgentDeliverySupervisor {
    state: Arc<StdMutex<AgentDeliverySupervisorState>>,
    idle: Arc<tokio::sync::Notify>,
    cancel: echo_agent::agent::CancellationToken,
}

impl Default for AgentDeliverySupervisor {
    fn default() -> Self {
        Self {
            state: Arc::new(StdMutex::new(AgentDeliverySupervisorState::default())),
            idle: Arc::new(tokio::sync::Notify::new()),
            cancel: echo_agent::agent::CancellationToken::new(),
        }
    }
}

impl AgentDeliverySupervisor {
    pub fn cancellation_token(&self) -> echo_agent::agent::CancellationToken {
        self.cancel.clone()
    }

    pub fn has_active_workspace(&self, workspace_id: &WorkspaceId) -> bool {
        self.state
            .lock()
            .map(|state| {
                state
                    .active
                    .keys()
                    .any(|target| &target.workspace_id == workspace_id)
            })
            .unwrap_or(true)
    }

    pub fn has_active_target(&self, target: &AgentAddress) -> bool {
        self.state
            .lock()
            .map(|state| state.active.contains_key(target))
            .unwrap_or(true)
    }

    #[cfg(test)]
    fn is_retiring_target(&self, target: &AgentAddress) -> bool {
        self.state
            .lock()
            .map(|state| state.retiring_targets.contains(target))
            .unwrap_or(false)
    }

    /// Start one target-owned delivery task or mark the already-running task
    /// dirty so it performs another empty-inbox check before exit.
    pub(crate) fn supervise<Factory, Operation>(
        &self,
        target: AgentAddress,
        recover: Arc<dyn Fn(AgentAddress) + Send + Sync>,
        operation: Factory,
    ) -> Result<bool, AgentRouterError>
    where
        Factory: FnOnce(AgentDeliveryDriverCycle) -> Operation + Send + 'static,
        Operation: std::future::Future<Output = ()> + Send + 'static,
    {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| AgentRouterError::RuntimeUnavailable(error.to_string()))?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        Self::collect_finished(&mut state);
        if state.shutting_down {
            return Err(AgentRouterError::ShuttingDown);
        }
        if state.retiring_targets.contains(&target)
            || state.retiring_workspaces.contains(&target.workspace_id)
        {
            return Err(AgentRouterError::Retiring {
                workspace_id: target.workspace_id.to_string(),
                conversation_id: Some(target.conversation_id),
            });
        }
        if let Some(generation) = state.active.get(&target).copied() {
            state.dirty.insert(target, generation);
            return Ok(false);
        }
        let generation = state
            .next_driver_generation
            .checked_add(1)
            .ok_or_else(|| AgentRouterError::Task("delivery driver generation exhausted".into()))?;
        state.next_driver_generation = generation;
        state.active.insert(target.clone(), generation);
        let guard = AgentDeliveryDriverGuard {
            state: Arc::clone(&self.state),
            idle: Arc::clone(&self.idle),
            target: target.clone(),
            generation,
            recover,
        };
        let cycle = AgentDeliveryDriverCycle {
            state: Arc::clone(&self.state),
            idle: Arc::clone(&self.idle),
            target: target.clone(),
            generation,
        };
        let abort = state.drivers.spawn_on(
            async move {
                let _guard = guard;
                operation(cycle).await;
            },
            &runtime,
        );
        state.driver_targets.insert(abort.id(), target);
        Ok(true)
    }

    pub async fn retire_target(
        &self,
        target: AgentAddress,
    ) -> Result<AgentDeliveryRetirementGuard, AgentRouterError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            Self::collect_finished(&mut state);
            if state.shutting_down {
                return Err(AgentRouterError::ShuttingDown);
            }
            if !state.retiring_targets.insert(target.clone()) {
                return Err(AgentRouterError::Retiring {
                    workspace_id: target.workspace_id.to_string(),
                    conversation_id: Some(target.conversation_id),
                });
            }
        }
        let guard = AgentDeliveryRetirementGuard {
            state: Arc::clone(&self.state),
            idle: Arc::clone(&self.idle),
            target: target.clone(),
        };
        loop {
            let notified = self.idle.notified();
            let active = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?
                .active
                .contains_key(&target);
            if !active {
                return Ok(guard);
            }
            notified.await;
        }
    }

    pub async fn retire_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<AgentDeliveryWorkspaceRetirementGuard, AgentRouterError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            Self::collect_finished(&mut state);
            if state.shutting_down {
                return Err(AgentRouterError::ShuttingDown);
            }
            if !state.retiring_workspaces.insert(workspace_id.clone())
                || state
                    .retiring_targets
                    .iter()
                    .any(|target| target.workspace_id == workspace_id)
            {
                state.retiring_workspaces.remove(&workspace_id);
                return Err(AgentRouterError::Retiring {
                    workspace_id: workspace_id.to_string(),
                    conversation_id: None,
                });
            }
        }
        let guard = AgentDeliveryWorkspaceRetirementGuard {
            state: Arc::clone(&self.state),
            idle: Arc::clone(&self.idle),
            workspace_id: workspace_id.clone(),
        };
        loop {
            let notified = self.idle.notified();
            let active = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?
                .active
                .keys()
                .any(|target| target.workspace_id == workspace_id);
            if !active {
                return Ok(guard);
            }
            notified.await;
        }
    }

    fn collect_finished(state: &mut AgentDeliverySupervisorState) {
        while let Some(result) = state.drivers.try_join_next_with_id() {
            match result {
                Ok((driver_id, ())) => {
                    state.driver_targets.remove(&driver_id);
                }
                Err(error) => {
                    let target = state.driver_targets.remove(&error.id());
                    let failure = target.map_or_else(
                        || format!("Agent delivery task failed to join: {error}"),
                        |target| {
                            format!("Agent delivery task for {target:?} failed to join: {error}")
                        },
                    );
                    tracing::error!(error = %failure, "Agent delivery task failed to join");
                    state.driver_failures.push(failure);
                }
            }
        }
    }

    /// Permanently close delivery admission and broadcast cancellation without
    /// waiting for any driver. Application shutdown calls this in its first
    /// phase so dependent foreground owners can observe cancellation before any
    /// lifecycle join begins.
    pub fn close_admission_and_cancel(&self) -> Result<(), AgentRouterError> {
        self.cancel.cancel();
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        state.shutting_down = true;
        state.dirty.clear();
        state.retiring_targets.clear();
        state.retiring_workspaces.clear();
        Ok(())
    }

    /// Join every delivery driver accepted before admission closed.
    pub async fn join(&self) -> Result<(), AgentRouterError> {
        let (mut drivers, mut driver_targets, mut failures) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AgentRouterError::StateUnavailable)?;
            state.shutting_down = true;
            state.active.clear();
            state.dirty.clear();
            state.retiring_targets.clear();
            state.retiring_workspaces.clear();
            (
                std::mem::take(&mut state.drivers),
                std::mem::take(&mut state.driver_targets),
                std::mem::take(&mut state.driver_failures),
            )
        };
        while let Some(result) = drivers.join_next_with_id().await {
            match result {
                Ok((driver_id, ())) => {
                    driver_targets.remove(&driver_id);
                }
                Err(error) => {
                    let target = driver_targets.remove(&error.id());
                    failures.push(target.map_or_else(
                        || format!("Agent delivery task failed to join: {error}"),
                        |target| {
                            format!("Agent delivery task for {target:?} failed to join: {error}")
                        },
                    ));
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AgentRouterError::Task(failures.join("; ")))
        }
    }

    pub async fn shutdown(&self) -> Result<(), AgentRouterError> {
        self.close_admission_and_cancel()?;
        self.join().await
    }
}
