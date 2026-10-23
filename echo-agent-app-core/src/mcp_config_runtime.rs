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

use crate::config::EkoConfig;
use echo_agent::mcp::{McpConfigFile, McpServerEntry};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};
use tokio_util::sync::CancellationToken;

use crate::agent_handle::AgentHandle;

/// Upper bound for one MCP client connection during reconciliation.
///
/// The framework prepares a replacement client before swapping it in, so a
/// bounded *wait* never sacrifices the last-known-good connection. The
/// operation itself is never dropped on this deadline (see
/// [`BackgroundConnect`]): the pass stops waiting, reports the target
/// unconfirmed, and a background task keeps the framework preparation alive
/// until it reaches a terminal state.
const MCP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Upper bound for one MCP client disconnection during reconciliation.
///
/// The framework retains an unsettled client as cleanup debt, so a timed out
/// disconnect can be retried by a later generation. Until that retry is
/// confirmed the name must stay claimed: releasing it while the old transport
/// may still be half alive would let a plugin wire the same name underneath it.
const MCP_DISCONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

const REDACTED_VALUE: &str = "<redacted>";

/// Bounded deadlines for the two client operations a reconcile pass performs.
///
/// Production uses the module constants. Tests shrink them so deadline
/// regressions run in milliseconds instead of seconds.
#[derive(Clone, Copy, Debug)]
struct ReconcileDeadlines {
    connect: std::time::Duration,
    disconnect: std::time::Duration,
}

impl Default for ReconcileDeadlines {
    fn default() -> Self {
        Self {
            connect: MCP_CONNECT_TIMEOUT,
            disconnect: MCP_DISCONNECT_TIMEOUT,
        }
    }
}

/// Result class of one bounded reconcile operation.
///
/// Only [`Self::Confirmed`] is a safe point for settling the user namespace:
/// every other class means the previous connection may still be reachable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconcileOperation {
    /// The framework reported the target reached the requested state.
    Confirmed,
    /// The operation failed; the previous state of the target may survive.
    Failed,
    /// The operation exceeded its deadline and may still be in flight.
    TimedOut,
    /// A newer generation superseded this pass before the operation finished.
    Superseded,
}

/// Terminal result reported by a backgrounded connection operation.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ConnectOperationOutcome {
    Completed(echo_agent::mcp::McpTargetChange),
    Failed(String),
    /// Superseded before the framework future was ever polled.
    Superseded,
}

/// What the pass that issued a connection operation wants done with a result
/// that arrived after that pass stopped waiting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LateConnectDecision {
    /// The pass consumed the receipt in time; leave the published client.
    Keep,
    /// The pass timed out or was superseded; withdraw a client that this
    /// operation published after the pass reported the target unconfirmed.
    Withdraw,
}

/// Pass-side handle of one connection operation owned by a background task.
struct ConnectOperation {
    result: tokio::sync::oneshot::Receiver<ConnectOperationOutcome>,
    decision: tokio::sync::watch::Sender<Option<LateConnectDecision>>,
}

/// Pointer identity of one agent (reconcile target) inside this runtime.
///
/// A runtime is shared by every workspace agent, so a server name alone does
/// not identify a connection: the same name may be connected concurrently on
/// several agents. `AgentHandle` exposes no stable textual id (its agent name
/// is mutable), so identity is the address of the shared agent state. The
/// address stays unique for the whole lifetime of a tracked operation: the
/// background task keeps its own `AgentHandle` (an `Arc` clone) alive until the
/// operation is terminal, so no other agent can be allocated at that address
/// while a receipt for this one is still tracked.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AgentKey(usize);

impl AgentKey {
    fn of(agent: &AgentHandle) -> Self {
        Self(Arc::as_ptr(agent.inner()) as usize)
    }
}

/// Monotonic identity of one backgrounded connection operation.
///
/// The id is never reused, so an operation that finishes after its pass
/// stopped waiting can always tell whether the client currently wired for its
/// name is the one it published itself.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ConnectOperationId(u64);

/// The physical client whose publication or unchanged confirmation an
/// operation owns. A name can be rewired outside this runtime; retain its
/// identity weakly so that an old withdrawal cannot address that new client.
struct PublishedConnect {
    operation: ConnectOperationId,
    client: std::sync::Weak<echo_agent::mcp::McpClient>,
}

/// One target server of one agent, the granularity at which background
/// connection operations are tracked and withdrawn.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BackgroundConnectTarget {
    agent: AgentKey,
    name: String,
}

impl BackgroundConnectTarget {
    fn new(agent: &AgentHandle, name: &str) -> Self {
        Self {
            agent: AgentKey::of(agent),
            name: name.to_string(),
        }
    }
}

/// Settlement receipt for one connection operation owned by a background task.
///
/// A reconcile pass must never drop an in-flight framework preparation. The
/// manager only clears its `preparing` ticket when the operation itself
/// reaches a terminal state (`McpPreparationScope` has no `Drop`), so a dropped
/// preparation keeps `building` set forever and every later connection for that
/// name is rejected with "another preparation is still active". Keeping the
/// operation in a task therefore trades a bounded lock wait for a permanently
/// unusable server name.
struct BackgroundConnect {
    settled: tokio::sync::watch::Sender<bool>,
}

impl BackgroundConnect {
    /// Resolve once the owning operation reached a terminal state. A dropped
    /// sender also resolves: the operation cannot settle anything anymore.
    async fn wait_settled(&self) {
        let mut settled = self.settled.subscribe();
        loop {
            let is_settled = *settled.borrow();
            if is_settled {
                return;
            }
            if settled.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Result of the withdrawal a late connection operation performs for itself.
enum LateWithdrawOutcome {
    /// A newer operation published the live client, so this operation withdrew
    /// nothing.
    Superseded,
    /// The framework was asked to disconnect this operation's own client.
    Attempted(Result<echo_agent::mcp::McpTargetChange, String>),
}

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
pub fn resolve_mcp_config_path(cli_override: Option<&str>, app_config: &EkoConfig) -> PathBuf {
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
        .unwrap_or_else(|| crate::data_root::user_data_path("mcp.json"))
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

/// One independently isolated Agent/ToolManager namespace that must converge
/// to the process-wide durable user MCP generation.
#[derive(Clone)]
pub(crate) struct McpReconcileTarget {
    agent: AgentHandle,
    ownership: Arc<McpNameOwnershipRegistry>,
    pool: Option<Arc<crate::agent_pool::AgentPool>>,
}

impl McpReconcileTarget {
    pub(crate) fn new(
        agent: AgentHandle,
        ownership: Arc<McpNameOwnershipRegistry>,
        pool: Option<Arc<crate::agent_pool::AgentPool>>,
    ) -> Self {
        Self {
            agent,
            ownership,
            pool,
        }
    }
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

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum ConnectTestPoint {
    BeforeAdmission,
    BeforeFirstPoll,
}

#[cfg(test)]
struct ConnectTestGate {
    point: ConnectTestPoint,
    generation: u64,
    started: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconcileFault {
    /// Report an error without touching the framework manager.
    Fail,
    /// Keep the operation pending so the production deadline must fire.
    Hang,
}

#[cfg(test)]
impl ReconcileFault {
    /// Substitute the operation future so faults pass through the exact same
    /// bounded/deadline code path as a real framework operation.
    fn operation(
        self,
    ) -> futures::future::BoxFuture<
        'static,
        Result<echo_agent::mcp::McpTargetChange, echo_agent::error::ReactError>,
    > {
        match self {
            Self::Fail => Box::pin(async {
                Err(echo_agent::error::ReactError::Other(
                    "fixture MCP operation failure".to_string(),
                ))
            }),
            Self::Hang => Box::pin(std::future::pending()),
        }
    }
}

/// One queued fault, consumed by the next matching reconcile operation.
#[cfg(test)]
struct QueuedReconcileFault {
    name: String,
    fault: ReconcileFault,
}

/// Deterministic operation faults for reconcile tests.
///
/// The app layer can only observe what the framework manager reports, and the
/// manager has no public way to construct a client whose close fails. Injecting
/// the *operation outcome* here keeps the production convergence and settle
/// gates under test without weakening them. Only disconnects need this seam:
/// connection timeouts are covered with a real slow MCP server, which is what
/// proves the framework preparation is not stranded.
#[cfg(test)]
#[derive(Default)]
struct ReconcileFaultQueue {
    queued: Vec<QueuedReconcileFault>,
}

#[cfg(test)]
impl ReconcileFaultQueue {
    fn push(&mut self, name: &str, fault: ReconcileFault) {
        self.queued.push(QueuedReconcileFault {
            name: name.to_string(),
            fault,
        });
    }

    fn take(&mut self, name: &str) -> Option<ReconcileFault> {
        let index = self.queued.iter().position(|queued| queued.name == name)?;
        Some(self.queued.remove(index).fault)
    }
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

    #[cfg(test)]
    pub(crate) async fn is_user_owned(&self, name: &str) -> bool {
        matches!(
            self.state.lock().await.owners.get(name),
            Some(McpNameOwner::User)
        )
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

#[derive(Clone, Debug)]
struct ReconcilePlan {
    /// Tracked user names whose desired state is "not connected". Includes
    /// servers that were just disabled or removed, plus any disconnect a
    /// previous generation could not confirm.
    disconnect: Vec<String>,
    /// Enabled desired entries that must be connected or replaced.
    connect: Vec<(String, McpServerEntry)>,
    /// Enabled names in the desired snapshot. This becomes the retry frontier
    /// only after a pass confirmed every operation.
    active_names: BTreeSet<String>,
}

impl ReconcilePlan {
    #[cfg(test)]
    fn between(previous: &McpConfigFile, current: &McpConfigFile) -> Self {
        Self::with_tracked_names(previous.mcp_servers.keys().cloned(), current)
    }

    fn with_tracked_names(
        tracked_names: impl IntoIterator<Item = String>,
        current: &McpConfigFile,
    ) -> Self {
        // A canceled older generation may not have applied every disconnection
        // or connection, so the caller passes every name that could still own a
        // live connection (leftovers plus the previous snapshot). Names that
        // exist only in the plugin registry are outside this set.
        let mut disconnect = tracked_names.into_iter().collect::<Vec<_>>();
        disconnect.sort();
        disconnect.dedup();
        let active_names = enabled_server_names(current)
            .into_iter()
            .collect::<BTreeSet<_>>();
        // Disabling an entry is a connection change, not a no-op: only names
        // that stay enabled are reconciled through the framework manager's
        // replacement/unchanged contract. Everything else gets an explicit
        // disconnect pass so an enabled -> disabled flip withdraws the live
        // client instead of merely persisting `disabled: true`.
        disconnect.retain(|name| !active_names.contains(name));

        let mut connect = current
            .mcp_servers
            .iter()
            .filter(|(_, entry)| !entry.disabled)
            .map(|(name, entry)| (name.clone(), entry.clone()))
            .collect::<Vec<_>>();
        connect.sort_by(|left, right| left.0.cmp(&right.0));

        Self {
            disconnect,
            connect,
            active_names,
        }
    }

    fn is_empty(&self) -> bool {
        self.disconnect.is_empty() && self.connect.is_empty()
    }

    /// Every name this pass touches, used to serialize against connection
    /// operations still owned by a previous background task.
    fn touched_names(&self) -> BTreeSet<String> {
        self.disconnect
            .iter()
            .chain(self.connect.iter().map(|(name, _)| name))
            .cloned()
            .collect()
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
    deadlines: ReconcileDeadlines,
    /// Connection operations that outlived their reconcile pass and keep
    /// running in a background task, keyed by the exact target (agent and
    /// server name) and by operation identity. Later passes serialize against
    /// these before touching the same target; the same server name on another
    /// agent is a different entry and never replaces this one.
    background_connects: Mutex<
        BTreeMap<BackgroundConnectTarget, BTreeMap<ConnectOperationId, Arc<BackgroundConnect>>>,
    >,
    /// Operation that most recently published the live client for one target.
    /// The framework addresses clients by name, so a late operation must prove
    /// that the live client is still its own before withdrawing.
    published_connects: Mutex<BTreeMap<BackgroundConnectTarget, PublishedConnect>>,
    /// Allocates [`ConnectOperationId`]s for background connection operations.
    next_connect_operation: AtomicU64,
    shutdown: CancellationToken,
    #[cfg(test)]
    writer_gate: Mutex<Option<WriterTestGate>>,
    #[cfg(test)]
    reconcile_faults: Mutex<ReconcileFaultQueue>,
    #[cfg(test)]
    connect_test_gates: Mutex<Vec<ConnectTestGate>>,
}

impl McpConfigRuntime {
    /// Construct the application-owned authority from an explicit persisted
    /// snapshot. Embedders use this when assembling `AppState` directly.
    pub fn from_snapshot(path: PathBuf, snapshot: McpConfigFile) -> Self {
        Self::new(path, snapshot)
    }

    pub(crate) fn new(path: PathBuf, snapshot: McpConfigFile) -> Self {
        // Only enabled entries can own a live client when this runtime starts:
        // connections are process-local, and bootstrap connects enabled
        // entries only. Disabled entries therefore need no disconnect debt.
        let unreconciled_user_names = enabled_server_names(&snapshot).into_iter().collect();
        let ownership = McpNameOwnershipRegistry::new(snapshot.mcp_servers.keys().cloned());
        Self {
            path,
            snapshot: RwLock::new(snapshot),
            mutation_lock: Mutex::new(()),
            generation: AtomicU64::new(0),
            supervisor: Mutex::new(McpRuntimeSupervisor::default()),
            unreconciled_user_names: Mutex::new(unreconciled_user_names),
            ownership,
            deadlines: ReconcileDeadlines::default(),
            background_connects: Mutex::new(BTreeMap::new()),
            published_connects: Mutex::new(BTreeMap::new()),
            next_connect_operation: AtomicU64::new(0),
            shutdown: CancellationToken::new(),
            #[cfg(test)]
            writer_gate: Mutex::new(None),
            #[cfg(test)]
            reconcile_faults: Mutex::new(ReconcileFaultQueue::default()),
            #[cfg(test)]
            connect_test_gates: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    fn empty(path: PathBuf) -> Self {
        Self::new(path, McpConfigFile::default())
    }

    #[cfg(test)]
    fn with_deadlines(mut self, deadlines: ReconcileDeadlines) -> Self {
        self.deadlines = deadlines;
        self
    }

    #[cfg(test)]
    async fn queue_disconnect_fault(&self, name: &str, fault: ReconcileFault) {
        self.reconcile_faults.lock().await.push(name, fault);
    }

    #[cfg(test)]
    async fn take_reconcile_fault(&self, name: &str) -> Option<ReconcileFault> {
        self.reconcile_faults.lock().await.take(name)
    }

    #[cfg(test)]
    async fn pause_connect_at(&self, point: ConnectTestPoint, generation: u64) {
        let gate = {
            let mut gates = self.connect_test_gates.lock().await;
            gates
                .iter()
                .position(|gate| gate.point == point && gate.generation == generation)
                .map(|index| gates.remove(index))
        };
        if let Some(gate) = gate {
            let _ = gate.started.send(());
            let _ = gate.release.await;
        }
    }

    /// Number of background connection operations that have not settled yet.
    async fn outstanding_background_connects(&self) -> usize {
        self.background_connects
            .lock()
            .await
            .values()
            .map(|operations| operations.len())
            .sum()
    }

    #[cfg(test)]
    async fn background_connect_count(&self) -> usize {
        self.outstanding_background_connects().await
    }

    /// Publish one operation's receipt before its task starts, so a later pass
    /// serializes against this exact target instead of starting a second
    /// preparation for the same name.
    async fn track_background_connect(
        &self,
        target: &BackgroundConnectTarget,
        operation: ConnectOperationId,
        receipt: Arc<BackgroundConnect>,
    ) {
        self.background_connects
            .lock()
            .await
            .entry(target.clone())
            .or_default()
            .insert(operation, receipt);
    }

    /// Remove exactly this operation's receipt. A newer operation (or another
    /// agent's operation for the same name) may be tracked for the same target
    /// by now and must stay observable.
    async fn untrack_background_connect(
        &self,
        target: &BackgroundConnectTarget,
        operation: ConnectOperationId,
    ) {
        let mut background = self.background_connects.lock().await;
        let Some(operations) = background.get_mut(target) else {
            return;
        };
        operations.remove(&operation);
        if operations.is_empty() {
            background.remove(target);
        }
    }

    async fn background_receipts(
        &self,
        target: &BackgroundConnectTarget,
    ) -> Vec<Arc<BackgroundConnect>> {
        self.background_connects
            .lock()
            .await
            .get(target)
            .map(|operations| operations.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Record the operation that published the live client for `target`.
    ///
    /// Always called while the target agent's write lock is held, which is the
    /// same lock every framework connection mutation of that agent takes. The
    /// record is therefore ordered against any later publication or withdrawal
    /// for the same target.
    async fn record_published_connect(
        &self,
        target: &BackgroundConnectTarget,
        operation: ConnectOperationId,
        client: &Arc<echo_agent::mcp::McpClient>,
    ) {
        self.published_connects.lock().await.insert(
            target.clone(),
            PublishedConnect {
                operation,
                client: Arc::downgrade(client),
            },
        );
    }

    async fn clear_published_connect(&self, target: &BackgroundConnectTarget) {
        self.published_connects.lock().await.remove(target);
    }

    /// True when `operation` published the client that is currently wired for
    /// `target`, i.e. withdrawing it cannot hit a newer client.
    async fn owns_published_connect(
        &self,
        target: &BackgroundConnectTarget,
        operation: ConnectOperationId,
        client: &Arc<echo_agent::mcp::McpClient>,
    ) -> bool {
        self.published_connects
            .lock()
            .await
            .get(target)
            .is_some_and(|publication| {
                publication.operation == operation
                    && publication.client.ptr_eq(&Arc::downgrade(client))
            })
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

    pub(crate) async fn replace_and_reconcile(
        self: &Arc<Self>,
        targets: Vec<McpReconcileTarget>,
        candidate: McpConfigFile,
    ) -> Result<u64, McpConfigRuntimeError> {
        self.run_owned_mutation(move |runtime| async move {
            runtime
                .replace_and_reconcile_inner(targets, candidate)
                .await
        })
        .await
    }

    async fn replace_and_reconcile_inner(
        self: &Arc<Self>,
        targets: Vec<McpReconcileTarget>,
        candidate: McpConfigFile,
    ) -> Result<u64, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.ensure_open()?;
        let commit = self.commit_candidate_locked(candidate).await?;
        let generation = commit.generation;
        self.start_reconcile(targets, commit).await;
        Ok(generation)
    }

    pub(crate) async fn upsert_and_reconcile(
        self: &Arc<Self>,
        targets: Vec<McpReconcileTarget>,
        name: String,
        entry: McpServerEntry,
    ) -> Result<u64, McpConfigRuntimeError> {
        self.run_owned_mutation(move |runtime| async move {
            runtime
                .upsert_and_reconcile_inner(targets, name, entry)
                .await
        })
        .await
    }

    async fn upsert_and_reconcile_inner(
        self: &Arc<Self>,
        targets: Vec<McpReconcileTarget>,
        name: String,
        entry: McpServerEntry,
    ) -> Result<u64, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.ensure_open()?;
        let commit = self.commit_upsert_locked(name, entry).await?;
        let generation = commit.generation;
        self.start_reconcile(targets, commit).await;
        Ok(generation)
    }

    pub(crate) async fn set_enabled_and_reconcile(
        self: &Arc<Self>,
        targets: Vec<McpReconcileTarget>,
        name: &str,
        enabled: bool,
    ) -> Result<u64, McpConfigRuntimeError> {
        let name = name.to_string();
        self.run_owned_mutation(move |runtime| async move {
            runtime
                .set_enabled_and_reconcile_inner(targets, &name, enabled)
                .await
        })
        .await
    }

    async fn set_enabled_and_reconcile_inner(
        self: &Arc<Self>,
        targets: Vec<McpReconcileTarget>,
        name: &str,
        enabled: bool,
    ) -> Result<u64, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.ensure_open()?;
        let commit = self.commit_toggle_locked(name, enabled).await?;
        let generation = commit.generation;
        self.start_reconcile(targets, commit).await;
        Ok(generation)
    }

    pub(crate) async fn remove_and_reconcile(
        self: &Arc<Self>,
        targets: Vec<McpReconcileTarget>,
        name: &str,
    ) -> Result<u64, McpConfigRuntimeError> {
        let name = name.to_string();
        self.run_owned_mutation(move |runtime| async move {
            runtime.remove_and_reconcile_inner(targets, &name).await
        })
        .await
    }

    async fn remove_and_reconcile_inner(
        self: &Arc<Self>,
        targets: Vec<McpReconcileTarget>,
        name: &str,
    ) -> Result<u64, McpConfigRuntimeError> {
        let _mutation_guard = self.mutation_lock.lock().await;
        self.ensure_open()?;
        let commit = self.commit_remove_locked(name).await?;
        let generation = commit.generation;
        self.start_reconcile(targets, commit).await;
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
            echo_agent::utils::fs::atomic_write(&write_path, &bytes)
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
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
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

    async fn start_reconcile(
        self: &Arc<Self>,
        targets: Vec<McpReconcileTarget>,
        commit: McpConfigCommit,
    ) {
        let plan = {
            let mut names = self.unreconciled_user_names.lock().await;
            // Only entries that are enabled on either side can own (or be about
            // to own) a live client. Keeping the desired enabled names in the
            // frontier before the pass means a failed connection stays queued
            // for the next generation instead of silently disappearing.
            names.extend(enabled_server_names(&commit.previous));
            names.extend(enabled_server_names(&commit.current));
            ReconcilePlan::with_tracked_names(names.iter().cloned(), &commit.current)
        };
        let current_names = commit
            .current
            .mcp_servers
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        for target in &targets {
            target
                .ownership
                .claim_user_names(current_names.iter().cloned())
                .await;
            if let Some(pool) = target.pool.as_ref() {
                pool.update_mcp_config_snapshot(commit.current.clone())
                    .await;
            }
        }
        let cancel = commit.cancel.clone();
        let runtime = Arc::clone(self);
        self.start_tracked_reconcile(cancel, async move {
            runtime.reconcile(targets, commit, plan).await
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

        // Backgrounded connection operations are deliberately not dropped (a
        // dropped framework preparation would keep its manager ticket forever).
        // Their transports bound their own settlement, so shutdown observes them
        // instead of waiting unboundedly.
        let outstanding = self.outstanding_background_connects().await;
        if outstanding > 0 {
            tracing::warn!(
                outstanding,
                "MCP connection operations still settling in background tasks at shutdown; the framework retains their tickets and each transport bounds its own close"
            );
        }
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
        targets: Vec<McpReconcileTarget>,
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
        let reconciliations = targets
            .iter()
            .map(|target| {
                Self::reconcile_target(
                    target.agent.clone(),
                    Arc::clone(&self),
                    generation,
                    commit.cancel.clone(),
                    plan.clone(),
                )
            })
            .collect::<Vec<_>>();
        // A pass without any target cannot confirm a connection change. Only a
        // pass with nothing to do at all is a safe settle point in that case.
        let converged = if reconciliations.is_empty() {
            plan.is_empty()
        } else {
            futures::future::join_all(reconciliations)
                .await
                .into_iter()
                .all(|converged| converged)
        };
        if !converged {
            // At least one disconnect or connection was never confirmed. Keep
            // both the user namespace claims and the retry frontier exactly as
            // they were: settling here would release a name whose old client
            // may still be half alive, letting a plugin or a newer user
            // generation take over underneath it.
            tracing::warn!(
                generation,
                pending = plan.disconnect.len(),
                connect = plan.connect.len(),
                "MCP user config reconcile did not converge; name ownership and retry state retained"
            );
            return;
        }
        if self.generation() != generation {
            return;
        }
        let mut names = self.unreconciled_user_names.lock().await;
        if self.generation() != generation {
            return;
        }
        *names = plan.active_names.clone();
        // Disabled entries stay user-owned: the durable config still declares
        // that name, so a plugin must not take it over while the user has it
        // switched off. Only names absent from the desired config are dropped.
        self.ownership.settle_user_names(&current_names).await;
        for target in targets {
            target.ownership.settle_user_names(&current_names).await;
        }
        tracing::info!(
            generation,
            active = plan.active_names.len(),
            configured = current_names.len(),
            "MCP user config reconcile settled"
        );
    }

    async fn reconcile_target(
        agent: AgentHandle,
        runtime: Arc<Self>,
        generation: u64,
        reconcile_cancel: CancellationToken,
        plan: ReconcilePlan,
    ) -> bool {
        // Phase 1 (no agent lock): observe connection operations that outlived
        // an earlier pass. Their background tasks hold this agent's write lock
        // while the framework future runs, so waiting for them here - where
        // this pass holds no lock - is the only deadlock-free point. Only this
        // exact target's receipts count: another agent connecting the same
        // server name must neither mask nor be masked by them.
        let mut outstanding = BTreeSet::new();
        for name in plan.touched_names() {
            if reconcile_cancel.is_cancelled() || runtime.generation() != generation {
                return false;
            }
            if !runtime
                .observe_background_connect(&agent, &name, runtime.deadlines.connect)
                .await
            {
                outstanding.insert(name);
            }
        }
        if !outstanding.is_empty() {
            // Every operation that outlived its pass keeps this agent's write
            // lock for as long as the framework future runs. Taking the lock
            // here would wait without a bound, and a lock wait is not an
            // operation outcome: reporting it as one would hide that this
            // target is still unconfirmed. Stop before phase 2/3 instead; the
            // caller keeps the retry frontier, so a later generation retries
            // the same names.
            tracing::warn!(
                generation,
                outstanding = outstanding.len(),
                "MCP reconcile observed unsettled connection operations; pass stopped before phase 2"
            );
            return false;
        }

        // Phase 2: withdraw every server that must not stay connected, under
        // one write lock. A dropped disconnect future is safe: transports keep
        // cancellation-safe close receipts and the manager retains unsettled
        // clients as retryable cleanup debt, so a later pass can retry.
        //
        // Acquiring the lock is itself bounded and cancellable: it can be
        // owned by another target server's connection operation of this same
        // agent (or by a turn), and the cancellation token is only observable
        // after the lock is held. An unbounded `write_async` would let cancel,
        // retry, config removal, and shutdown wait forever behind it.
        let disconnect_targets = plan
            .disconnect
            .iter()
            .map(|name| BackgroundConnectTarget::new(&agent, name))
            .collect::<Vec<_>>();
        let mut guard = match tokio::select! {
            _ = reconcile_cancel.cancelled() => None,
            acquired = tokio::time::timeout(
                runtime.deadlines.connect,
                agent.inner().write(),
            ) => acquired.ok(),
        } {
            Some(guard) => guard,
            None => {
                tracing::warn!(
                    generation,
                    timeout_secs = runtime.deadlines.connect.as_secs_f64(),
                    cancelled = reconcile_cancel.is_cancelled(),
                    "MCP reconcile could not take the agent write lock; target stays unconfirmed"
                );
                return false;
            }
        };
        let mut settled = true;
        'disconnect: {
            for target in &disconnect_targets {
                if reconcile_cancel.is_cancelled() || runtime.generation() != generation {
                    settled = false;
                    break 'disconnect;
                }
                match runtime
                    .disconnect_entry(&mut guard, target, generation, &reconcile_cancel)
                    .await
                {
                    ReconcileOperation::Confirmed => {}
                    ReconcileOperation::Superseded => {
                        settled = false;
                        break 'disconnect;
                    }
                    ReconcileOperation::Failed | ReconcileOperation::TimedOut => {
                        settled = false;
                    }
                }
            }
        }
        // Tool projections may have been revoked by a partial pass, so the
        // prompt is refreshed on failures as well.
        let disabled_tools = guard.disabled_tool_names();
        crate::subagent_prompt::refresh_primary_system_prompt(&mut guard, &disabled_tools);
        drop(guard);

        // Phase 3 (no lock): reconcile desired connections. Every operation is
        // owned by a background task that keeps the framework preparation alive
        // after this pass stops waiting, so a deadline or a newer generation can
        // never strand the manager's `preparing` ticket.
        for (name, entry) in plan.connect {
            if reconcile_cancel.is_cancelled() || runtime.generation() != generation {
                return false;
            }
            match runtime
                .connect_entry(&agent, &name, entry, generation, &reconcile_cancel)
                .await
            {
                ReconcileOperation::Confirmed => {}
                ReconcileOperation::Superseded => return false,
                ReconcileOperation::Failed | ReconcileOperation::TimedOut => settled = false,
            }
        }
        settled
    }

    /// Withdraw one tracked user server connection.
    ///
    /// Only a framework receipt is `Confirmed`. A failure or deadline keeps the
    /// name claimed and lets a later generation retry the close; the framework
    /// either keeps the last-known-good client or retains it as cleanup debt,
    /// so this call never has to guess.
    async fn disconnect_entry(
        self: &Arc<Self>,
        agent: &mut echo_agent::agent::ReactAgent,
        target: &BackgroundConnectTarget,
        generation: u64,
        cancel: &CancellationToken,
    ) -> ReconcileOperation {
        let name = target.name.as_str();
        if cancel.is_cancelled() || self.generation() != generation {
            return ReconcileOperation::Superseded;
        }
        let deadline = self.deadlines.disconnect;
        #[cfg(test)]
        if let Some(fault) = self.take_reconcile_fault(name).await {
            return self
                .run_bounded_disconnect(name, generation, deadline, cancel, fault.operation())
                .await;
        }
        let operation = agent.reconcile_mcp_entry(name, None);
        let outcome = self
            .run_bounded_disconnect(name, generation, deadline, cancel, Box::pin(operation))
            .await;
        if outcome == ReconcileOperation::Confirmed {
            // No client remains wired for this target, so no operation owns its
            // publication anymore.
            self.clear_published_connect(target).await;
        }
        outcome
    }

    /// Drive one disconnect to a bounded, observable outcome.
    ///
    /// Supersession wins over the deadline so a newer generation never waits on
    /// a stale one. Dropping the disconnect future cannot strand a framework
    /// preparation (there is none), and the manager keeps failed or abandoned
    /// closes as retryable cleanup debt.
    async fn run_bounded_disconnect(
        &self,
        name: &str,
        generation: u64,
        deadline: std::time::Duration,
        cancel: &CancellationToken,
        operation: futures::future::BoxFuture<
            '_,
            Result<echo_agent::mcp::McpTargetChange, echo_agent::error::ReactError>,
        >,
    ) -> ReconcileOperation {
        tokio::select! {
            _ = cancel.cancelled() => ReconcileOperation::Superseded,
            result = tokio::time::timeout(deadline, operation) => {
                match result {
                    Ok(Ok(change)) => {
                        tracing::info!(
                            server = %name,
                            generation,
                            ?change,
                            "MCP user server disconnection confirmed"
                        );
                        ReconcileOperation::Confirmed
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(
                            server = %name,
                            generation,
                            %error,
                            "MCP user server disconnect failed; name ownership and retry state retained"
                        );
                        ReconcileOperation::Failed
                    }
                    Err(_) => {
                        tracing::warn!(
                            server = %name,
                            generation,
                            timeout_secs = deadline.as_secs_f64(),
                            "MCP user server disconnect timed out; name ownership and retry state retained"
                        );
                        ReconcileOperation::TimedOut
                    }
                }
            }
        }
    }

    /// Connect or replace one desired user server.
    ///
    /// The framework operation runs in a background task and this pass only
    /// waits for its receipt. If the deadline expires or a newer generation
    /// supersedes the pass, the task is told to withdraw any client it
    /// publishes late instead of being dropped. Failures and deadlines leave the
    /// target unconfirmed, which forbids settling the namespace this generation.
    async fn connect_entry(
        self: &Arc<Self>,
        agent: &AgentHandle,
        name: &str,
        entry: McpServerEntry,
        generation: u64,
        cancel: &CancellationToken,
    ) -> ReconcileOperation {
        if cancel.is_cancelled() || self.generation() != generation {
            return ReconcileOperation::Superseded;
        }
        #[cfg(test)]
        self.pause_connect_at(ConnectTestPoint::BeforeAdmission, generation)
            .await;
        let deadline = self.deadlines.connect;
        let Some(ConnectOperation {
            mut result,
            decision,
        }) = self
            .spawn_connect_operation(agent, name, entry, generation, cancel.clone())
            .await
        else {
            return ReconcileOperation::Superseded;
        };
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = decision.send_replace(Some(LateConnectDecision::Withdraw));
                tracing::info!(
                    server = %name,
                    "MCP user server connection was superseded; the background operation keeps settling"
                );
                ReconcileOperation::Superseded
            }
            outcome = &mut result => {
                // A durable commit can supersede this pass before its cancel
                // token is signalled. Consume a receipt only while its epoch
                // is current, including when result/cancel became ready together.
                if cancel.is_cancelled() || self.generation() != generation {
                    let _ = decision.send_replace(Some(LateConnectDecision::Withdraw));
                    return ReconcileOperation::Superseded;
                }
                // The pass consumed the receipt, so a client this operation
                // published belongs to this generation.
                let _ = decision.send_replace(Some(LateConnectDecision::Keep));
                match outcome {
                    Ok(ConnectOperationOutcome::Completed(change)) => {
                        tracing::info!(
                            server = %name,
                            ?change,
                            "MCP user server connection confirmed"
                        );
                        ReconcileOperation::Confirmed
                    }
                    Ok(ConnectOperationOutcome::Failed(error)) => {
                        tracing::warn!(
                            server = %name,
                            %error,
                            "MCP user server connection failed; name ownership and retry state retained"
                        );
                        ReconcileOperation::Failed
                    }
                    Ok(ConnectOperationOutcome::Superseded) => ReconcileOperation::Superseded,
                    Err(_) => {
                        tracing::warn!(
                            server = %name,
                            "MCP user server connection task ended without reporting a receipt; name ownership and retry state retained"
                        );
                        ReconcileOperation::Failed
                    }
                }
            }
            _ = tokio::time::sleep(deadline) => {
                // The operation keeps running: dropping it here would leave the
                // framework preparation ticket allocated for the rest of the
                // agent's life.
                let _ = decision.send_replace(Some(LateConnectDecision::Withdraw));
                tracing::warn!(
                    server = %name,
                    timeout_secs = deadline.as_secs_f64(),
                    "MCP user server connection timed out; the background operation keeps settling and the name stays claimed"
                );
                ReconcileOperation::TimedOut
            }
        }
    }

    /// Start a registered operation while preserving the cross-component lock
    /// order. Plugin cleanup can hold name ownership while waiting for the
    /// Agent, and durable commits hold `mutation_lock` while waiting for name
    /// ownership. Never wait for `mutation_lock` while holding the Agent lock.
    async fn run_admitted_connect(
        self: &Arc<Self>,
        agent: &AgentHandle,
        target: &BackgroundConnectTarget,
        operation: ConnectOperationId,
        entry: McpServerEntry,
        generation: u64,
        cancel: &CancellationToken,
    ) -> ConnectOperationOutcome {
        loop {
            let mut target_agent = tokio::select! {
                _ = cancel.cancelled() => return ConnectOperationOutcome::Superseded,
                _ = self.shutdown.cancelled() => return ConnectOperationOutcome::Superseded,
                guard = agent.inner().write() => guard,
            };
            #[cfg(test)]
            self.pause_connect_at(ConnectTestPoint::BeforeFirstPoll, generation)
                .await;

            let admission = match self.mutation_lock.try_lock() {
                Ok(admission) => admission,
                Err(_) => {
                    drop(target_agent);
                    // Wait without retaining the Agent, then try the pair
                    // again. A commit or plugin cleanup can now make progress.
                    let admission = tokio::select! {
                        _ = cancel.cancelled() => return ConnectOperationOutcome::Superseded,
                        _ = self.shutdown.cancelled() => return ConnectOperationOutcome::Superseded,
                        guard = self.mutation_lock.lock() => guard,
                    };
                    if cancel.is_cancelled()
                        || self.shutdown.is_cancelled()
                        || self.generation() != generation
                    {
                        return ConnectOperationOutcome::Superseded;
                    }
                    drop(admission);
                    continue;
                }
            };
            if cancel.is_cancelled()
                || self.shutdown.is_cancelled()
                || self.generation() != generation
            {
                return ConnectOperationOutcome::Superseded;
            }

            // Validity and the framework's first poll are one admission step
            // under the commit gate. Continue the exact same pending future
            // after releasing that gate: dropping a preparation would strand
            // its framework scope. No transport wait retains mutation_lock.
            let result = {
                let mut preparation =
                    Box::pin(target_agent.reconcile_mcp_entry(&target.name, Some(entry)));
                let first_poll = futures::future::poll_fn(|context| {
                    std::task::Poll::Ready(std::future::Future::poll(preparation.as_mut(), context))
                })
                .await;
                drop(admission);
                match first_poll {
                    std::task::Poll::Ready(result) => result,
                    std::task::Poll::Pending => preparation.await,
                }
            };
            return match result {
                Ok(change) => {
                    if matches!(
                        change,
                        echo_agent::mcp::McpTargetChange::Connected
                            | echo_agent::mcp::McpTargetChange::Replaced
                            | echo_agent::mcp::McpTargetChange::Unchanged
                    ) && let Some(client) = target_agent.mcp_client(&target.name)
                    {
                        // Unchanged transfers confirmation authority for the
                        // exact same physical client, so its previous operation
                        // can no longer withdraw that confirmed connection.
                        self.record_published_connect(target, operation, &client)
                            .await;
                    }
                    let disabled_tools = target_agent.disabled_tool_names();
                    crate::subagent_prompt::refresh_primary_system_prompt(
                        &mut target_agent,
                        &disabled_tools,
                    );
                    ConnectOperationOutcome::Completed(change)
                }
                Err(error) => ConnectOperationOutcome::Failed(error.to_string()),
            };
        }
    }

    /// Register and spawn one framework connection operation.
    ///
    /// The settlement receipt is published before the task starts, so a pass
    /// that arrives later serializes against this exact target instead of
    /// starting a second preparation for the same name. Receipts are keyed by
    /// target (agent and name) *and* operation identity: another agent
    /// connecting the same name cannot replace this operation's receipt, and a
    /// late withdrawal can only address the client this operation published.
    async fn spawn_connect_operation(
        self: &Arc<Self>,
        agent: &AgentHandle,
        name: &str,
        entry: McpServerEntry,
        generation: u64,
        cancel: CancellationToken,
    ) -> Option<ConnectOperation> {
        // Use the same gate as durable generation commits. If a newer commit
        // wins first, an old pass can never register after the newer pass has
        // observed an empty receipt frontier and confirmed its client. If this
        // admission wins first, its receipt is visible before that commit can
        // start phase 1. No framework preparation is polled under this guard.
        let admission = self.mutation_lock.lock().await;
        if cancel.is_cancelled() || self.shutdown.is_cancelled() || self.generation() != generation
        {
            return None;
        }
        let target = BackgroundConnectTarget::new(agent, name);
        let operation =
            ConnectOperationId(self.next_connect_operation.fetch_add(1, Ordering::AcqRel));
        let (result_sender, result) = tokio::sync::oneshot::channel();
        let (decision, decision_receiver) = tokio::sync::watch::channel(None);
        let (settled, _) = tokio::sync::watch::channel(false);
        let receipt = Arc::new(BackgroundConnect { settled });
        self.track_background_connect(&target, operation, Arc::clone(&receipt))
            .await;
        let runtime = Arc::clone(self);
        let agent = agent.clone();
        let operation_name = name.to_string();
        drop(admission);
        tokio::spawn(async move {
            let outcome = runtime
                .run_admitted_connect(&agent, &target, operation, entry, generation, &cancel)
                .await;
            let published = matches!(
                outcome,
                ConnectOperationOutcome::Completed(
                    echo_agent::mcp::McpTargetChange::Connected
                        | echo_agent::mcp::McpTargetChange::Replaced
                )
            );
            let _ = result_sender.send(outcome);
            let decision =
                wait_for_late_connect_decision(decision_receiver, runtime.deadlines.connect).await;
            if decision == LateConnectDecision::Withdraw && published {
                let withdraw_name = operation_name.clone();
                let withdraw_runtime = Arc::clone(&runtime);
                let withdraw_target = target.clone();
                match agent
                    .write_async(move |agent| {
                        Box::pin(async move {
                            // A newer operation may have replaced this
                            // operation's client in the meantime. The framework
                            // addresses clients by name, so the publication
                            // record is the only way to withdraw this
                            // operation's own client instead of a connection a
                            // newer generation already confirmed.
                            let Some(current_client) = agent.mcp_client(&withdraw_name) else {
                                return LateWithdrawOutcome::Superseded;
                            };
                            if !withdraw_runtime
                                .owns_published_connect(
                                    &withdraw_target,
                                    operation,
                                    &current_client,
                                )
                                .await
                            {
                                return LateWithdrawOutcome::Superseded;
                            }
                            let withdrawn = agent
                                .reconcile_mcp_entry(&withdraw_name, None)
                                .await
                                .map_err(|error| error.to_string());
                            if withdrawn.is_ok() {
                                withdraw_runtime
                                    .clear_published_connect(&withdraw_target)
                                    .await;
                                let disabled_tools = agent.disabled_tool_names();
                                crate::subagent_prompt::refresh_primary_system_prompt(
                                    agent,
                                    &disabled_tools,
                                );
                            }
                            LateWithdrawOutcome::Attempted(withdrawn)
                        })
                    })
                    .await
                {
                    LateWithdrawOutcome::Superseded => tracing::info!(
                        server = %operation_name,
                        "MCP late client was already replaced by a newer operation; nothing withdrawn"
                    ),
                    LateWithdrawOutcome::Attempted(Ok(change)) => tracing::info!(
                        server = %operation_name,
                        ?change,
                        "MCP connection completed after its pass stopped waiting; late client withdrawn"
                    ),
                    LateWithdrawOutcome::Attempted(Err(error)) => tracing::warn!(
                        server = %operation_name,
                        %error,
                        "MCP late client withdrawal failed; the framework retains it as retryable cleanup debt"
                    ),
                }
            }
            runtime.untrack_background_connect(&target, operation).await;
            receipt.settled.send_replace(true);
        });
        Some(ConnectOperation { result, decision })
    }

    /// Observe the background connection operations of one target without
    /// holding the agent lock.
    ///
    /// Returns true only when no operation remains outstanding for this exact
    /// target. The same server name on another agent is a different target and
    /// must neither mask nor be masked by these receipts.
    async fn observe_background_connect(
        self: &Arc<Self>,
        agent: &AgentHandle,
        name: &str,
        deadline: std::time::Duration,
    ) -> bool {
        let target = BackgroundConnectTarget::new(agent, name);
        let receipts = self.background_receipts(&target).await;
        if receipts.is_empty() {
            return true;
        }
        let wait = async {
            for receipt in &receipts {
                receipt.wait_settled().await;
            }
        };
        let settled_in_time = tokio::time::timeout(deadline, wait).await.is_ok();
        // An operation registered after the snapshot is still outstanding, so
        // never report the target converged on a stale snapshot.
        let no_newer_receipt = self.background_receipts(&target).await.is_empty();
        if settled_in_time && no_newer_receipt {
            return true;
        }
        tracing::warn!(
            server = %name,
            timeout_secs = deadline.as_secs_f64(),
            "MCP backgrounded connection is still running; this pass stays unconfirmed"
        );
        false
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

/// Wait for the issuing pass to decide what to do with a late result.
///
/// The pass decides immediately after it stops waiting, so this bound only
/// exists to keep a lost pass (dropped or panicked reconcile task) from parking
/// the operation forever. Withdrawal is the conservative default: a client this
/// generation never confirmed must not stay wired.
async fn wait_for_late_connect_decision(
    mut receiver: tokio::sync::watch::Receiver<Option<LateConnectDecision>>,
    deadline: std::time::Duration,
) -> LateConnectDecision {
    let wait = async {
        loop {
            if let Some(decision) = *receiver.borrow() {
                return decision;
            }
            if receiver.changed().await.is_err() {
                return LateConnectDecision::Withdraw;
            }
        }
    };
    match tokio::time::timeout(deadline, wait).await {
        Ok(decision) => decision,
        Err(_) => {
            tracing::warn!(
                timeout_secs = deadline.as_secs_f64(),
                "MCP connection receipt was never claimed by its pass; withdrawing late publications"
            );
            LateConnectDecision::Withdraw
        }
    }
}

/// Names the desired snapshot expects to be connected. Disabled entries are
/// intentionally excluded: they own no client and need no disconnect retry.
fn enabled_server_names(config: &McpConfigFile) -> Vec<String> {
    config
        .mcp_servers
        .iter()
        .filter(|(_, entry)| !entry.disabled)
        .map(|(name, _)| name.clone())
        .collect()
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

    /// One tool exposed by the fixture MCP server.
    struct FixtureTool {
        name: String,
    }

    impl echo_agent::tools::Tool for FixtureTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "fixture MCP tool"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        fn execute<'a>(
            &'a self,
            _parameters: echo_agent::tools::ToolParameters,
        ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<echo_agent::tools::ToolResult>>
        {
            Box::pin(async { Ok(echo_agent::tools::ToolResult::success("fixture-ok")) })
        }
    }

    /// In-process MCP server exposed over Streamable HTTP.
    ///
    /// Reconcile regressions are about what the framework manager actually
    /// confirms (a live client, a revoked tool projection), so the tests drive
    /// a real transport instead of mocking the seam under test.
    struct HttpMcpFixture {
        endpoint: String,
        requests: Arc<AtomicU64>,
        task: tokio::task::JoinHandle<()>,
    }

    impl HttpMcpFixture {
        async fn start(tool_name: &str) -> anyhow::Result<Self> {
            Self::start_delayed(tool_name, std::time::Duration::ZERO).await
        }

        /// Start a fixture that answers JSON-RPC only after `delay`, which makes
        /// a real framework client preparation outlive a short reconcile
        /// deadline. That is what a stranded preparation ticket would poison.
        async fn start_delayed(
            tool_name: &str,
            delay: std::time::Duration,
        ) -> anyhow::Result<Self> {
            let server = Arc::new(
                echo_agent::mcp::McpServer::builder()
                    .name("fixture")
                    .version("1.0.0")
                    .tool(Arc::new(FixtureTool {
                        name: tool_name.to_string(),
                    }))
                    .build(),
            );
            let requests = Arc::new(AtomicU64::new(0));
            let handler_requests = Arc::clone(&requests);
            let router = axum::Router::new().route(
                "/mcp",
                axum::routing::post(move |body: String| {
                    let server = Arc::clone(&server);
                    let requests = Arc::clone(&handler_requests);
                    async move {
                        requests.fetch_add(1, Ordering::AcqRel);
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        let response = server.handle_json_rpc(&body).await;
                        if response.is_empty() {
                            (axum::http::StatusCode::ACCEPTED, String::new())
                        } else {
                            (axum::http::StatusCode::OK, response)
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let endpoint = format!("http://{}/mcp", listener.local_addr()?);
            let task = tokio::spawn(async move {
                if let Err(error) = axum::serve(listener, router).await {
                    tracing::warn!(%error, "fixture MCP HTTP server stopped");
                }
            });
            Ok(Self {
                endpoint,
                requests,
                task,
            })
        }

        fn entry(&self) -> McpServerEntry {
            McpServerEntry {
                url: Some(self.endpoint.clone()),
                ..Default::default()
            }
        }

        fn request_count(&self) -> u64 {
            self.requests.load(Ordering::Acquire)
        }
    }

    impl Drop for HttpMcpFixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// In-process MCP server whose requests are answered only while its gate is
    /// open.
    ///
    /// Arrival of a request is signaled to the test. Together with the gate
    /// this gives deterministic control over how long a real framework client
    /// preparation stays in flight: while it is blocked the owning agent's
    /// write lock is held, without any sleep-based racing.
    struct GatedMcpFixture {
        endpoint: String,
        gate: tokio::sync::watch::Sender<bool>,
        arrivals: Arc<tokio::sync::Notify>,
        task: tokio::task::JoinHandle<()>,
    }

    impl GatedMcpFixture {
        async fn start(tool_name: &str) -> anyhow::Result<Self> {
            let server = Arc::new(
                echo_agent::mcp::McpServer::builder()
                    .name("fixture")
                    .version("1.0.0")
                    .tool(Arc::new(FixtureTool {
                        name: tool_name.to_string(),
                    }))
                    .build(),
            );
            let arrivals = Arc::new(tokio::sync::Notify::new());
            let handler_arrivals = Arc::clone(&arrivals);
            let (gate, gate_receiver) = tokio::sync::watch::channel(false);
            let router = axum::Router::new().route(
                "/mcp",
                axum::routing::post(move |body: String| {
                    let server = Arc::clone(&server);
                    let arrivals = Arc::clone(&handler_arrivals);
                    let mut open = gate_receiver.clone();
                    async move {
                        arrivals.notify_one();
                        loop {
                            let is_open = *open.borrow();
                            if is_open {
                                break;
                            }
                            if open.changed().await.is_err() {
                                break;
                            }
                        }
                        let response = server.handle_json_rpc(&body).await;
                        if response.is_empty() {
                            (axum::http::StatusCode::ACCEPTED, String::new())
                        } else {
                            (axum::http::StatusCode::OK, response)
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let endpoint = format!("http://{}/mcp", listener.local_addr()?);
            let task = tokio::spawn(async move {
                if let Err(error) = axum::serve(listener, router).await {
                    tracing::warn!(%error, "gated fixture MCP HTTP server stopped");
                }
            });
            Ok(Self {
                endpoint,
                gate,
                arrivals,
                task,
            })
        }

        fn open(&self) {
            self.gate.send_replace(true);
        }

        fn close(&self) {
            self.gate.send_replace(false);
        }

        /// Resolve once a request reached the gate. After this point the
        /// requesting framework client holds its agent's write lock.
        async fn wait_for_arrival(&self) {
            self.arrivals.notified().await;
        }

        fn entry(&self) -> McpServerEntry {
            McpServerEntry {
                url: Some(self.endpoint.clone()),
                ..Default::default()
            }
        }
    }

    impl Drop for GatedMcpFixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// One runtime plus a real agent namespace, mirroring how production wires
    /// a reconcile target.
    struct ReconcileHarness {
        runtime: Arc<McpConfigRuntime>,
        agent: AgentHandle,
        ownership: Arc<McpNameOwnershipRegistry>,
        targets: Vec<McpReconcileTarget>,
        _temp: tempfile::TempDir,
    }

    impl ReconcileHarness {
        fn new(deadlines: ReconcileDeadlines) -> anyhow::Result<Self> {
            let temp = tempfile::tempdir()?;
            let runtime = Arc::new(
                McpConfigRuntime::empty(temp.path().join("mcp.json")).with_deadlines(deadlines),
            );
            let agent = AgentHandle::new(echo_agent::agent::ReactAgent::new(
                echo_agent::agent::AgentConfig::minimal("fixture-model", "fixture-agent"),
            ));
            let ownership = runtime.ownership();
            let targets = vec![McpReconcileTarget::new(
                agent.clone(),
                Arc::clone(&ownership),
                None,
            )];
            Ok(Self {
                runtime,
                agent,
                ownership,
                targets,
                _temp: temp,
            })
        }

        async fn upsert(&self, name: &str, entry: McpServerEntry) -> anyhow::Result<()> {
            self.runtime
                .upsert_and_reconcile(self.targets.clone(), name.to_string(), entry)
                .await?;
            self.await_reconcile().await
        }

        async fn set_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()> {
            self.runtime
                .set_enabled_and_reconcile(self.targets.clone(), name, enabled)
                .await?;
            self.await_reconcile().await
        }

        async fn remove(&self, name: &str) -> anyhow::Result<()> {
            self.runtime
                .remove_and_reconcile(self.targets.clone(), name)
                .await?;
            self.await_reconcile().await
        }

        async fn replace_all(&self, candidate: McpConfigFile) -> anyhow::Result<()> {
            self.runtime
                .replace_and_reconcile(self.targets.clone(), candidate)
                .await?;
            self.await_reconcile().await
        }

        /// Await the exact reconcile generation this mutation handed off, so a
        /// later assertion observes settled state rather than a live pass.
        async fn await_reconcile(&self) -> anyhow::Result<()> {
            let tasks = {
                let mut supervisor = self.runtime.supervisor.lock().await;
                std::mem::take(&mut supervisor.reconcile_tasks)
            };
            tokio::time::timeout(std::time::Duration::from_secs(15), async {
                for task in tasks {
                    task.handle
                        .await
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            .map_err(|_| {
                anyhow::anyhow!("MCP reconcile did not finish within the test window")
            })??;
            Ok(())
        }

        /// Await the newest pass without waiting for an older pass that the
        /// test deliberately paused before operation registration.
        async fn await_latest_reconcile(&self) -> anyhow::Result<()> {
            let task = self
                .runtime
                .supervisor
                .lock()
                .await
                .reconcile_tasks
                .pop()
                .ok_or_else(|| anyhow::anyhow!("no reconcile task was registered"))?;
            tokio::time::timeout(std::time::Duration::from_secs(15), task.handle).await??;
            Ok(())
        }

        async fn pause_connect(
            &self,
            point: ConnectTestPoint,
            generation: u64,
        ) -> (
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<()>,
        ) {
            let (started, started_receiver) = tokio::sync::oneshot::channel();
            let (release, release_receiver) = tokio::sync::oneshot::channel();
            self.runtime
                .connect_test_gates
                .lock()
                .await
                .push(ConnectTestGate {
                    point,
                    generation,
                    started,
                    release: release_receiver,
                });
            (started_receiver, release)
        }

        async fn connected_servers(&self) -> Vec<String> {
            self.agent.read(|agent| agent.list_mcp_servers()).await
        }

        async fn tool_names(&self) -> Vec<String> {
            self.agent.read(|agent| agent.list_tools()).await
        }

        /// Names whose desired state is not confirmed yet. After a converged
        /// pass this is exactly the enabled set; after a failure it also keeps
        /// names that still need a disconnect or a connection retry.
        async fn retry_frontier(&self) -> BTreeSet<String> {
            self.runtime.unreconciled_user_names.lock().await.clone()
        }

        /// Wait until every backgrounded connection operation settled. The
        /// runtime removes its receipt exactly when the framework preparation
        /// reached a terminal state and any late client was withdrawn.
        async fn await_background_connections(&self) -> anyhow::Result<()> {
            tokio::time::timeout(std::time::Duration::from_secs(15), async {
                loop {
                    if self.runtime.background_connect_count().await == 0 {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| {
                anyhow::anyhow!("backgrounded MCP connection did not settle within the test window")
            })?;
            Ok(())
        }

        async fn queue_disconnect_fault(&self, name: &str, fault: ReconcileFault) {
            self.runtime.queue_disconnect_fault(name, fault).await;
        }
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

        assert!(plan.disconnect.is_empty());
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

        assert!(plan.disconnect.is_empty());
        assert_eq!(plan.connect.len(), 1);
        assert_eq!(
            plan.connect.first().map(|(name, _)| name.as_str()),
            Some("user-server")
        );
    }

    #[test]
    fn reconcile_plan_disconnects_names_carried_from_a_canceled_generation() {
        let current = McpConfigFile::default();
        let plan =
            ReconcilePlan::with_tracked_names(["removed-before-reconcile".to_string()], &current);

        assert_eq!(
            plan.disconnect,
            vec!["removed-before-reconcile".to_string()]
        );
        assert!(plan.connect.is_empty());
        assert!(plan.active_names.is_empty());
    }

    #[test]
    fn reconcile_plan_withdraws_a_disabled_server() {
        let previous = config_with("local", stdio_entry("node", false));
        let mut current = config_with("local", stdio_entry("node", false));
        if let Some(entry) = current.mcp_servers.get_mut("local") {
            entry.disabled = true;
        }
        let plan = ReconcilePlan::between(&previous, &current);

        // Disabling is a connection change: the old client must be withdrawn,
        // not merely left connected while `disabled` is persisted.
        assert_eq!(plan.disconnect, vec!["local".to_string()]);
        assert!(plan.connect.is_empty());
        assert!(plan.active_names.is_empty());
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
        let mut app_config = EkoConfig::default();
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

    #[tokio::test]
    async fn disabling_a_server_disconnects_it_and_revokes_its_tools() -> anyhow::Result<()> {
        let fixture = HttpMcpFixture::start("fixture_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines::default())?;

        harness.upsert("local", fixture.entry()).await?;
        assert!(
            harness
                .connected_servers()
                .await
                .contains(&"local".to_string())
        );
        let exposed = echo_agent::mcp::McpToolAdapter::exposed_name_for("local", "fixture_tool");
        assert!(harness.tool_names().await.contains(&exposed));

        harness.set_enabled("local", false).await?;

        assert!(
            !harness
                .connected_servers()
                .await
                .contains(&"local".to_string()),
            "an enabled -> disabled flip must withdraw the live client"
        );
        assert!(
            !harness.tool_names().await.contains(&exposed),
            "a disabled MCP server must no longer expose callable tools"
        );
        // The disabled entry still owns its name: the durable user config
        // declares it, so a plugin must not take the name over.
        assert!(harness.ownership.is_user_owned("local").await);
        // The disconnect was confirmed, so no retry is left pending.
        assert!(!harness.retry_frontier().await.contains("local"));
        Ok(())
    }

    #[tokio::test]
    async fn failed_connection_retains_the_name_until_a_retry_settles() -> anyhow::Result<()> {
        let fixture = HttpMcpFixture::start("fixture_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines::default())?;

        harness
            .upsert("shared", stdio_entry("/nonexistent/eko-mcp-fixture", false))
            .await?;

        assert!(harness.connected_servers().await.is_empty());
        assert!(
            harness.ownership.is_user_owned("shared").await,
            "a failed connection must not release the user namespace claim"
        );
        assert!(
            harness.retry_frontier().await.contains("shared"),
            "an unconfirmed connection must stay in the retry frontier"
        );

        harness.upsert("shared", fixture.entry()).await?;

        assert!(
            harness
                .connected_servers()
                .await
                .contains(&"shared".to_string())
        );
        assert_eq!(
            harness.retry_frontier().await,
            BTreeSet::from(["shared".to_string()]),
            "a confirmed retry settles the frontier to the enabled set"
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_disconnect_keeps_the_name_claimed_until_a_retry_releases_it()
    -> anyhow::Result<()> {
        let fixture = HttpMcpFixture::start("fixture_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines::default())?;
        harness.upsert("shared", fixture.entry()).await?;

        harness
            .queue_disconnect_fault("shared", ReconcileFault::Fail)
            .await;
        harness.remove("shared").await?;

        assert!(
            harness.ownership.is_user_owned("shared").await,
            "an unconfirmed disconnect must keep the name out of plugin reach"
        );
        assert!(harness.retry_frontier().await.contains("shared"));
        {
            let mut guard = harness.ownership.lock().await;
            assert!(guard.claim_plugin("fixture-plugin", "shared").is_err());
        }
        // The disconnect never reached the framework, so the client is live.
        assert!(
            harness
                .connected_servers()
                .await
                .contains(&"shared".to_string())
        );

        // The next generation retries the pending disconnect and settles.
        harness.replace_all(McpConfigFile::default()).await?;

        assert!(
            !harness
                .connected_servers()
                .await
                .contains(&"shared".to_string())
        );
        assert!(!harness.ownership.is_user_owned("shared").await);
        let mut guard = harness.ownership.lock().await;
        assert!(guard.claim_plugin("fixture-plugin", "shared").is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn timed_out_disconnect_keeps_the_name_claimed_until_a_retry_releases_it()
    -> anyhow::Result<()> {
        let fixture = HttpMcpFixture::start("fixture_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines {
            connect: std::time::Duration::from_secs(10),
            disconnect: std::time::Duration::from_millis(50),
        })?;
        harness.upsert("shared", fixture.entry()).await?;

        harness
            .queue_disconnect_fault("shared", ReconcileFault::Hang)
            .await;
        harness.remove("shared").await?;

        assert!(harness.ownership.is_user_owned("shared").await);
        assert!(harness.retry_frontier().await.contains("shared"));
        assert!(
            harness
                .connected_servers()
                .await
                .contains(&"shared".to_string())
        );

        harness.replace_all(McpConfigFile::default()).await?;

        assert!(!harness.ownership.is_user_owned("shared").await);
        assert!(!harness.retry_frontier().await.contains("shared"));
        Ok(())
    }

    #[tokio::test]
    async fn timed_out_connection_settles_its_preparation_and_reconnects() -> anyhow::Result<()> {
        // The first server answers initialize only after the reconcile
        // deadline, so a real framework client preparation is still in flight
        // when the pass stops waiting. Dropping it there would strand the
        // manager's `preparing` ticket for this name.
        let slow =
            HttpMcpFixture::start_delayed("fixture_tool", std::time::Duration::from_millis(600))
                .await?;
        let fast = HttpMcpFixture::start("fixture_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines {
            connect: std::time::Duration::from_millis(150),
            disconnect: std::time::Duration::from_secs(10),
        })?;

        harness.upsert("slow", slow.entry()).await?;

        // The pass reported the deadline, so nothing is confirmed yet.
        assert!(harness.ownership.is_user_owned("slow").await);
        assert!(harness.retry_frontier().await.contains("slow"));

        // The background task keeps the preparation alive until the slow
        // server answers, then withdraws the late client instead of letting it
        // join the generation that never confirmed it.
        harness.await_background_connections().await?;
        assert!(
            !harness
                .connected_servers()
                .await
                .contains(&"slow".to_string()),
            "a connection that completed after its pass timed out must be withdrawn"
        );

        // The retry must reach the framework. A stranded preparation would make
        // it fail with "another preparation is still active", which would leave
        // the name permanently unusable.
        harness.upsert("slow", fast.entry()).await?;
        assert!(
            harness
                .connected_servers()
                .await
                .contains(&"slow".to_string()),
            "the generation after a timed out connection must be able to connect the same name"
        );
        let exposed = echo_agent::mcp::McpToolAdapter::exposed_name_for("slow", "fixture_tool");
        assert!(harness.tool_names().await.contains(&exposed));
        assert_eq!(
            harness.retry_frontier().await,
            BTreeSet::from(["slow".to_string()])
        );
        Ok(())
    }

    #[tokio::test]
    async fn stale_generation_cannot_register_after_new_generation_confirmed() -> anyhow::Result<()>
    {
        let stale = HttpMcpFixture::start("stale_tool").await?;
        let current = HttpMcpFixture::start("current_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines::default())?;
        let stale_generation = harness.runtime.generation().saturating_add(1);
        let (started, release) = harness
            .pause_connect(ConnectTestPoint::BeforeAdmission, stale_generation)
            .await;

        harness
            .runtime
            .upsert_and_reconcile(harness.targets.clone(), "shared".to_string(), stale.entry())
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), started).await??;

        // G1 passed its first validity check but has not registered a receipt.
        // A real G2 mutation can therefore finish its complete reconcile pass.
        harness
            .runtime
            .upsert_and_reconcile(
                harness.targets.clone(),
                "shared".to_string(),
                current.entry(),
            )
            .await?;
        harness.await_latest_reconcile().await?;
        let confirmed = harness
            .agent
            .read(|agent| agent.mcp_client("shared"))
            .await
            .ok_or_else(|| anyhow::anyhow!("G2 did not confirm its client"))?;
        release
            .send(())
            .map_err(|_| anyhow::anyhow!("stale admission gate was dropped"))?;
        harness.await_reconcile().await?;
        harness.await_background_connections().await?;

        let live = harness.agent.read(|agent| agent.mcp_client("shared")).await;
        assert!(
            live.as_ref()
                .is_some_and(|client| Arc::ptr_eq(client, &confirmed)),
            "G1 must not replace or withdraw a client G2 already confirmed"
        );
        assert_eq!(
            stale.request_count(),
            0,
            "an operation superseded before registration must never start its transport"
        );
        let exposed = echo_agent::mcp::McpToolAdapter::exposed_name_for("shared", "current_tool");
        assert!(harness.tool_names().await.contains(&exposed));
        Ok(())
    }

    #[tokio::test]
    async fn stale_generation_cannot_start_preparation_after_waiting_for_agent_lock()
    -> anyhow::Result<()> {
        let stale = HttpMcpFixture::start("stale_tool").await?;
        let current = HttpMcpFixture::start("current_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines::default())?;
        let stale_generation = harness.runtime.generation().saturating_add(1);
        let (started, release) = harness
            .pause_connect(ConnectTestPoint::BeforeFirstPoll, stale_generation)
            .await;

        harness
            .runtime
            .upsert_and_reconcile(harness.targets.clone(), "shared".to_string(), stale.entry())
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), started).await??;
        // G1 has a tracked receipt and its Agent write lock, but no framework
        // preparation has been polled. G2 durably invalidates it at this seam.
        harness
            .runtime
            .upsert_and_reconcile(
                harness.targets.clone(),
                "shared".to_string(),
                current.entry(),
            )
            .await?;
        release
            .send(())
            .map_err(|_| anyhow::anyhow!("stale preparation gate was dropped"))?;
        harness.await_reconcile().await?;
        harness.await_background_connections().await?;

        assert_eq!(
            stale.request_count(),
            0,
            "taking the Agent lock must not authorize an already superseded preparation"
        );
        let exposed = echo_agent::mcp::McpToolAdapter::exposed_name_for("shared", "current_tool");
        assert!(harness.tool_names().await.contains(&exposed));
        Ok(())
    }

    #[tokio::test]
    async fn unchanged_confirmation_transfers_the_live_client_before_old_withdrawal()
    -> anyhow::Result<()> {
        let fixture = HttpMcpFixture::start("fixture_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines::default())?;
        // Keep the first real background operation's decision pending so the
        // second operation can confirm the same physical client as Unchanged.
        let first = harness
            .runtime
            .spawn_connect_operation(
                &harness.agent,
                "shared",
                fixture.entry(),
                harness.runtime.generation(),
                CancellationToken::new(),
            )
            .await
            .ok_or_else(|| anyhow::anyhow!("first operation was not admitted"))?;
        let first_outcome =
            tokio::time::timeout(std::time::Duration::from_secs(5), first.result).await??;
        assert_eq!(
            first_outcome,
            ConnectOperationOutcome::Completed(echo_agent::mcp::McpTargetChange::Connected)
        );
        let confirmed = harness
            .agent
            .read(|agent| agent.mcp_client("shared"))
            .await
            .ok_or_else(|| anyhow::anyhow!("first operation did not publish its client"))?;

        let second = harness
            .runtime
            .spawn_connect_operation(
                &harness.agent,
                "shared",
                fixture.entry(),
                harness.runtime.generation(),
                CancellationToken::new(),
            )
            .await
            .ok_or_else(|| anyhow::anyhow!("second operation was not admitted"))?;
        let second_outcome =
            tokio::time::timeout(std::time::Duration::from_secs(5), second.result).await??;
        assert_eq!(
            second_outcome,
            ConnectOperationOutcome::Completed(echo_agent::mcp::McpTargetChange::Unchanged)
        );
        second
            .decision
            .send(Some(LateConnectDecision::Keep))
            .map_err(|_| anyhow::anyhow!("unchanged confirmation lost its settlement owner"))?;
        first
            .decision
            .send(Some(LateConnectDecision::Withdraw))
            .map_err(|_| anyhow::anyhow!("old withdrawal lost its settlement owner"))?;
        harness.await_background_connections().await?;

        let live = harness.agent.read(|agent| agent.mcp_client("shared")).await;
        assert!(
            live.as_ref()
                .is_some_and(|client| Arc::ptr_eq(client, &confirmed)),
            "the old operation must not withdraw a client the next operation confirmed unchanged"
        );
        let exposed = echo_agent::mcp::McpToolAdapter::exposed_name_for("shared", "fixture_tool");
        assert!(harness.tool_names().await.contains(&exposed));
        Ok(())
    }

    #[tokio::test]
    async fn pending_commit_does_not_keep_agent_locked_while_waiting_for_admission()
    -> anyhow::Result<()> {
        let stale = GatedMcpFixture::start("stale_tool").await?;
        let current = HttpMcpFixture::start("current_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines::default())?;
        let stale_generation = harness.runtime.generation().saturating_add(1);
        let (started, release) = harness
            .pause_connect(ConnectTestPoint::BeforeFirstPoll, stale_generation)
            .await;
        harness
            .runtime
            .upsert_and_reconcile(harness.targets.clone(), "shared".to_string(), stale.entry())
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), started).await??;

        // Plugin cleanup may retain namespace ownership while asking for the
        // Agent. A real config commit now owns mutation_lock and waits for
        // that namespace, reproducing the other two edges of the lock cycle.
        let ownership = harness.ownership.lock().await;
        let runtime = Arc::clone(&harness.runtime);
        let targets = harness.targets.clone();
        let entry = current.entry();
        let mutation = tokio::spawn(async move {
            runtime
                .upsert_and_reconcile(targets, "shared".to_string(), entry)
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if harness.runtime.mutation_lock.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        release
            .send(())
            .map_err(|_| anyhow::anyhow!("lock-order preparation gate was dropped"))?;

        let agent = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            harness.agent.inner().write(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("pending admission retained the Agent lock"))?;
        // The owner that needed the Agent can now finish and release the
        // namespace. G2 commits, supersedes G1, and uses the freed Agent.
        drop(ownership);
        tokio::time::timeout(std::time::Duration::from_secs(5), mutation).await???;
        drop(agent);
        harness.await_reconcile().await?;
        harness.await_background_connections().await?;
        let exposed = echo_agent::mcp::McpToolAdapter::exposed_name_for("shared", "current_tool");
        assert!(harness.tool_names().await.contains(&exposed));
        Ok(())
    }

    /// One `McpConfigRuntime` is shared by every workspace agent, so the same
    /// server name can be connected concurrently on several agents. A fast
    /// target settling must not take the slow target's receipt out of the
    /// runtime's bookkeeping, and the slow target's late withdrawal must not
    /// touch the connection the fast target already confirmed.
    #[tokio::test]
    async fn same_server_name_on_two_agents_keeps_an_independent_background_receipt()
    -> anyhow::Result<()> {
        let gated = GatedMcpFixture::start("fixture_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines {
            connect: std::time::Duration::from_millis(150),
            disconnect: std::time::Duration::from_secs(5),
        })?;
        let second_agent = AgentHandle::new(echo_agent::agent::ReactAgent::new(
            echo_agent::agent::AgentConfig::minimal("fixture-model", "second-agent"),
        ));

        // The second agent starts already connected to the exact entry the
        // shared pass hands to both targets, so its reconcile is a framework
        // `Unchanged` no-op that settles immediately.
        gated.open();
        let pre_connected = second_agent
            .write_async({
                let entry = gated.entry();
                move |agent| {
                    Box::pin(async move { agent.reconcile_mcp_entry("shared", Some(entry)).await })
                }
            })
            .await?;
        assert_eq!(pre_connected, echo_agent::mcp::McpTargetChange::Connected);
        // From here on the slow target's preparation blocks inside the
        // framework client and holds that agent's write lock.
        gated.close();

        let plan = ReconcilePlan::between(
            &McpConfigFile::default(),
            &config_with("shared", gated.entry()),
        );
        let generation = harness.runtime.generation();
        let slow = McpConfigRuntime::reconcile_target(
            harness.agent.clone(),
            Arc::clone(&harness.runtime),
            generation,
            CancellationToken::new(),
            plan.clone(),
        )
        .await;
        assert!(
            !slow,
            "a gated connection must report its target unconfirmed"
        );
        gated.wait_for_arrival().await;
        // The slow target's agent write lock is held by its preparation, so
        // reading its namespace here would wait for that lock; the publication
        // state is asserted after the operation settles instead.

        let fast = McpConfigRuntime::reconcile_target(
            second_agent.clone(),
            Arc::clone(&harness.runtime),
            generation,
            CancellationToken::new(),
            plan.clone(),
        )
        .await;
        assert!(fast, "the already-connected target reconciles immediately");

        // A retry for the slow target must still see its own in-flight
        // operation. If the fast target's receipt replaced it, phase 1 would
        // report the target settled and the pass would then wait for the agent
        // write lock that the slow operation holds.
        let retry = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            McpConfigRuntime::reconcile_target(
                harness.agent.clone(),
                Arc::clone(&harness.runtime),
                generation,
                CancellationToken::new(),
                plan.clone(),
            ),
        )
        .await
        .expect(
            "the slow target's receipt must survive the fast target's settlement, \
             so the retry does not wait for the agent write lock",
        );
        assert!(
            !retry,
            "a retry that observes an unsettled connection must report the target unconfirmed"
        );
        assert!(
            harness.runtime.background_connect_count().await >= 1,
            "the slow target's in-flight operation must still be tracked"
        );

        // Let the slow preparation finish: it publishes late, its pass already
        // reported the target unconfirmed, so the operation withdraws its own
        // client again.
        gated.open();
        harness.await_background_connections().await?;
        assert!(
            !harness
                .connected_servers()
                .await
                .contains(&"shared".to_string()),
            "a connection published after its own pass stopped waiting must be withdrawn"
        );
        assert!(
            second_agent
                .read(|agent| agent.list_mcp_servers())
                .await
                .contains(&"shared".to_string()),
            "the slow target's late withdrawal must not disconnect the settled target's connection"
        );
        Ok(())
    }

    /// A pass that cannot confirm an earlier background connection must not
    /// then block on the agent write lock which that connection holds. It has
    /// to return in bounded time and report the target unconfirmed.
    #[tokio::test]
    async fn unsettled_background_connection_does_not_block_the_retry_on_the_agent_lock()
    -> anyhow::Result<()> {
        let gated = GatedMcpFixture::start("fixture_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines {
            connect: std::time::Duration::from_millis(150),
            disconnect: std::time::Duration::from_secs(5),
        })?;
        let plan = ReconcilePlan::between(
            &McpConfigFile::default(),
            &config_with("blocked", gated.entry()),
        );
        let generation = harness.runtime.generation();
        let first = McpConfigRuntime::reconcile_target(
            harness.agent.clone(),
            Arc::clone(&harness.runtime),
            generation,
            CancellationToken::new(),
            plan.clone(),
        )
        .await;
        assert!(!first);
        // The background task now owns the agent write lock.
        gated.wait_for_arrival().await;

        let retry = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            McpConfigRuntime::reconcile_target(
                harness.agent.clone(),
                Arc::clone(&harness.runtime),
                generation,
                CancellationToken::new(),
                plan.clone(),
            ),
        )
        .await
        .expect("a pass that observes an unsettled connection must not wait unboundedly");
        assert!(
            !retry,
            "the retry must report the target unconfirmed instead of claiming settlement"
        );
        Ok(())
    }

    /// Even when phase 1 converges, the write lock can be owned by another
    /// target server's connection of the same agent. Acquiring it must be
    /// bounded and cancellable, and shutdown must not wait for it forever.
    #[tokio::test]
    async fn reconcile_lock_and_shutdown_stay_bounded_while_a_connection_holds_the_lock()
    -> anyhow::Result<()> {
        let gated = GatedMcpFixture::start("fixture_tool").await?;
        let harness = ReconcileHarness::new(ReconcileDeadlines {
            connect: std::time::Duration::from_millis(150),
            disconnect: std::time::Duration::from_secs(5),
        })?;
        let blocking_plan = ReconcilePlan::between(
            &McpConfigFile::default(),
            &config_with("blocking", gated.entry()),
        );
        let generation = harness.runtime.generation();
        let first = McpConfigRuntime::reconcile_target(
            harness.agent.clone(),
            Arc::clone(&harness.runtime),
            generation,
            CancellationToken::new(),
            blocking_plan,
        )
        .await;
        assert!(!first);
        gated.wait_for_arrival().await;

        // A different name means phase 1 converges; only the lock can block.
        let unrelated_plan = ReconcilePlan::between(
            &config_with("unrelated", gated.entry()),
            &McpConfigFile::default(),
        );
        let converged = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            McpConfigRuntime::reconcile_target(
                harness.agent.clone(),
                Arc::clone(&harness.runtime),
                generation,
                CancellationToken::new(),
                unrelated_plan,
            ),
        )
        .await
        .expect("taking the agent write lock must be bounded");
        assert!(
            !converged,
            "a pass that cannot take the agent write lock must report unconfirmed"
        );

        // The same lock is held while a registered pass waits for it; shutdown
        // cancels and awaits that pass. An empty plan has no per-name cancel
        // check before the lock, so the pass deterministically reaches the
        // lock acquisition and only shutdown's cancellation can release it.
        let runtime = Arc::clone(&harness.runtime);
        let task_agent = harness.agent.clone();
        let task_runtime = Arc::clone(&harness.runtime);
        let task_cancel = CancellationToken::new();
        let task_plan =
            ReconcilePlan::between(&McpConfigFile::default(), &McpConfigFile::default());
        let (started, started_rx) = tokio::sync::oneshot::channel();
        runtime
            .start_tracked_reconcile(task_cancel.clone(), {
                let cancel = task_cancel.clone();
                async move {
                    let _ = started.send(());
                    McpConfigRuntime::reconcile_target(
                        task_agent,
                        task_runtime,
                        generation,
                        cancel,
                        task_plan,
                    )
                    .await;
                }
            })
            .await;
        started_rx
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        tokio::time::timeout(std::time::Duration::from_secs(3), runtime.shutdown())
            .await
            .expect("shutdown must not wait unboundedly for the agent write lock");
        Ok(())
    }
}
