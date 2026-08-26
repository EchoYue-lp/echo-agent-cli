//! Extension-config watcher — monitors the app config plus every registered
//! workspace's `hooks.yaml`/`.lsp.yaml`, hot-reloads user hooks, LSP generations
//! and webhook endpoints, and fires the `ConfigChange` lifecycle hook.
//!
//! ## Scope (intentional)
//!
//! Hooks, LSP generations and webhook endpoints are reloaded here. Model and
//! MCP generations have separate application-owned publication paths; runtime
//! limits still require a restart. LSP rebind delegates through the shared
//! `ExtensionControlService` admission and `PluginRuntimeService` mutation owner,
//! so this watcher never becomes a second mutation state machine.
//!
//! ## Robustness features
//!
//! - **Parent-directory watch with path filter.** Watching the file directly
//!   breaks on editors that save atomically (write-temp + rename): on macOS and
//!   Linux the inode under the original path is replaced and the watch dies.
//!   Watching the parent directory and filtering events to the target file
//!   survives rename-save.
//! - **Resettable debounce.** Events arrive in bursts (one save = many modify
//!   events). The debounce timer is reset on every qualifying event during the
//!   quiet window, so a burst collapses into exactly one reload rather than N
//!   serial reloads (the previous fixed 500ms-sleep-per-event approach).
//! - **Channel draining.** While waiting for the quiet window, additional
//!   events are drained and only used to extend the window — never to trigger
//!   additional reloads.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use notify::{Config, EventKind, RecursiveMode, Watcher};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent_handle::AgentHandle;

/// Quiet window for the resettable debounce. A save is considered "settled"
/// when no qualifying event has arrived for this long.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);

enum WatcherCommand {
    Register {
        identity: ConfigWatcherWorkspaceIdentity,
        root: PathBuf,
        agent: AgentHandle,
        plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
        ack: oneshot::Sender<anyhow::Result<ConfigWatcherRegistrationReceipt>>,
    },
    Unregister {
        identity: ConfigWatcherWorkspaceIdentity,
        ack: oneshot::Sender<anyhow::Result<bool>>,
    },
    #[cfg(test)]
    TriggerChange {
        path: PathBuf,
        ack: oneshot::Sender<anyhow::Result<()>>,
    },
}

/// Exact identity of one workspace generation registered with the watcher.
///
/// The generation is derived from the workspace's durable identity. Reusing a
/// workspace id or root therefore cannot let a delayed unregister remove the
/// replacement generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWatcherWorkspaceIdentity {
    workspace_id: String,
    generation: String,
}

impl ConfigWatcherWorkspaceIdentity {
    pub fn new(workspace_id: impl Into<String>, generation: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            generation: generation.into(),
        }
    }

    fn global() -> Self {
        Self::new("global", "global")
    }

    fn matches(&self, other: &Self) -> bool {
        self.workspace_id == other.workspace_id && self.generation == other.generation
    }
}

#[derive(Debug, Clone)]
pub struct ConfigWatcherRegistrationReceipt {
    pub registered_root: PathBuf,
    pub watched_roots: Vec<PathBuf>,
    pub errors: Vec<String>,
}

#[derive(Clone)]
struct RegisteredConfigTarget {
    identity: ConfigWatcherWorkspaceIdentity,
    root: PathBuf,
    agent: Weak<tokio::sync::RwLock<echo_agent::agent::ReactAgent>>,
    plugin_runtime: Option<Weak<crate::plugin_runtime::PluginRuntimeService>>,
}

impl RegisteredConfigTarget {
    fn new(
        identity: ConfigWatcherWorkspaceIdentity,
        root: PathBuf,
        agent: &AgentHandle,
        plugin_runtime: Option<&Arc<crate::plugin_runtime::PluginRuntimeService>>,
    ) -> Self {
        Self {
            identity,
            root,
            agent: Arc::downgrade(agent.inner()),
            plugin_runtime: plugin_runtime.map(Arc::downgrade),
        }
    }

    fn agent(&self) -> Option<AgentHandle> {
        self.agent.upgrade().map(AgentHandle::from_arc)
    }

    fn plugin_runtime(&self) -> Option<Arc<crate::plugin_runtime::PluginRuntimeService>> {
        self.plugin_runtime.as_ref().and_then(Weak::upgrade)
    }

    fn is_live(&self) -> bool {
        self.agent.strong_count() > 0
    }
}

/// Owns the config watcher's control channel, cancellation, and background task.
pub struct ConfigWatcherHandle {
    config_path: Option<PathBuf>,
    control: mpsc::Sender<WatcherCommand>,
    registered_roots: Arc<tokio::sync::RwLock<Vec<PathBuf>>>,
    cancel: CancellationToken,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl ConfigWatcherHandle {
    pub fn config_path(&self) -> Option<PathBuf> {
        self.config_path.clone()
    }

    /// Parse every hook source for a target root without mutating live hooks.
    pub fn preflight_workspace(&self, root: &Path) -> anyhow::Result<()> {
        let loaded =
            crate::hook_config_loader::HookConfigLoader::load_merged_from_disk_for_workspace(
                self.config_path.as_deref(),
                Some(root),
            );
        if loaded.errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(loaded.errors.join("; ")))
        }
    }

    /// Add or refresh one exact workspace generation without evicting other
    /// workspace hosts.
    pub async fn register_workspace(
        &self,
        identity: ConfigWatcherWorkspaceIdentity,
        root: PathBuf,
        agent: AgentHandle,
        plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
    ) -> anyhow::Result<ConfigWatcherRegistrationReceipt> {
        let (ack, result) = oneshot::channel();
        self.control
            .send(WatcherCommand::Register {
                identity,
                root,
                agent,
                plugin_runtime,
                ack,
            })
            .await
            .map_err(|_| anyhow::anyhow!("config watcher is not running"))?;
        result.await.map_err(|_| {
            anyhow::anyhow!("config watcher stopped before acknowledging registration")
        })?
    }

    /// Remove only the exact registered workspace generation.
    pub async fn unregister_workspace(
        &self,
        identity: ConfigWatcherWorkspaceIdentity,
    ) -> anyhow::Result<bool> {
        let (ack, result) = oneshot::channel();
        self.control
            .send(WatcherCommand::Unregister { identity, ack })
            .await
            .map_err(|_| anyhow::anyhow!("config watcher is not running"))?;
        result.await.map_err(|_| {
            anyhow::anyhow!("config watcher stopped before acknowledging unregistration")
        })?
    }

    #[cfg(test)]
    async fn trigger_change_for_test(&self, path: PathBuf) -> anyhow::Result<()> {
        let (ack, result) = oneshot::channel();
        self.control
            .send(WatcherCommand::TriggerChange { path, ack })
            .await
            .map_err(|_| anyhow::anyhow!("config watcher is not running"))?;
        result
            .await
            .map_err(|_| anyhow::anyhow!("config watcher stopped before handling test change"))?
    }

    pub async fn registered_roots(&self) -> Vec<PathBuf> {
        self.registered_roots.read().await.clone()
    }

    /// Cancel and await the watcher. Repeated calls are harmless.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.cancel.cancel();
        let Some(join) = self.join.lock().await.take() else {
            return Ok(());
        };
        join.await
            .map_err(|error| anyhow::anyhow!("config watcher task failed: {error}"))
    }
}

/// Resolve the config file path that was actually loaded.
///
/// Returns the first existing path from the search list, or the explicit
/// override path if provided.
pub fn resolve_config_path(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(anchor_to_current_dir(PathBuf::from(p)));
    }
    crate::config::config_search_paths()
        .into_iter()
        .find(|path| path.exists())
        .map(anchor_to_current_dir)
}

/// Resolve the immutable file targeted by application-side configuration edits.
/// Relative paths are anchored before workspace switches can change the process
/// working directory.
pub fn resolve_config_save_path(explicit: Option<&str>) -> PathBuf {
    let selected = resolve_config_path(explicit).unwrap_or_else(|| {
        crate::config::config_search_paths()
            .into_iter()
            .nth(1)
            .unwrap_or_else(|| crate::data_root::user_data_path("config.yaml"))
    });
    anchor_to_current_dir(selected)
}

fn anchor_to_current_dir(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(&path))
            .unwrap_or(path)
    }
}

/// Spawn a background task that watches the config file for changes and
/// hot-reloads user hooks and webhook endpoints.
///
/// When the file settles after a burst of edits, it:
/// 1. Fires `ConfigChange` hook with the file path as matcher context
/// 2. Reloads the config, user hooks, and webhook endpoints
///
/// The watcher stops when the cancellation token is triggered.
pub fn spawn_config_watcher(
    config_path: Option<PathBuf>,
    agent: AgentHandle,
    initial_root: PathBuf,
    plugin_runtime: Option<Arc<crate::plugin_runtime::PluginRuntimeService>>,
    extension_control: Arc<crate::extension_control::ExtensionControlService>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    parent_cancel: CancellationToken,
) -> ConfigWatcherHandle {
    let cancel = parent_cancel.child_token();
    let task_cancel = cancel.clone();
    let (control, mut commands) = mpsc::channel(8);
    let handle_config_path = config_path.clone();
    let registered_roots = Arc::new(tokio::sync::RwLock::new(vec![initial_root.clone()]));
    let task_registered_roots = Arc::clone(&registered_roots);
    let join = tokio::spawn(async move {
        let mut registered = vec![RegisteredConfigTarget::new(
            ConfigWatcherWorkspaceIdentity::global(),
            initial_root,
            &agent,
            plugin_runtime.as_ref(),
        )];
        let mut targets = config_watch_targets(
            config_path.as_deref(),
            registered.iter().map(|target| target.root.as_path()),
        );

        // Use a bounded async channel to receive filesystem events.
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);

        let mut watcher = match notify::RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                let _ = tx.blocking_send(res);
            },
            Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                warn!("Failed to create config file watcher: {}", e);
                return;
            }
        };

        let mut watched = HashSet::new();
        if let Err(error) = reconcile_watched_directories(&mut watcher, &targets, &mut watched) {
            warn!(%error, "Failed to initialize config watcher");
        }
        if watched.is_empty() {
            warn!("No existing directory is available for config watching");
        }

        info!(targets = ?targets, "Config watcher started");

        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => {
                    info!("Config watcher shutting down");
                    break;
                }
                command = commands.recv() => {
                    let Some(command) = command else {
                        debug!("Config watcher control channel closed");
                        break;
                    };
                    let mut registration = ConfigWatcherRegistrationContext {
                        config_path: config_path.as_deref(),
                        watcher: &mut watcher,
                        watched: &mut watched,
                        targets: &mut targets,
                        registered: &mut registered,
                        registered_roots: &task_registered_roots,
                        extension_control: &extension_control,
                    };
                    match command {
                        WatcherCommand::Register {
                            identity,
                            root,
                            agent,
                            plugin_runtime,
                            ack,
                        } => {
                            let target = RegisteredConfigTarget::new(
                                identity,
                                root,
                                &agent,
                                plugin_runtime.as_ref(),
                            );
                            let result = register_workspace_target(target, &mut registration).await;
                            let _ = ack.send(Ok(result));
                        }
                        WatcherCommand::Unregister { identity, ack } => {
                            let result = unregister_workspace_target(&identity, &mut registration).await;
                            let _ = ack.send(result);
                        }
                        #[cfg(test)]
                        WatcherCommand::TriggerChange { path, ack } => {
                            registration.registered.retain(RegisteredConfigTarget::is_live);
                            let result = refresh_registered_targets(&mut registration).await;
                            if result.is_ok() {
                                handle_config_change(
                                    &path,
                                    registration.config_path,
                                    registration.registered,
                                    registration.extension_control,
                                    webhook_emitter.as_deref(),
                                ).await;
                            }
                            let _ = ack.send(result);
                        }
                    }
                }
                result = rx.recv() => {
                    let Some(event) = result else {
                        debug!("Config watcher channel closed");
                        break;
                    };
                    let notify_event = match event {
                        Ok(ev) => ev,
                        Err(e) => {
                            warn!("Config watch error: {}", e);
                            continue;
                        }
                    };
                    // React to writes and removals that touch one of the
                    // watched config files. A removal must rebuild the merged
                    // registry so hooks from the deleted source stop running.
                    if !is_config_change_event(&notify_event.kind) {
                        continue;
                    }
                    let Some(changed_path) = event_touched_target(&notify_event, &targets) else {
                        continue;
                    };

                    // Resettable debounce: keep resetting the quiet window while
                    // events keep arriving. Drain the channel non-blockingly so
                    // a burst collapses into one reload, not N serial reloads.
                    let settled = tokio::time::sleep(DEBOUNCE_WINDOW);
                    tokio::pin!(settled);
                    loop {
                        tokio::select! {
                            _ = task_cancel.cancelled() => break,
                            _ = &mut settled => break,
                            extra = rx.recv() => {
                                let Some(Ok(ev)) = extra else { continue };
                                if is_config_change_event(&ev.kind)
                                    && event_touched_target(&ev, &targets).is_some()
                                {
                                    // Qualifying event during quiet window:
                                    // reset the debounce timer.
                                    settled.as_mut().reset(tokio::time::Instant::now() + DEBOUNCE_WINDOW);
                                }
                            }
                        }
                    }
                    if task_cancel.is_cancelled() {
                        break;
                    }

                    info!("Config file changed: {}", changed_path.display());
                    handle_config_change(
                        &changed_path,
                        config_path.as_deref(),
                        &registered,
                        &extension_control,
                        webhook_emitter.as_deref(),
                    )
                    .await;
                }
            }
        }
    });
    ConfigWatcherHandle {
        config_path: handle_config_path,
        control,
        registered_roots,
        cancel,
        join: Mutex::new(Some(join)),
    }
}

struct ConfigWatcherRegistrationContext<'a> {
    config_path: Option<&'a Path>,
    watcher: &'a mut notify::RecommendedWatcher,
    watched: &'a mut HashSet<PathBuf>,
    targets: &'a mut Vec<PathBuf>,
    registered: &'a mut Vec<RegisteredConfigTarget>,
    registered_roots: &'a tokio::sync::RwLock<Vec<PathBuf>>,
    extension_control: &'a Arc<crate::extension_control::ExtensionControlService>,
}

async fn register_workspace_target(
    new_target: RegisteredConfigTarget,
    context: &mut ConfigWatcherRegistrationContext<'_>,
) -> ConfigWatcherRegistrationReceipt {
    let root = new_target.root.clone();
    context.registered.retain(|target| {
        target.is_live() && target.identity.workspace_id != new_target.identity.workspace_id
    });
    context.registered.push(new_target.clone());
    let mut errors = Vec::new();
    if let Err(error) = refresh_registered_targets(context).await {
        errors.push(error.to_string());
    }
    let roots = context.registered_roots.read().await.clone();

    let workspace_hook = root.join(".eko").join("hooks.yaml");
    let reload = match new_target.agent() {
        Some(agent) => {
            context
                .extension_control
                .reload_hooks_for_target(
                    context.config_path.map(Path::to_path_buf),
                    root.clone(),
                    agent,
                    false,
                )
                .await
        }
        None => Err(anyhow::anyhow!(
            "workspace Agent generation is no longer live"
        )),
    };
    info!(path = %workspace_hook.display(), "Config watcher registered workspace host");
    if let Err(error) = reload {
        errors.push(format!("target hook rebuild failed: {error}"));
    }
    ConfigWatcherRegistrationReceipt {
        registered_root: root,
        watched_roots: roots,
        errors,
    }
}

async fn unregister_workspace_target(
    identity: &ConfigWatcherWorkspaceIdentity,
    context: &mut ConfigWatcherRegistrationContext<'_>,
) -> anyhow::Result<bool> {
    let removed = context
        .registered
        .iter()
        .any(|target| target.identity.matches(identity));
    context
        .registered
        .retain(|target| !target.identity.matches(identity) && target.is_live());
    refresh_registered_targets(context).await?;
    Ok(removed)
}

async fn refresh_registered_targets(
    context: &mut ConfigWatcherRegistrationContext<'_>,
) -> anyhow::Result<()> {
    let next_targets = config_watch_targets(
        context.config_path,
        context
            .registered
            .iter()
            .map(|target| target.root.as_path()),
    );
    reconcile_watched_directories(context.watcher, &next_targets, context.watched)?;
    *context.targets = next_targets;
    let mut roots = context
        .registered
        .iter()
        .map(|target| target.root.clone())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    *context.registered_roots.write().await = roots;
    Ok(())
}

fn reconcile_watched_directories(
    watcher: &mut notify::RecommendedWatcher,
    targets: &[PathBuf],
    watched: &mut HashSet<PathBuf>,
) -> anyhow::Result<()> {
    let desired = desired_watch_directories(targets);
    let additions = desired.difference(watched).cloned().collect::<Vec<_>>();
    let mut added: Vec<PathBuf> = Vec::new();
    for directory in additions {
        if let Err(error) = watcher.watch(&directory, RecursiveMode::Recursive) {
            for rollback in added {
                let _ = watcher.unwatch(&rollback);
                watched.remove(&rollback);
            }
            return Err(anyhow::anyhow!(
                "Failed to watch config directory '{}': {error}",
                directory.display()
            ));
        }
        watched.insert(directory.clone());
        added.push(directory);
    }

    let removals = watched.difference(&desired).cloned().collect::<Vec<_>>();
    let mut removed: Vec<PathBuf> = Vec::new();
    for directory in removals {
        if let Err(error) = watcher.unwatch(&directory) {
            let mut rollback_errors = Vec::new();
            for restore in removed {
                if let Err(restore_error) = watcher.watch(&restore, RecursiveMode::Recursive) {
                    rollback_errors.push(format!(
                        "Failed to restore config watch '{}': {restore_error}",
                        restore.display()
                    ));
                } else {
                    watched.insert(restore);
                }
            }
            for rollback in added {
                if let Err(rollback_error) = watcher.unwatch(&rollback) {
                    rollback_errors.push(format!(
                        "Failed to roll back config watch '{}': {rollback_error}",
                        rollback.display()
                    ));
                } else {
                    watched.remove(&rollback);
                }
            }
            return Err(anyhow::anyhow!(append_watch_errors(
                format!(
                    "Failed to unwatch old config directory '{}': {error}",
                    directory.display()
                ),
                rollback_errors,
            )));
        }
        watched.remove(&directory);
        removed.push(directory);
    }
    Ok(())
}

fn desired_watch_directories(targets: &[PathBuf]) -> HashSet<PathBuf> {
    targets
        .iter()
        .filter_map(|target| nearest_existing_parent(target))
        .collect()
}

fn append_watch_errors(mut primary: String, errors: Vec<String>) -> String {
    if !errors.is_empty() {
        primary.push_str("; ");
        primary.push_str(&errors.join("; "));
    }
    primary
}

fn is_config_change_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// Return the watched target changed by `event` (compared by canonical path
/// when possible, falling back to a suffix/contains match on the event paths).
///
/// Path comparison is character-safe (uses `Path` API only).
fn event_touched_target(event: &notify::Event, targets: &[PathBuf]) -> Option<PathBuf> {
    for target in targets {
        let target_canon = target
            .canonicalize()
            .unwrap_or_else(|_| target.to_path_buf());
        for path in &event.paths {
            let path_canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            if path_canon == target_canon || path == target {
                return Some(target.clone());
            }
        }
    }
    None
}

fn config_watch_targets<'a>(
    config_path: Option<&Path>,
    workspace_roots: impl IntoIterator<Item = &'a Path>,
) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    if let Some(path) = config_path {
        targets.push(path.to_path_buf());
    }
    targets.push(crate::data_root::user_data_path("hooks.yaml"));
    targets.push(crate::data_root::user_data_path(".lsp.yaml"));
    targets.extend(
        workspace_roots
            .into_iter()
            .flat_map(|root| [root.join(".eko").join("hooks.yaml"), root.join(".lsp.yaml")]),
    );
    targets.sort();
    targets.dedup();
    targets
}

fn nearest_existing_parent(path: &Path) -> Option<PathBuf> {
    let mut current = path.parent();
    while let Some(candidate) = current {
        // Never fall back to a recursive filesystem-root watch when an
        // explicitly configured path has no existing ancestor directory.
        candidate.parent()?;
        if candidate.is_dir() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

async fn handle_config_change(
    changed_path: &std::path::Path,
    config_path: Option<&std::path::Path>,
    registered: &[RegisteredConfigTarget],
    extension_control: &Arc<crate::extension_control::ExtensionControlService>,
    webhook_emitter: Option<&crate::webhook::WebhookEmitter>,
) {
    let path_str = changed_path.to_string_lossy().to_string();

    for target in registered {
        let is_lsp = changed_path
            .file_name()
            .is_some_and(|name| name == ".lsp.yaml");
        if is_lsp {
            if let Some(runtime) = target.plugin_runtime()
                && let Err(error) = extension_control
                    .rebind_plugin_runtime(runtime, target.root.clone())
                    .await
            {
                warn!(workspace_root = %target.root.display(), %error, "LSP config reload rejected; keeping last known-good generation");
            }
            continue;
        }
        let Some(agent) = target.agent() else {
            continue;
        };
        let path_for_hook = path_str.clone();
        agent
            .read_async(|agent| {
                Box::pin(async move {
                    agent
                        .fire_lifecycle_hook(
                            echo_agent::skills::hooks::HookEvent::ConfigChange,
                            Some(&path_for_hook),
                        )
                        .await;
                })
            })
            .await;
        if let Err(error) = extension_control
            .reload_hooks_for_target(
                config_path.map(Path::to_path_buf),
                target.root.clone(),
                agent,
                true,
            )
            .await
        {
            warn!(
                workspace_root = %target.root.display(),
                %error,
                "Hook config reload rejected; keeping last known-good hooks"
            );
        }
    }
    if let Some(emitter) = webhook_emitter {
        let new_config = config_path
            .and_then(Path::to_str)
            .map(|path| crate::config::load_config(Some(path)))
            .unwrap_or_else(|| crate::config::load_config(None));
        emitter.reload_from_config(&new_config).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::skills::hooks::{HookContext, HookEvent};
    use echo_agent::testing::MockLlmClient;

    #[test]
    fn watch_targets_include_hook_and_lsp_authorities() -> Result<(), String> {
        let current = std::env::current_dir().map_err(|error| error.to_string())?;
        let app = current.join("echo-agent.test.yaml");
        let targets = config_watch_targets(Some(&app), [current.as_path()]);

        assert!(targets.contains(&app));
        assert!(targets.contains(&crate::data_root::user_data_path("hooks.yaml")));
        assert!(targets.contains(&crate::data_root::user_data_path(".lsp.yaml")));
        assert!(targets.contains(&current.join(".eko/hooks.yaml")));
        assert!(targets.contains(&current.join(".lsp.yaml")));
        Ok(())
    }

    #[test]
    fn explicit_config_path_is_anchored_before_workspace_rebind() -> Result<(), String> {
        let current = std::env::current_dir().map_err(|error| error.to_string())?;
        assert_eq!(
            resolve_config_path(Some("relative-config.yaml")),
            Some(current.join("relative-config.yaml"))
        );
        Ok(())
    }

    #[test]
    fn missing_path_never_falls_back_to_filesystem_root() {
        assert_eq!(
            nearest_existing_parent(Path::new("/definitely-missing/hooks.yaml")),
            None
        );
    }

    #[test]
    fn create_modify_and_remove_events_trigger_reload() {
        assert!(is_config_change_event(&EventKind::Create(
            notify::event::CreateKind::File
        )));
        assert!(is_config_change_event(&EventKind::Modify(
            notify::event::ModifyKind::Data(notify::event::DataChange::Content)
        )));
        assert!(is_config_change_event(&EventKind::Remove(
            notify::event::RemoveKind::File
        )));
        assert!(!is_config_change_event(&EventKind::Access(
            notify::event::AccessKind::Read
        )));
    }

    #[tokio::test]
    async fn owned_handle_registers_three_independent_workspace_hook_targets() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let third = temp.path().join("third");
        let malformed = temp.path().join("malformed");
        for (root, marker) in [
            (&first, "workspace-a"),
            (&second, "workspace-b"),
            (&third, "workspace-c"),
        ] {
            let hooks_dir = root.join(".eko");
            std::fs::create_dir_all(&hooks_dir).map_err(|error| error.to_string())?;
            std::fs::write(
                hooks_dir.join("hooks.yaml"),
                format!(
                    "SessionStart:\n  - matcher: \"{marker}\"\n    hooks:\n      - type: prompt\n        prompt: \"{marker}\"\n"
                ),
            )
            .map_err(|error| error.to_string())?;
        }
        std::fs::create_dir_all(malformed.join(".eko")).map_err(|error| error.to_string())?;
        std::fs::write(
            malformed.join(".eko/hooks.yaml"),
            "SessionStart: [not-a-hook-rule]\n",
        )
        .map_err(|error| error.to_string())?;

        let bootstrap = test_agent()?;
        let first_agent = test_agent()?;
        let second_agent = test_agent()?;
        let third_agent = test_agent()?;
        let handle = spawn_config_watcher(
            None,
            bootstrap,
            temp.path().to_path_buf(),
            None,
            Arc::new(crate::extension_control::ExtensionControlService::default()),
            None,
            CancellationToken::new(),
        );

        for (workspace_id, root, agent) in [
            ("first", first.clone(), first_agent.clone()),
            ("second", second.clone(), second_agent.clone()),
            ("third", third.clone(), third_agent.clone()),
        ] {
            let receipt = handle
                .register_workspace(
                    ConfigWatcherWorkspaceIdentity::new(
                        workspace_id,
                        format!("generation:{}", root.display()),
                    ),
                    root.clone(),
                    agent,
                    None,
                )
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(receipt.registered_root, root);
            assert!(receipt.errors.is_empty());
        }
        assert_eq!(hook_match_count(&first_agent, "workspace-a").await, 1);
        assert_eq!(hook_match_count(&first_agent, "workspace-b").await, 0);
        assert_eq!(hook_match_count(&second_agent, "workspace-b").await, 1);
        assert_eq!(hook_match_count(&third_agent, "workspace-c").await, 1);

        std::fs::write(
            first.join(".eko/hooks.yaml"),
            "SessionStart:\n  - matcher: \"workspace-a2\"\n    hooks:\n      - type: prompt\n        prompt: \"workspace-a2\"\n",
        )
        .map_err(|error| error.to_string())?;
        handle
            .register_workspace(
                ConfigWatcherWorkspaceIdentity::new("first", "generation:first-refreshed"),
                first.clone(),
                first_agent.clone(),
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(hook_match_count(&first_agent, "workspace-a").await, 0);
        assert_eq!(hook_match_count(&first_agent, "workspace-a2").await, 1);
        assert_eq!(hook_match_count(&second_agent, "workspace-b").await, 1);
        assert_eq!(hook_match_count(&third_agent, "workspace-c").await, 1);

        assert!(handle.preflight_workspace(&malformed).is_err());
        let degraded = handle
            .register_workspace(
                ConfigWatcherWorkspaceIdentity::new("third", "generation:malformed"),
                malformed.clone(),
                third_agent.clone(),
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(degraded.registered_root, malformed);
        assert!(!degraded.errors.is_empty());
        assert_eq!(hook_match_count(&third_agent, "workspace-c").await, 0);
        assert_eq!(hook_match_count(&second_agent, "workspace-b").await, 1);
        handle.shutdown().await.map_err(|error| error.to_string())?;
        handle.shutdown().await.map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn deleted_workspace_recreation_routes_changes_only_to_new_generation()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let bootstrap_root = temp.path().join("bootstrap");
        let workspace_root = temp.path().join("same-root");
        std::fs::create_dir_all(bootstrap_root.join(".eko")).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(workspace_root.join(".eko")).map_err(|error| error.to_string())?;
        let hook_path = workspace_root.join(".eko/hooks.yaml");
        let lsp_path = workspace_root.join(".lsp.yaml");
        write_test_hook(&hook_path, "old-generation")?;
        write_test_lsp(&lsp_path, "old-language")?;

        let bootstrap = test_agent()?;
        let old_agent = test_agent()?;
        let old_plugin = crate::plugin_runtime::PluginRuntimeService::new_for_test(
            old_agent.clone(),
            workspace_root.clone(),
            temp.path().join("old-plugin-state.json"),
            temp.path().join("old-plugin-data"),
        )
        .await;
        let agent_strong_before = Arc::strong_count(old_agent.inner());
        let plugin_strong_before = Arc::strong_count(&old_plugin);
        let handle = spawn_config_watcher(
            None,
            bootstrap,
            bootstrap_root.clone(),
            None,
            Arc::new(crate::extension_control::ExtensionControlService::default()),
            None,
            CancellationToken::new(),
        );
        let old_identity = ConfigWatcherWorkspaceIdentity::new("same-id", "generation-1");
        handle
            .register_workspace(
                old_identity.clone(),
                workspace_root.clone(),
                old_agent.clone(),
                Some(Arc::clone(&old_plugin)),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(Arc::strong_count(old_agent.inner()), agent_strong_before);
        assert_eq!(Arc::strong_count(&old_plugin), plugin_strong_before);
        assert_eq!(hook_match_count(&old_agent, "old-generation").await, 1);
        handle
            .trigger_change_for_test(lsp_path.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            old_plugin
                .lsp_configured_languages()
                .await
                .iter()
                .any(|language| language == "old-language")
        );

        assert!(
            handle
                .unregister_workspace(old_identity.clone())
                .await
                .map_err(|error| error.to_string())?
        );
        assert!(!handle.registered_roots().await.contains(&workspace_root));

        write_test_hook(&hook_path, "new-generation")?;
        write_test_lsp(&lsp_path, "new-language")?;
        let new_agent = test_agent()?;
        let new_plugin = crate::plugin_runtime::PluginRuntimeService::new_for_test(
            new_agent.clone(),
            workspace_root.clone(),
            temp.path().join("new-plugin-state.json"),
            temp.path().join("new-plugin-data"),
        )
        .await;
        let new_identity = ConfigWatcherWorkspaceIdentity::new("same-id", "generation-2");
        handle
            .register_workspace(
                new_identity,
                workspace_root.clone(),
                new_agent.clone(),
                Some(Arc::clone(&new_plugin)),
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(hook_match_count(&new_agent, "new-generation").await, 1);
        handle
            .trigger_change_for_test(lsp_path.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            new_plugin
                .lsp_configured_languages()
                .await
                .iter()
                .any(|language| language == "new-language")
        );

        // A delayed delete settlement from generation 1 cannot unregister the
        // same-id replacement generation.
        assert!(
            !handle
                .unregister_workspace(old_identity)
                .await
                .map_err(|error| error.to_string())?
        );
        write_test_hook(&hook_path, "new-generation-reloaded")?;
        write_test_lsp(&lsp_path, "new-language-reloaded")?;
        handle
            .trigger_change_for_test(hook_path)
            .await
            .map_err(|error| error.to_string())?;
        handle
            .trigger_change_for_test(lsp_path)
            .await
            .map_err(|error| error.to_string())?;

        assert_eq!(
            hook_match_count(&new_agent, "new-generation-reloaded").await,
            1
        );
        assert_eq!(
            hook_match_count(&old_agent, "new-generation-reloaded").await,
            0
        );
        assert_eq!(hook_match_count(&old_agent, "old-generation").await, 1);
        let new_languages = new_plugin.lsp_configured_languages().await;
        assert!(
            new_languages
                .iter()
                .any(|language| language == "new-language-reloaded")
        );
        assert!(
            !new_languages
                .iter()
                .any(|language| language == "old-language")
        );
        let old_languages = old_plugin.lsp_configured_languages().await;
        assert!(
            old_languages
                .iter()
                .any(|language| language == "old-language")
        );
        assert!(
            !old_languages
                .iter()
                .any(|language| language == "new-language-reloaded")
        );
        assert!(handle.registered_roots().await.contains(&workspace_root));
        handle.shutdown().await.map_err(|error| error.to_string())?;
        old_plugin
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        new_plugin
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn write_test_hook(path: &Path, marker: &str) -> Result<(), String> {
        std::fs::write(
            path,
            format!(
                "SessionStart:\n  - matcher: \"{marker}\"\n    hooks:\n      - type: prompt\n        prompt: \"{marker}\"\n"
            ),
        )
        .map_err(|error| error.to_string())
    }

    fn write_test_lsp(path: &Path, language: &str) -> Result<(), String> {
        let config = echo_agent::lsp::LspConfigFile {
            languages: std::collections::HashMap::from([(
                language.to_string(),
                echo_agent::lsp::LspServerConfig {
                    language: language.to_string(),
                    command: "test-language-server".to_string(),
                    args: Vec::new(),
                    extensions: vec![format!(".{language}")],
                    env: std::collections::HashMap::new(),
                    initialization_options: None,
                    max_restarts: 0,
                },
            )]),
        };
        let yaml = serde_yaml::to_string(&config).map_err(|error| error.to_string())?;
        std::fs::write(path, yaml).map_err(|error| error.to_string())
    }

    fn test_agent() -> Result<AgentHandle, String> {
        ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("config watcher test")
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())
    }

    async fn hook_match_count(agent: &AgentHandle, matcher: &str) -> usize {
        let context = HookContext::for_dry_run(HookEvent::SessionStart, matcher);
        agent
            .read_async(|agent| {
                Box::pin(async move {
                    agent
                        .hook_registry()
                        .read()
                        .await
                        .dry_run(&context)
                        .matches
                        .len()
                })
            })
            .await
    }
}
