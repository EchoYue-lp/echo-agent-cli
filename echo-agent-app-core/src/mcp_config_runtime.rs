//! Canonical MCP configuration source and user-config connection reconciliation.
//!
//! EKO owns one durable `mcp.json` source per process. The generic framework
//! remains responsible for parsing entries and connecting clients; this module
//! owns the application policy around source selection, atomic commits, and
//! keeping user-configured servers separate from plugin-owned servers.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use echo_agent::config::AppConfig;
use echo_agent::mcp::{McpConfigFile, McpServerEntry};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};
use tokio_util::sync::CancellationToken;

use crate::agent_handle::AgentHandle;

const MCP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const REDACTED_VALUE: &str = "<redacted>";

#[derive(Debug, thiserror::Error)]
pub enum McpConfigRuntimeError {
    #[error("failed to read MCP config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid MCP config {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("MCP config {path} contains an invalid server entry: {message}")]
    InvalidExisting { path: PathBuf, message: String },
    #[error("invalid MCP config: {0}")]
    Validation(String),
    #[error("failed to serialize MCP config: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to persist MCP config {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("MCP config writer task failed: {0}")]
    WriterTask(String),
    #[error("MCP config mutation settlement task failed: {0}")]
    MutationTask(String),
    #[error("MCP server '{0}' is not present in the user config")]
    ServerNotFound(String),
    #[error("MCP configuration runtime is shutting down")]
    Closed,
}

/// Resolve the one writable MCP source for this process.
///
/// Precedence is CLI override, application YAML, environment, then EKO's
/// user-data `mcp.json`. The default path is returned even before the file
/// exists so the first GUI mutation has an unambiguous durable destination.
pub fn resolve_mcp_config_path(cli_override: Option<&str>, app_config: &AppConfig) -> PathBuf {
    let env_override = std::env::var("MCP_CONFIG_PATH").ok();
    resolve_mcp_config_path_sources(
        cli_override,
        app_config.mcp.config_path.as_deref(),
        env_override.as_deref(),
    )
}

fn resolve_mcp_config_path_sources(
    cli_override: Option<&str>,
    yaml_config: Option<&str>,
    env_override: Option<&str>,
) -> PathBuf {
    cli_override
        .map(PathBuf::from)
        .or_else(|| yaml_config.map(PathBuf::from))
        .or_else(|| env_override.map(PathBuf::from))
        .unwrap_or_else(|| echo_agent::paths::user_data_path("mcp.json"))
}

/// Parse one configuration snapshot. A missing file is an empty initial
/// snapshot; malformed or unreadable existing files remain visible as errors.
pub fn load_mcp_config_snapshot(path: &Path) -> Result<McpConfigFile, McpConfigRuntimeError> {
    load_mcp_config_snapshot_inner(path, true)
}

/// Parse a configuration being explicitly imported by the user. Unlike the
/// bootstrap loader, an absent source is an error and cannot clear the durable
/// canonical configuration.
pub fn load_existing_mcp_config_snapshot(
    path: &Path,
) -> Result<McpConfigFile, McpConfigRuntimeError> {
    load_mcp_config_snapshot_inner(path, false)
}

fn load_mcp_config_snapshot_inner(
    path: &Path,
    missing_is_empty: bool,
) -> Result<McpConfigFile, McpConfigRuntimeError> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if missing_is_empty && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(McpConfigFile::default());
        }
        Err(source) => {
            return Err(McpConfigRuntimeError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let config = McpConfigFile::parse(&content).map_err(|error| McpConfigRuntimeError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    validate_mcp_config(&config).map_err(|error| McpConfigRuntimeError::InvalidExisting {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    Ok(config)
}

struct McpConfigCommit {
    generation: u64,
    previous: McpConfigFile,
    current: McpConfigFile,
    cancel: CancellationToken,
}

struct ReconcileTask {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

struct McpRuntimeSupervisor {
    accepting_mutations: bool,
    mutation_tasks: Vec<tokio::task::JoinHandle<()>>,
    reconcile_tasks: Vec<ReconcileTask>,
}

impl Default for McpRuntimeSupervisor {
    fn default() -> Self {
        Self {
            accepting_mutations: true,
            mutation_tasks: Vec::new(),
            reconcile_tasks: Vec::new(),
        }
    }
}

#[cfg(test)]
struct WriterTestGate {
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PluginMcpOwnershipToken(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
enum McpNameOwner {
    User,
    Plugin {
        plugin_id: String,
        token: PluginMcpOwnershipToken,
    },
}

#[derive(Default)]
struct McpNameOwnershipState {
    owners: BTreeMap<String, McpNameOwner>,
    next_token: u64,
}

/// Application-layer namespace authority shared by durable user MCP config and
/// plugin receipts. It does not own clients; it only prevents a stale plugin
/// receipt from disconnecting a name that user configuration has taken over.
pub(crate) struct McpNameOwnershipRegistry {
    state: Arc<Mutex<McpNameOwnershipState>>,
}

impl McpNameOwnershipRegistry {
    pub(crate) fn new(user_names: impl IntoIterator<Item = String>) -> Arc<Self> {
        let owners = user_names
            .into_iter()
            .map(|name| (name, McpNameOwner::User))
            .collect();
        Arc::new(Self {
            state: Arc::new(Mutex::new(McpNameOwnershipState {
                owners,
                next_token: 0,
            })),
        })
    }

    pub(crate) async fn lock(self: &Arc<Self>) -> McpNameOwnershipGuard {
        McpNameOwnershipGuard {
            state: Arc::clone(&self.state).lock_owned().await,
        }
    }

    #[cfg(test)]
    pub(crate) async fn claim_user_names(&self, names: impl IntoIterator<Item = String>) {
        let mut state = self.state.lock().await;
        for name in names {
            state.owners.insert(name, McpNameOwner::User);
        }
    }

    async fn settle_user_names(&self, current_names: &BTreeSet<String>) {
        let mut state = self.state.lock().await;
        state.owners.retain(|name, owner| {
            !matches!(owner, McpNameOwner::User) || current_names.contains(name)
        });
        for name in current_names {
            state.owners.insert(name.clone(), McpNameOwner::User);
        }
    }

    #[cfg(test)]
    async fn owner(&self, name: &str) -> Option<McpNameOwner> {
        self.state.lock().await.owners.get(name).cloned()
    }
}

pub(crate) struct McpNameOwnershipGuard {
    state: OwnedMutexGuard<McpNameOwnershipState>,
}

impl McpNameOwnershipGuard {
    pub(crate) fn claim_user_names(&mut self, names: impl IntoIterator<Item = String>) {
        for name in names {
            self.state.owners.insert(name, McpNameOwner::User);
        }
    }

    pub(crate) fn validate_plugin_claim(
        &self,
        plugin_id: &str,
        name: &str,
        previous_token: Option<PluginMcpOwnershipToken>,
    ) -> Result<(), String> {
        match self.state.owners.get(name) {
            None => Ok(()),
            Some(McpNameOwner::Plugin {
                plugin_id: owner,
                token,
            }) if owner == plugin_id && Some(*token) == previous_token => Ok(()),
            Some(McpNameOwner::User) => Err(format!(
                "Plugin '{plugin_id}' MCP server name '{name}' is owned by user configuration"
            )),
            Some(McpNameOwner::Plugin {
                plugin_id: owner, ..
            }) => Err(format!(
                "Plugin '{plugin_id}' MCP server name '{name}' is owned by plugin '{owner}'"
            )),
        }
    }

    pub(crate) fn claim_plugin(
        &mut self,
        plugin_id: &str,
        name: &str,
    ) -> Result<PluginMcpOwnershipToken, String> {
        if let Some(owner) = self.state.owners.get(name) {
            let owner = match owner {
                McpNameOwner::User => "user configuration".to_string(),
                McpNameOwner::Plugin { plugin_id, .. } => format!("plugin '{plugin_id}'"),
            };
            return Err(format!(
                "Plugin '{plugin_id}' MCP server name '{name}' is owned by {owner}"
            ));
        }
        self.state.next_token = self.state.next_token.saturating_add(1);
        let token = PluginMcpOwnershipToken(self.state.next_token);
        self.state.owners.insert(
            name.to_string(),
            McpNameOwner::Plugin {
                plugin_id: plugin_id.to_string(),
                token,
            },
        );
        Ok(token)
    }

    pub(crate) fn owns_plugin(
        &self,
        plugin_id: &str,
        name: &str,
        token: PluginMcpOwnershipToken,
    ) -> bool {
        matches!(
            self.state.owners.get(name),
            Some(McpNameOwner::Plugin {
                plugin_id: owner,
                token: owner_token,
            }) if owner == plugin_id && *owner_token == token
        )
    }

    pub(crate) fn release_plugin(
        &mut self,
        plugin_id: &str,
        name: &str,
        token: PluginMcpOwnershipToken,
    ) {
        if self.owns_plugin(plugin_id, name, token) {
            self.state.owners.remove(name);
        }
    }
}

#[derive(Debug)]
struct ReconcilePlan {
    disconnect: Vec<String>,
    connect: Vec<(String, McpServerEntry)>,
}

impl ReconcilePlan {
    #[cfg(test)]
    fn between(previous: &McpConfigFile, current: &McpConfigFile) -> Self {
        Self::with_disconnect_names(previous.mcp_servers.keys().cloned(), current)
    }

    fn with_disconnect_names(
        disconnect_names: impl IntoIterator<Item = String>,
        current: &McpConfigFile,
    ) -> Self {
        // A canceled older generation may not have applied every disconnection
        // or connection. Rebuild the tracked user set so the newest
        // generation converges from any partial predecessor state. Names that
        // exist only in the plugin registry are outside this set.
        let mut disconnect = disconnect_names.into_iter().collect::<Vec<_>>();
        disconnect.sort();
        disconnect.dedup();

        let mut connect = current
            .mcp_servers
            .iter()
            .filter(|(_, current_entry)| !current_entry.disabled)
            .map(|(name, entry)| (name.clone(), entry.clone()))
            .collect::<Vec<_>>();
        connect.sort_by(|left, right| left.0.cmp(&right.0));

        Self {
            disconnect,
            connect,
        }
    }
}

/// Application-owned MCP config authority.
pub struct McpConfigRuntime {
    path: PathBuf,
    snapshot: RwLock<McpConfigFile>,
    mutation_lock: Mutex<()>,
    generation: AtomicU64,
    supervisor: Mutex<McpRuntimeSupervisor>,
    unreconciled_user_names: Mutex<BTreeSet<String>>,
    ownership: Arc<McpNameOwnershipRegistry>,
    shutdown: CancellationToken,
    #[cfg(test)]
    writer_gate: Mutex<Option<WriterTestGate>>,
}

impl McpConfigRuntime {
    pub(crate) fn new(path: PathBuf, snapshot: McpConfigFile) -> Self {
        let unreconciled_user_names = snapshot.mcp_servers.keys().cloned().collect();
        let ownership = McpNameOwnershipRegistry::new(snapshot.mcp_servers.keys().cloned());
        Self {
            path,
            snapshot: RwLock::new(snapshot),
            mutation_lock: Mutex::new(()),
            generation: AtomicU64::new(0),
            supervisor: Mutex::new(McpRuntimeSupervisor::default()),
            unreconciled_user_names: Mutex::new(unreconciled_user_names),
            ownership,
            shutdown: CancellationToken::new(),
            #[cfg(test)]
            writer_gate: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn empty(path: PathBuf) -> Self {
        Self::new(path, McpConfigFile::default())
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }

    pub async fn snapshot(&self) -> McpConfigFile {
        self.snapshot.read().await.clone()
    }

    pub(crate) fn ownership(&self) -> Arc<McpNameOwnershipRegistry> {
        Arc::clone(&self.ownership)
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    async fn run_owned_mutation<T, F, Fut>(
        self: &Arc<Self>,
        operation: F,
    ) -> Result<T, McpConfigRuntimeError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Self>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, McpConfigRuntimeError>> + Send + 'static,
    {
        let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
        let mut supervisor = self.supervisor.lock().await;
        if !supervisor.accepting_mutations || self.shutdown.is_cancelled() {
            return Err(McpConfigRuntimeError::Closed);
        }
        let mut completed = Vec::new();
        let mut pending = Vec::new();
        for task in std::mem::take(&mut supervisor.mutation_tasks) {
            if task.is_finished() {
                completed.push(task);
            } else {
                pending.push(task);
            }
        }
        let runtime = Arc::clone(self);
        pending.push(tokio::spawn(async move {
            let result = operation(runtime).await;
            let _ = result_sender.send(result);
        }));
        supervisor.mutation_tasks = pending;
        drop(supervisor);
        Self::await_mutation_tasks(completed, "replacement").await;
        result_receiver
            .await
            .map_err(|error| McpConfigRuntimeError::MutationTask(error.to_string()))?
    }

    pub async fn replace_and_reconcile(
        self: &Arc<Self>,
        agent: AgentHandle,
        candidate: McpConfigFile,
    ) -> Result<u64, McpConfigRuntimeError> {
        self.run_owned_mutation(move |runtime| async move {
            runtime.replace_and_reconcile_inner(agent, candidate).await
        })
        .await
    }

    async fn replace_and_reconcile_inner(
        self: &Arc<Self>,
        agent: AgentHandle,
        candidate: McpConfigFile,
    ) -> Result<u64, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.ensure_open()?;
        let commit = self.commit_candidate_locked(candidate).await?;
        let generation = commit.generation;
        self.start_reconcile(agent, commit).await;
        Ok(generation)
    }

    pub async fn upsert_and_reconcile(
        self: &Arc<Self>,
        agent: AgentHandle,
        name: String,
        entry: McpServerEntry,
    ) -> Result<u64, McpConfigRuntimeError> {
        self.run_owned_mutation(move |runtime| async move {
            runtime.upsert_and_reconcile_inner(agent, name, entry).await
        })
        .await
    }

    async fn upsert_and_reconcile_inner(
        self: &Arc<Self>,
        agent: AgentHandle,
        name: String,
        entry: McpServerEntry,
    ) -> Result<u64, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.ensure_open()?;
        let commit = self.commit_upsert_locked(name, entry).await?;
        let generation = commit.generation;
        self.start_reconcile(agent, commit).await;
        Ok(generation)
    }

    pub async fn set_enabled_and_reconcile(
        self: &Arc<Self>,
        agent: AgentHandle,
        name: &str,
        enabled: bool,
    ) -> Result<u64, McpConfigRuntimeError> {
        let name = name.to_string();
        self.run_owned_mutation(move |runtime| async move {
            runtime
                .set_enabled_and_reconcile_inner(agent, &name, enabled)
                .await
        })
        .await
    }

    async fn set_enabled_and_reconcile_inner(
        self: &Arc<Self>,
        agent: AgentHandle,
        name: &str,
        enabled: bool,
    ) -> Result<u64, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.ensure_open()?;
        let commit = self.commit_toggle_locked(name, enabled).await?;
        let generation = commit.generation;
        self.start_reconcile(agent, commit).await;
        Ok(generation)
    }

    pub async fn remove_and_reconcile(
        self: &Arc<Self>,
        agent: AgentHandle,
        name: &str,
    ) -> Result<u64, McpConfigRuntimeError> {
        let name = name.to_string();
        self.run_owned_mutation(move |runtime| async move {
            runtime.remove_and_reconcile_inner(agent, &name).await
        })
        .await
    }

    async fn remove_and_reconcile_inner(
        self: &Arc<Self>,
        agent: AgentHandle,
        name: &str,
    ) -> Result<u64, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.ensure_open()?;
        let commit = self.commit_remove_locked(name).await?;
        let generation = commit.generation;
        self.start_reconcile(agent, commit).await;
        Ok(generation)
    }

    async fn commit_upsert_locked(
        &self,
        name: String,
        entry: McpServerEntry,
    ) -> Result<McpConfigCommit, McpConfigRuntimeError> {
        let mut candidate = self.snapshot().await;
        candidate.mcp_servers.insert(name, entry);
        self.commit_candidate_locked(candidate).await
    }

    async fn commit_toggle_locked(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<McpConfigCommit, McpConfigRuntimeError> {
        let mut candidate = self.snapshot().await;
        let entry = candidate
            .mcp_servers
            .get_mut(name)
            .ok_or_else(|| McpConfigRuntimeError::ServerNotFound(name.to_string()))?;
        entry.disabled = !enabled;
        self.commit_candidate_locked(candidate).await
    }

    async fn commit_remove_locked(
        &self,
        name: &str,
    ) -> Result<McpConfigCommit, McpConfigRuntimeError> {
        let mut candidate = self.snapshot().await;
        if candidate.mcp_servers.remove(name).is_none() {
            return Err(McpConfigRuntimeError::ServerNotFound(name.to_string()));
        }
        self.commit_candidate_locked(candidate).await
    }

    async fn commit_candidate_locked(
        &self,
        mut candidate: McpConfigFile,
    ) -> Result<McpConfigCommit, McpConfigRuntimeError> {
        let mut snapshot = self.snapshot.write().await;
        let previous = snapshot.clone();
        restore_redacted_values(&mut candidate, &previous);
        validate_mcp_config(&candidate)?;
        let bytes = serde_json::to_vec_pretty(&candidate)?;
        let path = self.path.clone();
        let write_path = path.clone();

        // Reserve the shared namespace before the durable write starts. The
        // snapshot and ownership guards stay held through writer settlement,
        // so a successful write is promoted without another cancellation
        // point or a plugin takeover window.
        let mut ownership = self.ownership.lock().await;
        #[cfg(test)]
        let writer_gate = self.writer_gate.lock().await.take();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(gate) = writer_gate {
                gate.started.send(()).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "MCP writer test observer was dropped",
                    )
                })?;
                gate.release.recv().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "MCP writer test release was dropped",
                    )
                })?;
            }
            echo_core::utils::fs::atomic_write(&write_path, &bytes)
        })
        .await
        .map_err(|error| McpConfigRuntimeError::WriterTask(error.to_string()))?
        .map_err(|source| McpConfigRuntimeError::Write { path, source })?;

        // The durable user source wins the shared ReactAgent namespace. Claim
        // before reconcile so a plugin cannot wire the same name between the
        // successful commit and the user connection replacement.
        ownership.claim_user_names(candidate.mcp_servers.keys().cloned());
        *snapshot = candidate.clone();
        let previous_generation = self
            .generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_add(1))
            })
            .unwrap_or_else(|current| current);
        let generation = previous_generation.saturating_add(1);

        Ok(McpConfigCommit {
            generation,
            previous,
            current: candidate,
            cancel: CancellationToken::new(),
        })
    }

    async fn start_reconcile(self: &Arc<Self>, agent: AgentHandle, commit: McpConfigCommit) {
        let plan = {
            let mut names = self.unreconciled_user_names.lock().await;
            names.extend(commit.previous.mcp_servers.keys().cloned());
            names.extend(commit.current.mcp_servers.keys().cloned());
            ReconcilePlan::with_disconnect_names(names.iter().cloned(), &commit.current)
        };
        let cancel = commit.cancel.clone();
        let runtime = Arc::clone(self);
        self.start_tracked_reconcile(cancel, async move {
            runtime.reconcile(agent, commit, plan).await
        })
        .await;
    }

    async fn start_tracked_reconcile<F>(&self, cancel: CancellationToken, reconcile: F) -> usize
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let completed = {
            let mut supervisor = self.supervisor.lock().await;
            if self.shutdown.is_cancelled() {
                cancel.cancel();
                return 0;
            }

            let mut completed = Vec::new();
            let mut pending = Vec::new();
            for task in std::mem::take(&mut supervisor.reconcile_tasks) {
                task.cancel.cancel();
                if task.handle.is_finished() {
                    completed.push(task);
                } else {
                    pending.push(task);
                }
            }
            let handle = tokio::spawn(reconcile);
            pending.push(ReconcileTask { cancel, handle });
            supervisor.reconcile_tasks = pending;
            completed
        };
        Self::await_reconcile_tasks(completed, "replacement").await
    }

    /// Cancel and await every connection reconciliation started by this owner.
    /// Calling this more than once is safe.
    pub async fn shutdown(&self) {
        let mutations = {
            let mut supervisor = self.supervisor.lock().await;
            supervisor.accepting_mutations = false;
            std::mem::take(&mut supervisor.mutation_tasks)
        };
        Self::await_mutation_tasks(mutations, "shutdown").await;

        // Every accepted mutation has now durably settled and handed its
        // connection work to the same supervisor. Close the runtime only after
        // that handoff, then cancel and drain all reconciliation receipts.
        self.shutdown.cancel();
        let tasks = {
            let mut supervisor = self.supervisor.lock().await;
            for task in &supervisor.reconcile_tasks {
                task.cancel.cancel();
            }
            std::mem::take(&mut supervisor.reconcile_tasks)
        };
        Self::await_reconcile_tasks(tasks, "shutdown").await;
    }

    async fn await_mutation_tasks(
        tasks: Vec<tokio::task::JoinHandle<()>>,
        phase: &'static str,
    ) -> usize {
        let mut failures = 0usize;
        for task in tasks {
            if let Err(error) = task.await {
                failures = failures.saturating_add(1);
                tracing::warn!(%error, phase, "MCP mutation settlement task failed");
            }
        }
        failures
    }

    async fn await_reconcile_tasks(tasks: Vec<ReconcileTask>, phase: &'static str) -> usize {
        let mut failures = 0usize;
        for task in tasks {
            if let Err(error) = task.handle.await {
                failures = failures.saturating_add(1);
                tracing::warn!(%error, phase, "MCP reconcile task failed");
            }
        }
        failures
    }

    fn ensure_open(&self) -> Result<(), McpConfigRuntimeError> {
        if self.shutdown.is_cancelled() {
            Err(McpConfigRuntimeError::Closed)
        } else {
            Ok(())
        }
    }

    async fn reconcile(
        self: Arc<Self>,
        agent: AgentHandle,
        commit: McpConfigCommit,
        plan: ReconcilePlan,
    ) {
        if commit.cancel.is_cancelled() || self.generation() != commit.generation {
            return;
        }
        let current_names = commit
            .current
            .mcp_servers
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let generation = commit.generation;
        let runtime = Arc::clone(&self);
        let wait_cancel = commit.cancel.clone();
        let reconcile_cancel = commit.cancel.clone();
        let reconcile = agent.write_async(|agent| {
                Box::pin(async move {
                    for name in plan.disconnect {
                        if reconcile_cancel.is_cancelled()
                            || runtime.generation() != generation
                        {
                            return false;
                        }
                        agent.disconnect_mcp(&name).await;
                    }

                    for (name, entry) in plan.connect {
                        if reconcile_cancel.is_cancelled()
                            || runtime.generation() != generation
                        {
                            return false;
                        }
                        let server_config = match entry.to_server_config(&name) {
                            Ok(config) => config,
                            Err(error) => {
                                tracing::warn!(server = %name, %error, "MCP config became invalid before reconcile");
                                continue;
                            }
                        };
                        let connect = agent.connect_mcp_from_config(server_config);
                        tokio::select! {
                            _ = reconcile_cancel.cancelled() => return false,
                            result = tokio::time::timeout(MCP_CONNECT_TIMEOUT, connect) => {
                                match result {
                                    Ok(Ok(_)) => tracing::info!(server = %name, generation, "MCP user server reconciled"),
                                    Ok(Err(error)) => tracing::warn!(server = %name, %error, "MCP user server connection failed"),
                                    Err(_) => tracing::warn!(server = %name, timeout_secs = MCP_CONNECT_TIMEOUT.as_secs(), "MCP user server connection timed out"),
                                }
                            }
                        }
                    }
                    true
                })
            });
        let converged = tokio::select! {
            biased;
            _ = wait_cancel.cancelled() => false,
            converged = reconcile => converged,
        };
        if converged && self.generation() == generation {
            let mut names = self.unreconciled_user_names.lock().await;
            if self.generation() == generation {
                *names = current_names;
                self.ownership.settle_user_names(&names).await;
            }
        }
    }

    #[cfg(test)]
    async fn commit_candidate(
        &self,
        candidate: McpConfigFile,
    ) -> Result<McpConfigCommit, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.commit_candidate_locked(candidate).await
    }

    #[cfg(test)]
    async fn commit_candidate_owned(
        self: &Arc<Self>,
        candidate: McpConfigFile,
    ) -> Result<u64, McpConfigRuntimeError> {
        self.run_owned_mutation(move |runtime| async move {
            let _mutation_guard = runtime.mutation_lock.lock().await;
            runtime.ensure_open()?;
            let commit = runtime.commit_candidate_locked(candidate).await?;
            let generation = commit.generation;
            runtime
                .start_tracked_reconcile(commit.cancel, async {})
                .await;
            Ok(generation)
        })
        .await
    }

    #[cfg(test)]
    async fn commit_upsert(
        &self,
        name: String,
        entry: McpServerEntry,
    ) -> Result<McpConfigCommit, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.commit_upsert_locked(name, entry).await
    }

    #[cfg(test)]
    async fn commit_toggle(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<McpConfigCommit, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.commit_toggle_locked(name, enabled).await
    }

    #[cfg(test)]
    async fn commit_remove(&self, name: &str) -> Result<McpConfigCommit, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.commit_remove_locked(name).await
    }
}

/// The JSON editor receives a credential-redacted snapshot. Treat those exact
/// markers as "keep the existing local value" so an unrelated edit cannot
/// replace a real token with the display placeholder on disk.
fn restore_redacted_values(candidate: &mut McpConfigFile, previous: &McpConfigFile) {
    for (name, candidate_entry) in &mut candidate.mcp_servers {
        let Some(previous_entry) = previous.mcp_servers.get(name) else {
            continue;
        };
        for (key, value) in &mut candidate_entry.env {
            if value == REDACTED_VALUE
                && let Some(previous_value) = previous_entry.env.get(key)
            {
                *value = previous_value.clone();
            }
        }
        for (key, value) in &mut candidate_entry.headers {
            if value.contains(REDACTED_VALUE)
                && let Some(previous_value) = previous_entry.headers.get(key)
            {
                *value = previous_value.clone();
            }
        }
        if candidate_entry
            .url
            .as_deref()
            .is_some_and(|url| url.contains(REDACTED_VALUE))
        {
            candidate_entry.url = previous_entry.url.clone();
        }
    }
}

fn validate_mcp_config(config: &McpConfigFile) -> Result<(), McpConfigRuntimeError> {
    for (name, entry) in &config.mcp_servers {
        if name.trim().is_empty() {
            return Err(McpConfigRuntimeError::Validation(
                "server name must not be empty".to_string(),
            ));
        }
        let mut enabled_entry = entry.clone();
        enabled_entry.disabled = false;
        enabled_entry.to_server_config(name).map_err(|error| {
            McpConfigRuntimeError::Validation(format!("server '{name}': {error}"))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio_entry(command: &str, disabled: bool) -> McpServerEntry {
        McpServerEntry {
            command: Some(command.to_string()),
            disabled,
            ..Default::default()
        }
    }

    fn config_with(name: &str, entry: McpServerEntry) -> McpConfigFile {
        let mut config = McpConfigFile::default();
        config.mcp_servers.insert(name.to_string(), entry);
        config
    }

    fn configs_equal(left: &McpConfigFile, right: &McpConfigFile) -> anyhow::Result<bool> {
        Ok(serde_json::to_value(left)? == serde_json::to_value(right)?)
    }

    #[tokio::test]
    async fn user_removal_releases_name_only_after_reconcile_settles() -> anyhow::Result<()> {
        let ownership = McpNameOwnershipRegistry::new(["shared".to_string()]);
        {
            let mut guard = ownership.lock().await;
            assert!(guard.claim_plugin("fixture", "shared").is_err());
        }

        ownership.settle_user_names(&BTreeSet::new()).await;
        let mut guard = ownership.lock().await;
        assert!(guard.claim_plugin("fixture", "shared").is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn save_survives_runtime_restart() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("mcp.json");
        let runtime = McpConfigRuntime::empty(path.clone());
        let candidate = config_with("local", stdio_entry("node", false));

        let commit = runtime.commit_candidate(candidate.clone()).await?;
        assert_eq!(commit.generation, 1);
        let reloaded = load_mcp_config_snapshot(&path)?;
        let restarted = McpConfigRuntime::new(path, reloaded);
        assert!(configs_equal(&candidate, &restarted.snapshot().await)?);
        Ok(())
    }

    #[tokio::test]
    async fn failed_write_preserves_snapshot_and_generation() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let blocked_parent = temp.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"file")?;
        let runtime = McpConfigRuntime::empty(blocked_parent.join("mcp.json"));

        let result = runtime
            .commit_candidate(config_with("local", stdio_entry("node", false)))
            .await;
        assert!(result.is_err());
        assert!(runtime.snapshot().await.mcp_servers.is_empty());
        assert_eq!(runtime.generation(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn aborted_waiter_after_writer_start_still_settles_owned_commit() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("mcp.json");
        let runtime = Arc::new(McpConfigRuntime::empty(path.clone()));
        let candidate = config_with("accepted", stdio_entry("node", false));
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        *runtime.writer_gate.lock().await = Some(WriterTestGate {
            started: started_sender,
            release: release_receiver,
        });

        let runtime_for_waiter = Arc::clone(&runtime);
        let candidate_for_waiter = candidate.clone();
        let waiter = tokio::spawn(async move {
            runtime_for_waiter
                .commit_candidate_owned(candidate_for_waiter)
                .await
        });
        tokio::task::spawn_blocking(move || {
            started_receiver.recv_timeout(std::time::Duration::from_secs(2))
        })
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))??;
        waiter.abort();
        let waiter_error = waiter
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("aborted MCP waiter unexpectedly completed"))?;
        assert!(waiter_error.is_cancelled());

        release_sender.send(())?;
        tokio::time::timeout(std::time::Duration::from_secs(2), runtime.shutdown()).await?;

        let durable = load_mcp_config_snapshot(&path)?;
        assert!(configs_equal(&candidate, &durable)?);
        assert!(configs_equal(&candidate, &runtime.snapshot().await)?);
        assert_eq!(runtime.generation(), 1);
        assert_eq!(
            runtime.ownership.owner("accepted").await,
            Some(McpNameOwner::User)
        );
        let supervisor = runtime.supervisor.lock().await;
        assert!(supervisor.mutation_tasks.is_empty());
        assert!(supervisor.reconcile_tasks.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn toggle_is_persisted() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("mcp.json");
        let initial = config_with("local", stdio_entry("node", false));
        let runtime = McpConfigRuntime::new(path.clone(), initial);

        runtime.commit_toggle("local", false).await?;
        let reloaded = load_mcp_config_snapshot(&path)?;
        let disabled = reloaded
            .mcp_servers
            .get("local")
            .map(|entry| entry.disabled)
            .unwrap_or(false);
        assert!(disabled);
        Ok(())
    }

    #[tokio::test]
    async fn removal_is_persisted_and_disconnects_only_the_removed_user_server()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("mcp.json");
        let initial = config_with("local", stdio_entry("node", false));
        let runtime = McpConfigRuntime::new(path.clone(), initial);

        let commit = runtime.commit_remove("local").await?;
        let plan = ReconcilePlan::between(&commit.previous, &commit.current);
        assert_eq!(plan.disconnect, vec!["local".to_string()]);
        assert!(plan.connect.is_empty());
        let reloaded = load_mcp_config_snapshot(&path)?;
        assert!(reloaded.mcp_servers.is_empty());
        Ok(())
    }

    #[test]
    fn reconcile_plan_never_disconnects_plugin_owned_names() {
        let previous = config_with("user-old", stdio_entry("node", false));
        let current = config_with("user-new", stdio_entry("node", false));
        let plan = ReconcilePlan::between(&previous, &current);

        assert_eq!(plan.disconnect, vec!["user-old".to_string()]);
        assert_eq!(
            plan.connect
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["user-new"]
        );
        assert!(!plan.disconnect.contains(&"plugin-owned".to_string()));
    }

    #[test]
    fn reconcile_plan_replaces_changed_enabled_entry() {
        let previous = config_with("user-server", stdio_entry("node", false));
        let current = config_with("user-server", stdio_entry("python3", false));
        let plan = ReconcilePlan::between(&previous, &current);

        assert_eq!(plan.disconnect, vec!["user-server".to_string()]);
        assert_eq!(plan.connect.len(), 1);
        assert_eq!(
            plan.connect.first().map(|(name, _)| name.as_str()),
            Some("user-server")
        );
        assert_eq!(
            plan.connect
                .first()
                .and_then(|(_, entry)| entry.command.as_deref()),
            Some("python3")
        );
    }

    #[test]
    fn reconcile_plan_rehydrates_unchanged_entry() {
        let previous = config_with("user-server", stdio_entry("node", false));
        let current = previous.clone();
        let plan = ReconcilePlan::between(&previous, &current);

        assert_eq!(plan.disconnect, vec!["user-server".to_string()]);
        assert_eq!(plan.connect.len(), 1);
        assert_eq!(
            plan.connect.first().map(|(name, _)| name.as_str()),
            Some("user-server")
        );
    }

    #[test]
    fn reconcile_plan_disconnects_names_carried_from_a_canceled_generation() {
        let current = McpConfigFile::default();
        let plan = ReconcilePlan::with_disconnect_names(
            ["removed-before-reconcile".to_string()],
            &current,
        );

        assert_eq!(
            plan.disconnect,
            vec!["removed-before-reconcile".to_string()]
        );
        assert!(plan.connect.is_empty());
    }

    #[tokio::test]
    async fn concurrent_upserts_preserve_every_server() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let runtime = Arc::new(McpConfigRuntime::empty(temp.path().join("mcp.json")));
        let mut handles = Vec::new();
        for index in 0..16 {
            let runtime = Arc::clone(&runtime);
            handles.push(tokio::spawn(async move {
                runtime
                    .commit_upsert(format!("server-{index}"), stdio_entry("node", false))
                    .await
                    .map(|_| ())
            }));
        }
        for handle in handles {
            handle.await??;
        }

        let snapshot = runtime.snapshot().await;
        assert_eq!(snapshot.mcp_servers.len(), 16);
        assert_eq!(runtime.generation(), 16);
        let reloaded = load_mcp_config_snapshot(runtime.path())?;
        assert!(configs_equal(&snapshot, &reloaded)?);
        Ok(())
    }

    #[tokio::test]
    async fn replacement_awaits_completed_reconcile_handle() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let runtime = McpConfigRuntime::empty(temp.path().join("mcp.json"));
        let first_cancel = CancellationToken::new();
        let first_task_cancel = first_cancel.clone();
        runtime
            .start_tracked_reconcile(first_cancel, async move {
                first_task_cancel.cancelled().await;
            })
            .await;
        {
            let supervisor = runtime.supervisor.lock().await;
            if let Some(task) = supervisor.reconcile_tasks.first() {
                task.handle.abort();
            }
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let finished = runtime
                    .supervisor
                    .lock()
                    .await
                    .reconcile_tasks
                    .first()
                    .is_some_and(|task| task.handle.is_finished());
                if finished {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;

        let second_cancel = CancellationToken::new();
        let second_task_cancel = second_cancel.clone();
        let join_failures = runtime
            .start_tracked_reconcile(second_cancel, async move {
                second_task_cancel.cancelled().await;
            })
            .await;
        assert_eq!(join_failures, 1);
        assert_eq!(runtime.supervisor.lock().await.reconcile_tasks.len(), 1);
        runtime.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_waits_for_in_flight_mutation() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let runtime = Arc::new(McpConfigRuntime::empty(temp.path().join("mcp.json")));
        let mutation_guard = runtime.mutation_lock.lock().await;
        let mutation_runtime = Arc::clone(&runtime);
        let mutation = tokio::spawn(async move {
            mutation_runtime
                .commit_candidate_owned(McpConfigFile::default())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if !runtime.supervisor.lock().await.mutation_tasks.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let shutdown_runtime = Arc::clone(&runtime);
        let mut shutdown = tokio::spawn(async move {
            shutdown_runtime.shutdown().await;
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut shutdown)
                .await
                .is_err(),
            "shutdown returned before the accepted mutation settled"
        );
        assert!(!runtime.shutdown.is_cancelled());
        drop(mutation_guard);
        mutation.await??;
        shutdown.await?;
        assert!(runtime.shutdown.is_cancelled());
        assert!(matches!(
            runtime.ensure_open(),
            Err(McpConfigRuntimeError::Closed)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn newer_reconcile_cancels_stale_and_shutdown_awaits_tasks() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let runtime = McpConfigRuntime::empty(temp.path().join("mcp.json"));
        let first_cancel = CancellationToken::new();
        let first_task_cancel = first_cancel.clone();
        let first_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first_task_finished = Arc::clone(&first_finished);
        runtime
            .start_tracked_reconcile(first_cancel.clone(), async move {
                first_task_cancel.cancelled().await;
                first_task_finished.store(true, Ordering::Release);
            })
            .await;

        let second_cancel = CancellationToken::new();
        let second_task_cancel = second_cancel.clone();
        let second_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_task_finished = Arc::clone(&second_finished);
        runtime
            .start_tracked_reconcile(second_cancel.clone(), async move {
                second_task_cancel.cancelled().await;
                second_task_finished.store(true, Ordering::Release);
            })
            .await;

        assert!(first_cancel.is_cancelled());
        assert!(!second_cancel.is_cancelled());
        runtime.shutdown().await;
        assert!(second_cancel.is_cancelled());
        assert!(first_finished.load(Ordering::Acquire));
        assert!(second_finished.load(Ordering::Acquire));
        assert!(runtime.supervisor.lock().await.reconcile_tasks.is_empty());
        assert!(matches!(
            runtime.ensure_open(),
            Err(McpConfigRuntimeError::Closed)
        ));

        let after_shutdown_cancel = CancellationToken::new();
        let started_after_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_started_after_shutdown = Arc::clone(&started_after_shutdown);
        runtime
            .start_tracked_reconcile(after_shutdown_cancel.clone(), async move {
                task_started_after_shutdown.store(true, Ordering::Release);
            })
            .await;
        assert!(after_shutdown_cancel.is_cancelled());
        assert!(!started_after_shutdown.load(Ordering::Acquire));
        Ok(())
    }

    #[test]
    fn malformed_existing_config_is_rejected_without_modification() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("mcp.json");
        let malformed = b"{ this is not valid MCP json";
        std::fs::write(&path, malformed)?;

        let result = load_mcp_config_snapshot(&path);
        assert!(matches!(result, Err(McpConfigRuntimeError::Parse { .. })));
        let persisted = std::fs::read(&path)?;
        assert_eq!(persisted.as_slice(), malformed);
        Ok(())
    }

    #[test]
    fn missing_import_source_is_rejected() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("missing.json");

        let result = load_existing_mcp_config_snapshot(&path);
        assert!(matches!(
            result,
            Err(McpConfigRuntimeError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound
        ));
        Ok(())
    }

    #[test]
    fn semantically_invalid_existing_config_is_rejected_without_modification() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("mcp.json");
        let invalid = br#"{"mcpServers":{"broken":{}}}"#;
        std::fs::write(&path, invalid)?;

        let result = load_mcp_config_snapshot(&path);
        assert!(matches!(
            result,
            Err(McpConfigRuntimeError::InvalidExisting { .. })
        ));
        let persisted = std::fs::read(&path)?;
        assert_eq!(persisted.as_slice(), invalid);
        Ok(())
    }

    #[tokio::test]
    async fn redacted_editor_values_do_not_overwrite_credentials() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("mcp.json");
        let mut initial = McpConfigFile::default();
        let mut stdio = stdio_entry("node", false);
        stdio
            .env
            .insert("API_KEY".to_string(), "secret-key".to_string());
        initial.mcp_servers.insert("stdio".to_string(), stdio);
        let mut remote = McpServerEntry {
            url: Some("https://example.com/mcp?token=secret-token".to_string()),
            ..Default::default()
        };
        remote.headers.insert(
            "Authorization".to_string(),
            "Bearer secret-token".to_string(),
        );
        initial.mcp_servers.insert("remote".to_string(), remote);
        let runtime = McpConfigRuntime::new(path.clone(), initial);

        let mut editor = runtime.snapshot().await;
        if let Some(entry) = editor.mcp_servers.get_mut("stdio") {
            entry
                .env
                .insert("API_KEY".to_string(), REDACTED_VALUE.to_string());
        }
        if let Some(entry) = editor.mcp_servers.get_mut("remote") {
            entry.headers.insert(
                "Authorization".to_string(),
                format!("Bearer {REDACTED_VALUE}"),
            );
            entry.url = Some(format!("https://example.com/mcp?token={REDACTED_VALUE}"));
        }
        runtime.commit_candidate(editor).await?;

        let reloaded = load_mcp_config_snapshot(&path)?;
        assert_eq!(
            reloaded
                .mcp_servers
                .get("stdio")
                .and_then(|entry| entry.env.get("API_KEY"))
                .map(String::as_str),
            Some("secret-key")
        );
        assert_eq!(
            reloaded
                .mcp_servers
                .get("remote")
                .and_then(|entry| entry.headers.get("Authorization"))
                .map(String::as_str),
            Some("Bearer secret-token")
        );
        assert_eq!(
            reloaded
                .mcp_servers
                .get("remote")
                .and_then(|entry| entry.url.as_deref()),
            Some("https://example.com/mcp?token=secret-token")
        );
        Ok(())
    }

    #[test]
    fn cli_override_selects_the_canonical_path() {
        let mut app_config = AppConfig::default();
        app_config.mcp.config_path = Some("from-yaml.json".to_string());
        assert_eq!(
            resolve_mcp_config_path(Some("from-cli.json"), &app_config),
            PathBuf::from("from-cli.json")
        );
        assert_eq!(
            resolve_mcp_config_path(None, &app_config),
            PathBuf::from("from-yaml.json")
        );
    }

    #[test]
    fn canonical_path_precedence_is_cli_then_yaml_then_environment() {
        assert_eq!(
            resolve_mcp_config_path_sources(
                Some("from-cli.json"),
                Some("from-yaml.json"),
                Some("from-env.json"),
            ),
            PathBuf::from("from-cli.json")
        );
        assert_eq!(
            resolve_mcp_config_path_sources(None, Some("from-yaml.json"), Some("from-env.json"),),
            PathBuf::from("from-yaml.json")
        );
        assert_eq!(
            resolve_mcp_config_path_sources(None, None, Some("from-env.json")),
            PathBuf::from("from-env.json")
        );
    }
}
