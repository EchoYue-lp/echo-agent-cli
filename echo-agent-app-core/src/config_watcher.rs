//! Hooks-config file watcher — monitors the app config plus global/project
//! `hooks.yaml` files and hot-reloads user **hooks and webhook endpoints** (and fires the
//! `ConfigChange` lifecycle hook).
//!
//! ## Scope (intentional)
//!
//! Hooks and webhook endpoints are reloaded live. Other config domains (model
//! selection, MCP server topology, runtime limits) require a restart because
//! they are wired into long-lived subsystems at agent construction. The watcher's name and
//! this doc reflect that scope; do not widen it without a parallel story for
//! safely tearing down and rebuilding those subsystems.
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
use std::sync::Arc;
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
    Rebind {
        root: PathBuf,
        ack: oneshot::Sender<anyhow::Result<ConfigWatcherRebindReceipt>>,
    },
}

#[derive(Debug, Clone)]
pub struct ConfigWatcherRebindReceipt {
    pub settled_root: PathBuf,
    pub stale_watch_roots: Vec<PathBuf>,
    pub errors: Vec<String>,
}

/// Owns the config watcher's control channel, cancellation, and background task.
pub struct ConfigWatcherHandle {
    config_path: Option<PathBuf>,
    control: mpsc::Sender<WatcherCommand>,
    settled_root: Arc<tokio::sync::RwLock<PathBuf>>,
    cancel: CancellationToken,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl ConfigWatcherHandle {
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

    /// Retarget the watcher and rebuild workspace hooks before returning.
    pub async fn rebind_workspace(
        &self,
        root: PathBuf,
    ) -> anyhow::Result<ConfigWatcherRebindReceipt> {
        let (ack, result) = oneshot::channel();
        self.control
            .send(WatcherCommand::Rebind { root, ack })
            .await
            .map_err(|_| anyhow::anyhow!("config watcher is not running"))?;
        result
            .await
            .map_err(|_| anyhow::anyhow!("config watcher stopped before acknowledging rebind"))?
    }

    pub async fn settled_root(&self) -> PathBuf {
        self.settled_root.read().await.clone()
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
    echo_agent::config::config_search_paths()
        .into_iter()
        .find(|path| path.exists())
        .map(anchor_to_current_dir)
}

/// Resolve the immutable file targeted by application-side configuration edits.
/// Relative paths are anchored before workspace switches can change the process
/// working directory.
pub fn resolve_config_save_path(explicit: Option<&str>) -> PathBuf {
    let selected = resolve_config_path(explicit).unwrap_or_else(|| {
        echo_agent::config::config_search_paths()
            .into_iter()
            .nth(1)
            .unwrap_or_else(|| echo_agent::paths::user_data_path("config.yaml"))
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
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    parent_cancel: CancellationToken,
) -> ConfigWatcherHandle {
    let cancel = parent_cancel.child_token();
    let task_cancel = cancel.clone();
    let (control, mut commands) = mpsc::channel(8);
    let handle_config_path = config_path.clone();
    let initial_root = current_workspace_root();
    let settled_root = Arc::new(tokio::sync::RwLock::new(initial_root.clone()));
    let task_settled_root = Arc::clone(&settled_root);
    let join = tokio::spawn(async move {
        let mut workspace_root = initial_root;
        let mut targets = config_watch_targets(config_path.as_deref(), &workspace_root);

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
                    let Some(WatcherCommand::Rebind { root, ack }) = command else {
                        debug!("Config watcher control channel closed");
                        break;
                    };
                    let result = rebind_workspace(
                        root,
                        config_path.as_deref(),
                        &agent,
                        webhook_emitter.as_deref(),
                        &mut watcher,
                        &mut watched,
                        &mut targets,
                        &mut workspace_root,
                        &task_settled_root,
                    ).await;
                    let _ = ack.send(Ok(result));
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
                        &workspace_root,
                        &agent,
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
        settled_root,
        cancel,
        join: Mutex::new(Some(join)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn rebind_workspace(
    root: PathBuf,
    config_path: Option<&Path>,
    agent: &AgentHandle,
    webhook_emitter: Option<&crate::webhook::WebhookEmitter>,
    watcher: &mut notify::RecommendedWatcher,
    watched: &mut HashSet<PathBuf>,
    targets: &mut Vec<PathBuf>,
    workspace_root: &mut PathBuf,
    settled_root: &tokio::sync::RwLock<PathBuf>,
) -> ConfigWatcherRebindReceipt {
    let next_targets = config_watch_targets(config_path, &root);
    let desired = desired_watch_directories(&next_targets);
    let mut errors = Vec::new();
    if let Err(error) = reconcile_watched_directories(watcher, &next_targets, watched) {
        errors.push(error.to_string());
    }
    let mut stale_watch_roots = watched.difference(&desired).cloned().collect::<Vec<_>>();
    stale_watch_roots.sort();
    *targets = next_targets;
    *workspace_root = root;
    *settled_root.write().await = workspace_root.clone();

    let workspace_hook = workspace_root.join(".eko").join("hooks.yaml");
    let reload =
        reload_live_config(config_path, workspace_root, agent, webhook_emitter, false).await;
    info!(path = %workspace_hook.display(), "Config watcher retargeted to workspace");
    if let Err(error) = reload {
        errors.push(format!("target hook rebuild failed: {error}"));
    }
    ConfigWatcherRebindReceipt {
        settled_root: workspace_root.clone(),
        stale_watch_roots,
        errors,
    }
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

fn config_watch_targets(config_path: Option<&Path>, workspace_root: &Path) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    if let Some(path) = config_path {
        targets.push(path.to_path_buf());
    }
    targets.push(echo_agent::paths::user_data_path("hooks.yaml"));
    targets.push(workspace_root.join(".eko").join("hooks.yaml"));
    targets.sort();
    targets.dedup();
    targets
}

fn current_workspace_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
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
    workspace_root: &Path,
    agent: &AgentHandle,
    webhook_emitter: Option<&crate::webhook::WebhookEmitter>,
) {
    let path_str = changed_path.to_string_lossy().to_string();

    // 1. Fire ConfigChange hook
    let path_for_hook = path_str.clone();
    agent
        .read_async(|a| {
            Box::pin(async move {
                a.fire_lifecycle_hook(
                    echo_agent::skills::hooks::HookEvent::ConfigChange,
                    Some(&path_for_hook),
                )
                .await;
            })
        })
        .await;

    // 2. Reload config and re-register the live-reloadable domains.
    if let Err(error) =
        reload_live_config(config_path, workspace_root, agent, webhook_emitter, true).await
    {
        warn!(%error, "Hook config reload rejected; keeping last known-good hooks");
    }
}

async fn reload_live_config(
    config_path: Option<&Path>,
    workspace_root: &Path,
    agent: &AgentHandle,
    webhook_emitter: Option<&crate::webhook::WebhookEmitter>,
    preserve_hooks_on_error: bool,
) -> anyhow::Result<()> {
    // Model selection, MCP topology, and runtime limits are wired into
    // long-lived subsystems at agent build time and are NOT reloaded here — a
    // restart is required for those to take effect (see module docs).
    let new_config = config_path
        .and_then(Path::to_str)
        .map(|path| echo_agent::config::load_config(Some(path)))
        .unwrap_or_else(|| echo_agent::config::load_config(None));
    let loaded = crate::hook_config_loader::HookConfigLoader::load_merged_from_disk_for_workspace(
        config_path,
        Some(workspace_root),
    );
    if loaded.errors.is_empty() {
        replace_user_hooks(agent, loaded.definition).await;
    } else {
        if preserve_hooks_on_error {
            return Err(anyhow::anyhow!(loaded.errors.join("; ")));
        }

        // A malformed project file must not keep hooks from the previous
        // workspace alive. Rebuild the generation from inline + global
        // sources; clear all user hooks only if those sources also fail.
        let fallback =
            crate::hook_config_loader::HookConfigLoader::load_merged_from_disk_for_workspace(
                config_path,
                None,
            );
        if fallback.errors.is_empty() {
            replace_user_hooks(agent, fallback.definition).await;
        } else {
            replace_user_hooks(agent, Default::default()).await;
        }
        let mut errors = loaded.errors;
        errors.extend(fallback.errors);
        return Err(anyhow::anyhow!(errors.join("; ")));
    }
    if let Some(emitter) = webhook_emitter {
        emitter.reload_from_config(&new_config).await;
    }
    Ok(())
}

async fn replace_user_hooks(
    agent: &AgentHandle,
    definition: echo_agent::skills::hooks::HooksDefinition,
) {
    agent
        .write_async(|agent| {
            Box::pin(async move {
                let mut registry = agent.hook_registry().write().await;
                registry.clear_user_hooks();
                if !definition.is_empty() {
                    registry.register_user_hooks(definition);
                }
            })
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::skills::hooks::{HookContext, HookEvent};
    use echo_agent::testing::MockLlmClient;

    #[test]
    fn watch_targets_include_app_global_and_project_hook_files() -> Result<(), String> {
        let current = std::env::current_dir().map_err(|error| error.to_string())?;
        let app = current.join("echo-agent.test.yaml");
        let targets = config_watch_targets(Some(&app), &current);

        assert!(targets.contains(&app));
        assert!(targets.contains(&echo_agent::paths::user_data_path("hooks.yaml")));
        assert!(targets.contains(&current.join(".eko/hooks.yaml")));
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
    async fn owned_handle_acknowledges_rebind_and_rebuilds_workspace_hooks() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let malformed = temp.path().join("malformed");
        for (root, marker) in [(&first, "workspace-a"), (&second, "workspace-b")] {
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

        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("config watcher test")
            .build()
            .map_err(|error| error.to_string())?;
        let agent = AgentHandle::new(agent);
        let handle = spawn_config_watcher(None, agent.clone(), None, CancellationToken::new());

        handle
            .rebind_workspace(first)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(hook_match_count(&agent, "workspace-a").await, 1);

        handle
            .rebind_workspace(second)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(hook_match_count(&agent, "workspace-a").await, 0);
        assert_eq!(hook_match_count(&agent, "workspace-b").await, 1);
        assert!(handle.preflight_workspace(&malformed).is_err());
        assert_eq!(hook_match_count(&agent, "workspace-b").await, 1);
        let degraded = handle
            .rebind_workspace(malformed.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(degraded.settled_root, malformed);
        assert!(!degraded.errors.is_empty());
        assert_eq!(hook_match_count(&agent, "workspace-b").await, 0);
        handle.shutdown().await.map_err(|error| error.to_string())?;
        handle.shutdown().await.map_err(|error| error.to_string())?;
        Ok(())
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
