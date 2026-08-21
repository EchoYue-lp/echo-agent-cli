//! Workspace-scoped runtime hosts and their immutable file resources.
//!
//! Workspace roots, stores, and execution authorities are immutable per host.
//! `AppState` may change UI focus without rebinding or stopping another host.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent::memory::{ConversationStore, FileConversationStore, FileStore, Store};
use echo_agent::state::{FileRuntimeStateStore, RuntimeStateStore};
use tokio::sync::{Mutex, OnceCell, RwLock};

use super::layout::WorkspaceLayout;
use super::{Workspace, WorkspaceExecutionScope, WorkspaceId};
use crate::agent_pool::{AgentPool, WorkspaceAgentPoolResources};
use crate::conversation_deletion::ConversationDeletionService;
use crate::evolution::ReviewIntegration;
use crate::tasks::task_runtime::TaskRuntimeStore;

/// One coherently prepared set of workspace-scoped runtime resources.
///
/// The roots and stores are immutable after construction. Runtime publication
/// consumes these resources through their owning [`WorkspaceRuntimeHost`].
pub(crate) struct WorkspaceRuntimeResources {
    workspace: Workspace,
    state_dir: PathBuf,
    tasks_dir: PathBuf,
    conversation_store: Arc<dyn ConversationStore>,
    runtime_state_store: Arc<dyn RuntimeStateStore>,
    memory_store: Arc<dyn Store>,
    deletion_service: Arc<ConversationDeletionService>,
}

/// Stable application-layer owner for one workspace's file resources.
///
/// The workspace ID and canonical root never change. Display metadata can be
/// refreshed after registry operations such as linking a project without
/// replacing the host or reopening its stores.
pub(crate) struct WorkspaceRuntimeHost {
    workspace: RwLock<Workspace>,
    resources: WorkspaceRuntimeResources,
    execution: OnceCell<Arc<WorkspaceExecutionRuntime>>,
}

/// Workspace-owned execution authorities used by foreground and background
/// turns after focus has moved elsewhere.
pub(crate) struct WorkspaceExecutionRuntime {
    primary_agent: crate::agent_handle::AgentHandle,
    pool: Arc<AgentPool>,
    task_runtime: Arc<TaskRuntimeStore>,
    review_integration: Arc<ReviewIntegration>,
    plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
    mcp_ownership: Arc<crate::mcp_config_runtime::McpNameOwnershipRegistry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceRuntimeActivity {
    pub workspace_id: WorkspaceId,
    pub execution_loaded: bool,
    pub active_pool_executions: usize,
    pub active_run_drivers: usize,
    pub active_run_driver_receipts: usize,
}

impl WorkspaceRuntimeActivity {
    pub(crate) fn is_idle(&self) -> bool {
        self.active_pool_executions == 0
            && self.active_run_drivers == 0
            && self.active_run_driver_receipts == 0
    }
}

/// Sole process-level owner for loaded workspace hosts.
///
/// Host creation is serialized so concurrent opens of the same workspace
/// cannot build two independent in-process store owners. Loaded hosts remain
/// resident until application shutdown; eviction requires an explicit idle
/// proof and is intentionally deferred.
#[derive(Default)]
pub(crate) struct WorkspaceRuntimeRegistry {
    hosts: Mutex<HashMap<WorkspaceId, Arc<WorkspaceRuntimeHost>>>,
}

impl WorkspaceRuntimeResources {
    /// Validate a workspace root and open every file-backed store needed by a
    /// focused workspace generation before any live Agent binding is changed.
    pub(crate) async fn prepare(mut workspace: Workspace) -> anyhow::Result<Self> {
        let root = validated_workspace_root(&workspace.root)?;
        WorkspaceLayout::ensure_dirs(&root).map_err(|error| {
            anyhow::anyhow!(
                "Failed to prepare workspace layout at {}: {error}",
                root.display()
            )
        })?;

        let state_dir = WorkspaceLayout::state_dir(&root);
        let sessions_dir = WorkspaceLayout::sessions(&root);
        let tasks_dir = WorkspaceLayout::tasks(&root);
        let conversation_store: Arc<dyn ConversationStore> =
            Arc::new(FileConversationStore::new(&state_dir).map_err(|error| {
                anyhow::anyhow!(
                    "Failed to prepare workspace conversation store at {}: {error}",
                    state_dir.display()
                )
            })?);
        let runtime_state_store: Arc<dyn RuntimeStateStore> =
            Arc::new(FileRuntimeStateStore::new(&sessions_dir).map_err(|error| {
                anyhow::anyhow!(
                    "Failed to prepare workspace runtime state store at {}: {error}",
                    sessions_dir.display()
                )
            })?);
        let memory_path = WorkspaceLayout::memory_store(&root);
        let memory_store: Arc<dyn Store> =
            Arc::new(FileStore::new(&memory_path).map_err(|error| {
                anyhow::anyhow!(
                    "Failed to prepare workspace memory store at {}: {error}",
                    memory_path.display()
                )
            })?);
        let deletion_service = Arc::new(ConversationDeletionService::new(
            state_dir.join("conversation-deletions"),
        ));
        if let Err(error) = deletion_service
            .recover_committed_deletions(conversation_store.as_ref())
            .await
        {
            tracing::warn!(
                workspace = %workspace.id,
                %error,
                "workspace conversation deletion recovery remains pending"
            );
        }

        workspace.root = root;
        Ok(Self {
            workspace,
            state_dir,
            tasks_dir,
            conversation_store,
            runtime_state_store,
            memory_store,
            deletion_service,
        })
    }

    pub(crate) fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub(crate) fn root(&self) -> &Path {
        &self.workspace.root
    }

    pub(crate) fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub(crate) fn tasks_dir(&self) -> &Path {
        &self.tasks_dir
    }

    pub(crate) fn conversation_store(&self) -> Arc<dyn ConversationStore> {
        self.conversation_store.clone()
    }

    pub(crate) fn runtime_state_store(&self) -> Arc<dyn RuntimeStateStore> {
        self.runtime_state_store.clone()
    }

    pub(crate) fn memory_store(&self) -> Arc<dyn Store> {
        self.memory_store.clone()
    }

    pub(crate) fn deletion_service(&self) -> Arc<ConversationDeletionService> {
        self.deletion_service.clone()
    }
}

impl WorkspaceRuntimeHost {
    async fn open(workspace: Workspace) -> anyhow::Result<Arc<Self>> {
        let resources = WorkspaceRuntimeResources::prepare(workspace).await?;
        let workspace = resources.workspace().clone();
        Ok(Arc::new(Self {
            workspace: RwLock::new(workspace),
            resources,
            execution: OnceCell::new(),
        }))
    }

    pub(crate) async fn workspace(&self) -> Workspace {
        self.workspace.read().await.clone()
    }

    pub(crate) fn id(&self) -> &WorkspaceId {
        &self.resources.workspace().id
    }

    pub(crate) fn root(&self) -> &Path {
        self.resources.root()
    }

    pub(crate) fn resources(&self) -> &WorkspaceRuntimeResources {
        &self.resources
    }

    pub(crate) fn execution_scope(&self) -> WorkspaceExecutionScope {
        WorkspaceExecutionScope::workspace(self.id(), self.root())
    }

    /// Lazily build the one execution generation owned by this host.
    ///
    /// `seed_pool` supplies process-safe model/plugin/tool primitives. All
    /// workspace-bearing stores and task tools are replaced by host resources
    /// before the pool can admit its first conversation.
    pub(crate) async fn get_or_open_execution(
        &self,
        seed_pool: &Arc<AgentPool>,
    ) -> anyhow::Result<Arc<WorkspaceExecutionRuntime>> {
        let runtime = self
            .execution
            .get_or_try_init(|| async {
                let task_runtime = Arc::new(TaskRuntimeStore::open_for_workspace(
                    self.resources.tasks_dir(),
                    self.id().to_string(),
                )?);
                match task_runtime.recover_incomplete() {
                    Ok(recovered) if recovered > 0 => tracing::info!(
                        workspace = %self.id(),
                        recovered,
                        "Recovered interrupted workspace TaskRuns"
                    ),
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        workspace = %self.id(),
                        %error,
                        "Failed to recover interrupted workspace TaskRuns"
                    ),
                }
                let review_integration = Arc::new(ReviewIntegration::new(
                    echo_agent::evolution::ReviewConfig::default(),
                    self.resources.state_dir().to_path_buf(),
                    self.resources.memory_store(),
                ));
                let workspace = self.workspace().await;
                let (pool, plugin_runtime, mcp_ownership) = seed_pool
                    .fork_for_workspace(WorkspaceAgentPoolResources {
                        root: self.root().to_path_buf(),
                        kind: workspace.kind,
                        conversation_store: self.resources.conversation_store(),
                        state_store: self.resources.runtime_state_store(),
                        memory_store: self.resources.memory_store(),
                        task_runtime_store: task_runtime.clone(),
                        review_integration: review_integration.clone(),
                    })
                    .await?;
                let primary_agent = pool.primary_agent().await?;
                review_integration.bind_rule_projection_primary(primary_agent.clone());
                review_integration.bind_rule_projection_pool(&pool).await?;
                Ok::<Arc<WorkspaceExecutionRuntime>, anyhow::Error>(Arc::new(
                    WorkspaceExecutionRuntime {
                        primary_agent,
                        pool,
                        task_runtime,
                        review_integration,
                        plugin_runtime,
                        mcp_ownership,
                    },
                ))
            })
            .await?;
        Ok(Arc::clone(runtime))
    }

    pub(crate) async fn refresh_workspace(&self, mut workspace: Workspace) -> anyhow::Result<()> {
        if workspace.id != *self.id() {
            anyhow::bail!(
                "Workspace host identity mismatch: expected {}, received {}",
                self.id(),
                workspace.id
            );
        }
        let root = validated_workspace_root(&workspace.root)?;
        if root != self.root() {
            anyhow::bail!(
                "Workspace '{}' is already loaded from {}; refusing root change to {}",
                self.id(),
                self.root().display(),
                root.display()
            );
        }
        workspace.root = root;
        *self.workspace.write().await = workspace;
        Ok(())
    }
}

impl WorkspaceExecutionRuntime {
    pub(crate) fn primary_agent(&self) -> crate::agent_handle::AgentHandle {
        self.primary_agent.clone()
    }

    pub(crate) fn pool(&self) -> Arc<AgentPool> {
        Arc::clone(&self.pool)
    }

    pub(crate) fn task_runtime(&self) -> Arc<TaskRuntimeStore> {
        Arc::clone(&self.task_runtime)
    }

    pub(crate) fn review_integration(&self) -> Arc<ReviewIntegration> {
        Arc::clone(&self.review_integration)
    }

    pub(crate) fn plugin_runtime(
        &self,
    ) -> Option<Arc<crate::plugin_runtime::PluginRuntimeService>> {
        self.plugin_runtime.clone()
    }

    pub(crate) fn mcp_reconcile_target(&self) -> crate::mcp_config_runtime::McpReconcileTarget {
        crate::mcp_config_runtime::McpReconcileTarget::new(
            self.primary_agent(),
            Arc::clone(&self.mcp_ownership),
            Some(self.pool()),
        )
    }

    pub(crate) fn activity(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<WorkspaceRuntimeActivity> {
        Ok(WorkspaceRuntimeActivity {
            workspace_id,
            execution_loaded: true,
            active_pool_executions: self.pool.active_execution_count(),
            active_run_drivers: self
                .task_runtime
                .active_run_driver_count()
                .map_err(anyhow::Error::msg)?,
            active_run_driver_receipts: self
                .task_runtime
                .active_run_driver_receipt_count()
                .map_err(anyhow::Error::msg)?,
        })
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let mut errors = Vec::new();
        if let Err(error) = self.task_runtime.shutdown_run_drivers().await {
            errors.push(format!("TaskRun drivers: {error}"));
        }
        if let Err(error) = self.review_integration.shutdown_background_reviews().await {
            errors.push(format!("memory review: {error}"));
        }
        if let Err(error) = self.pool.shutdown().await {
            errors.push(format!("AgentPool: {error}"));
        }
        if let Some(plugin_runtime) = self.plugin_runtime.as_ref()
            && let Err(error) = plugin_runtime.shutdown().await
        {
            errors.push(format!("plugin runtime: {error}"));
        }
        if let Err(error) = self.task_runtime.shutdown_hook_events().await {
            errors.push(format!("task hooks: {error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("; ")))
        }
    }
}

impl WorkspaceRuntimeRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Return the one loaded host for a workspace, opening it on first use.
    pub(crate) async fn get_or_open(
        &self,
        workspace: Workspace,
    ) -> anyhow::Result<Arc<WorkspaceRuntimeHost>> {
        let workspace_id = workspace.id.clone();
        let mut hosts = self.hosts.lock().await;
        if let Some(host) = hosts.get(&workspace_id) {
            host.refresh_workspace(workspace).await?;
            return Ok(Arc::clone(host));
        }

        let host = WorkspaceRuntimeHost::open(workspace).await?;
        hosts.insert(workspace_id, Arc::clone(&host));
        Ok(host)
    }

    /// Stable, sorted snapshot of every initialized host generation. The map
    /// lock is released before callers await any runtime publication.
    pub(crate) async fn loaded_execution_runtimes(
        &self,
    ) -> Vec<(WorkspaceId, Arc<WorkspaceExecutionRuntime>)> {
        let hosts = self.hosts.lock().await;
        let mut runtimes = hosts
            .values()
            .filter_map(|host| {
                host.execution
                    .get()
                    .map(|runtime| (host.id().clone(), Arc::clone(runtime)))
            })
            .collect::<Vec<_>>();
        runtimes.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        runtimes
    }

    pub(crate) async fn activity_snapshot(&self) -> anyhow::Result<Vec<WorkspaceRuntimeActivity>> {
        let hosts = self.hosts.lock().await;
        let mut activity = Vec::with_capacity(hosts.len());
        for host in hosts.values() {
            match host.execution.get() {
                Some(runtime) => activity.push(runtime.activity(host.id().clone())?),
                None => activity.push(WorkspaceRuntimeActivity {
                    workspace_id: host.id().clone(),
                    execution_loaded: false,
                    active_pool_executions: 0,
                    active_run_drivers: 0,
                    active_run_driver_receipts: 0,
                }),
            }
        }
        activity.sort_by(|left, right| left.workspace_id.as_str().cmp(right.workspace_id.as_str()));
        Ok(activity)
    }

    pub(crate) async fn shutdown(&self) -> anyhow::Result<()> {
        let hosts = self
            .hosts
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut errors = Vec::new();
        for host in hosts {
            if let Some(runtime) = host.execution.get()
                && let Err(error) = runtime.shutdown().await
            {
                errors.push(format!("workspace {}: {error}", host.id()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("; ")))
        }
    }

    #[cfg(test)]
    async fn host_count(&self) -> usize {
        self.hosts.lock().await.len()
    }
}

fn validated_workspace_root(root: &Path) -> anyhow::Result<PathBuf> {
    let root = root.canonicalize().map_err(|error| {
        anyhow::anyhow!(
            "Workspace root is missing or cannot be resolved ({}): {error}",
            root.display()
        )
    })?;
    if !root.is_dir() {
        anyhow::bail!("Workspace root is not a directory: {}", root.display());
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use echo_agent::memory::NewConversation;
    use echo_agent::testing::MockLlmClient;

    use super::*;
    use crate::agent_handle::AgentHandle;
    use crate::workspace::{WorkspaceId, WorkspaceKind, WorkspaceMetadata};

    fn workspace(name: &str, root: PathBuf) -> Workspace {
        Workspace {
            id: WorkspaceId::from_name(name),
            name: name.to_string(),
            root,
            project_root: None,
            kind: WorkspaceKind::General,
            metadata: WorkspaceMetadata::default(),
            created_at: Utc::now(),
            last_active: Utc::now(),
        }
    }

    #[tokio::test]
    async fn prepare_rejects_missing_and_non_directory_roots() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let file = temp.path().join("workspace-file");
        std::fs::write(&file, "not a directory").map_err(|error| error.to_string())?;

        assert!(
            WorkspaceRuntimeResources::prepare(workspace("missing", temp.path().join("missing")))
                .await
                .is_err()
        );
        assert!(
            WorkspaceRuntimeResources::prepare(workspace("file", file))
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn prepare_builds_canonical_workspace_layout() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;

        let resources = WorkspaceRuntimeResources::prepare(workspace("alpha", root.clone()))
            .await
            .map_err(|error| error.to_string())?;
        let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;

        assert_eq!(resources.root(), canonical_root);
        assert_eq!(
            resources.state_dir(),
            WorkspaceLayout::state_dir(&canonical_root)
        );
        assert!(WorkspaceLayout::sessions(&canonical_root).is_dir());
        assert_eq!(
            resources.tasks_dir(),
            WorkspaceLayout::tasks(&canonical_root)
        );
        assert!(WorkspaceLayout::conversations(&canonical_root).is_dir());
        assert!(WorkspaceLayout::memory(&canonical_root).is_dir());
        Ok(())
    }

    #[tokio::test]
    async fn independent_resources_do_not_share_conversations() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root_a = temp.path().join("a");
        let root_b = temp.path().join("b");
        std::fs::create_dir_all(&root_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&root_b).map_err(|error| error.to_string())?;

        let resources_a = WorkspaceRuntimeResources::prepare(workspace("a", root_a))
            .await
            .map_err(|error| error.to_string())?;
        let resources_b = WorkspaceRuntimeResources::prepare(workspace("b", root_b))
            .await
            .map_err(|error| error.to_string())?;
        let conversation_id = "shared-conversation-id";
        resources_a
            .conversation_store()
            .ensure_conversation(NewConversation {
                conversation_id: conversation_id.to_string(),
                user_id: "default".to_string(),
                agent_type: None,
                title: Some("Workspace A".to_string()),
            })
            .await
            .map_err(|error| error.to_string())?;

        assert!(
            resources_a
                .conversation_store()
                .get_conversation(conversation_id)
                .await
                .map_err(|error| error.to_string())?
                .is_some()
        );
        assert!(
            resources_b
                .conversation_store()
                .get_conversation(conversation_id)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn two_hosts_own_independent_concurrent_execution_runtimes() -> Result<(), String> {
        let process_cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root_a = temp.path().join("a");
        let root_b = temp.path().join("b");
        std::fs::create_dir_all(&root_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&root_b).map_err(|error| error.to_string())?;
        let canonical_a = root_a.canonicalize().map_err(|error| error.to_string())?;
        let canonical_b = root_b.canonicalize().map_err(|error| error.to_string())?;

        let primary = echo_agent::agent::ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("workspace runtime seed")
            .build()
            .map_err(|error| error.to_string())?;
        let seed = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(
                AgentHandle::new(primary),
                None,
                None,
                4,
                false,
            )
            .await,
        );
        let registry = WorkspaceRuntimeRegistry::new();
        let host_a = registry
            .get_or_open(workspace("a", root_a))
            .await
            .map_err(|error| error.to_string())?;
        let host_b = registry
            .get_or_open(workspace("b", root_b))
            .await
            .map_err(|error| error.to_string())?;

        let (runtime_a, runtime_b) = tokio::try_join!(
            host_a.get_or_open_execution(&seed),
            host_b.get_or_open_execution(&seed)
        )
        .map_err(|error| error.to_string())?;
        assert!(!Arc::ptr_eq(&runtime_a, &runtime_b));
        assert!(!Arc::ptr_eq(&runtime_a.pool(), &runtime_b.pool()));
        assert_eq!(runtime_a.task_runtime().active_workspace_id(), "a");
        assert_eq!(runtime_b.task_runtime().active_workspace_id(), "b");
        assert_eq!(
            runtime_a.task_runtime().active_shadow_root(),
            WorkspaceLayout::tasks(&canonical_a)
        );
        assert_eq!(
            runtime_b.task_runtime().active_shadow_root(),
            WorkspaceLayout::tasks(&canonical_b)
        );

        let pool_a = runtime_a.pool();
        let pool_b = runtime_b.pool();
        let (lease_a, lease_b) = tokio::try_join!(
            pool_a.acquire("same-conversation"),
            pool_b.acquire("same-conversation")
        )
        .map_err(|error| error.to_string())?;
        let agent_a = lease_a.agent();
        let agent_b = lease_b.agent();
        assert!(!Arc::ptr_eq(agent_a.inner(), agent_b.inner()));

        let working_dir_a = agent_a.read(|agent| agent.working_dir()).await;
        let working_dir_b = agent_b.read(|agent| agent.working_dir()).await;
        assert_eq!(working_dir_a.as_deref(), Some(canonical_a.as_path()));
        assert_eq!(working_dir_b.as_deref(), Some(canonical_b.as_path()));

        let tools_a = agent_a.read(|agent| agent.tool_names()).await;
        let tools_b = agent_b.read(|agent| agent.tool_names()).await;
        for expected in ["task_create", "task_update", "task_list", "task_execute"] {
            assert!(tools_a.iter().any(|name| name == expected));
            assert!(tools_b.iter().any(|name| name == expected));
        }
        let tool_manager_a = agent_a.read(|agent| agent.tool_manager().clone()).await;
        let tool_manager_b = agent_b.read(|agent| agent.tool_manager().clone()).await;
        assert!(!Arc::ptr_eq(&tool_manager_a, &tool_manager_b));

        let artifacts_a = agent_a
            .read(|agent| agent.tool_output_artifacts())
            .await
            .ok_or_else(|| "workspace A artifact config missing".to_string())?;
        let artifacts_b = agent_b
            .read(|agent| agent.tool_output_artifacts())
            .await
            .ok_or_else(|| "workspace B artifact config missing".to_string())?;
        assert!(artifacts_a.root_dir.starts_with(&canonical_a));
        assert!(artifacts_b.root_dir.starts_with(&canonical_b));
        assert_eq!(
            std::env::current_dir().map_err(|error| error.to_string())?,
            process_cwd
        );
        Ok(())
    }

    #[tokio::test]
    async fn three_hosts_converge_mcp_generations_without_sharing_activity_or_tools()
    -> Result<(), String> {
        let process_cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let primary = echo_agent::agent::ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("workspace generation seed")
            .build()
            .map_err(|error| error.to_string())?;
        let seed = Arc::new(
            crate::agent_pool::AgentPool::new_for_test(
                AgentHandle::new(primary),
                None,
                None,
                4,
                false,
            )
            .await,
        );
        let registry = WorkspaceRuntimeRegistry::new();
        let mut runtimes = Vec::new();
        for position in 0..3 {
            let name = format!("workspace-{position}");
            let root = temp.path().join(&name);
            std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
            let host = registry
                .get_or_open(workspace(&name, root))
                .await
                .map_err(|error| error.to_string())?;
            runtimes.push(
                host.get_or_open_execution(&seed)
                    .await
                    .map_err(|error| error.to_string())?,
            );
        }

        let mcp = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            temp.path().join("mcp.json"),
            Default::default(),
        ));
        let mut final_name = String::new();
        for generation in 1..=24 {
            final_name = format!("fixture-{generation}");
            let mut candidate = echo_agent::mcp::McpConfigFile::default();
            candidate.mcp_servers.insert(
                final_name.clone(),
                echo_agent::mcp::McpServerEntry {
                    command: Some("fixture-command".to_string()),
                    disabled: true,
                    ..Default::default()
                },
            );
            let targets = runtimes
                .iter()
                .map(|runtime| runtime.mcp_reconcile_target())
                .collect();
            let committed = mcp
                .replace_and_reconcile(targets, candidate)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(committed, generation);
        }

        let expected =
            serde_json::to_value(mcp.snapshot().await).map_err(|error| error.to_string())?;
        let mut tool_managers = Vec::new();
        for runtime in &runtimes {
            let snapshot = runtime
                .pool()
                .mcp_config_snapshot_for_test()
                .await
                .ok_or_else(|| "workspace MCP snapshot missing".to_string())?;
            assert_eq!(
                serde_json::to_value(snapshot).map_err(|error| error.to_string())?,
                expected
            );
            assert!(runtime.mcp_ownership.is_user_owned(&final_name).await);
            tool_managers.push(
                runtime
                    .primary_agent()
                    .read(|agent| agent.tool_manager().clone())
                    .await,
            );
        }
        for (position, left) in tool_managers.iter().enumerate() {
            for right in tool_managers.iter().skip(position.saturating_add(1)) {
                assert!(!Arc::ptr_eq(left, right));
            }
        }

        let mut leases = Vec::new();
        for (position, runtime) in runtimes.iter().enumerate() {
            leases.push(
                runtime
                    .pool()
                    .acquire(&format!("conversation-{position}"))
                    .await
                    .map_err(|error| error.to_string())?,
            );
        }
        let active = registry
            .activity_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(active.len(), 3);
        assert!(active.iter().all(|activity| {
            activity.execution_loaded
                && activity.active_pool_executions == 1
                && activity.active_run_drivers == 0
                && activity.active_run_driver_receipts == 0
        }));
        drop(leases);
        let idle = registry
            .activity_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        assert!(idle.iter().all(WorkspaceRuntimeActivity::is_idle));
        assert_eq!(
            std::env::current_dir().map_err(|error| error.to_string())?,
            process_cwd
        );
        mcp.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn registry_reuses_one_host_and_refreshes_workspace_metadata() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let registry = WorkspaceRuntimeRegistry::new();

        let first = registry
            .get_or_open(workspace("alpha", root.clone()))
            .await
            .map_err(|error| error.to_string())?;
        let mut updated = workspace("alpha", root);
        updated.project_root = Some(temp.path().join("project"));
        let second = registry
            .get_or_open(updated.clone())
            .await
            .map_err(|error| error.to_string())?;

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(registry.host_count().await, 1);
        assert_eq!(second.workspace().await.project_root, updated.project_root);
        Ok(())
    }

    #[tokio::test]
    async fn registry_rejects_a_loaded_identity_at_another_root() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        std::fs::create_dir_all(&first_root).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second_root).map_err(|error| error.to_string())?;
        let registry = WorkspaceRuntimeRegistry::new();

        registry
            .get_or_open(workspace("alpha", first_root))
            .await
            .map_err(|error| error.to_string())?;
        let error = registry
            .get_or_open(workspace("alpha", second_root))
            .await
            .err()
            .ok_or_else(|| "root drift should be rejected".to_string())?;

        assert!(error.to_string().contains("refusing root change"));
        assert_eq!(registry.host_count().await, 1);
        Ok(())
    }
}
