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

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use notify::{Config, EventKind, RecursiveMode, Watcher};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent_handle::AgentHandle;

/// Quiet window for the resettable debounce. A save is considered "settled"
/// when no qualifying event has arrived for this long.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);

fn workspace_target_updates() -> &'static tokio::sync::broadcast::Sender<PathBuf> {
    static UPDATES: OnceLock<tokio::sync::broadcast::Sender<PathBuf>> = OnceLock::new();
    UPDATES.get_or_init(|| tokio::sync::broadcast::channel(16).0)
}

/// Retarget every live config watcher after EKO switches workspace.
pub fn notify_config_watcher_workspace(root: PathBuf) {
    let _ = workspace_target_updates().send(root.join(".eko").join("hooks.yaml"));
}

/// Resolve the config file path that was actually loaded.
///
/// Returns the first existing path from the search list, or the explicit
/// override path if provided.
pub fn resolve_config_path(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(PathBuf::from(p));
    }
    echo_agent::config::config_search_paths()
        .into_iter()
        .find(|path| path.exists())
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
    webhook_emitter: Option<std::sync::Arc<crate::webhook::WebhookEmitter>>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut targets = config_watch_targets(config_path.as_deref());
        let mut workspace_target = current_workspace_hook_target();
        let mut workspace_updates = workspace_target_updates().subscribe();

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

        let mut watched = std::collections::HashSet::new();
        for target in &targets {
            let Some(directory) = nearest_existing_parent(target) else {
                continue;
            };
            if !watched.insert(directory.clone()) {
                continue;
            }
            if let Err(error) = watcher.watch(&directory, RecursiveMode::Recursive) {
                warn!(path = %directory.display(), %error, "Failed to watch config directory");
            }
        }
        if watched.is_empty() {
            warn!("No existing directory is available for config watching");
            return;
        }

        info!(targets = ?targets, "Config watcher started");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("Config watcher shutting down");
                    break;
                }
                update = workspace_updates.recv() => {
                    let target = match update {
                        Ok(target) => target,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if let Some(previous) = workspace_target.replace(target.clone()) {
                        targets.retain(|candidate| candidate != &previous);
                    }
                    if !targets.contains(&target) {
                        targets.push(target.clone());
                    }
                    if let Some(directory) = nearest_existing_parent(&target)
                        && watched.insert(directory.clone())
                        && let Err(error) = watcher.watch(&directory, RecursiveMode::Recursive)
                    {
                        warn!(path = %directory.display(), %error, "Failed to watch workspace config directory");
                    }
                    info!(path = %target.display(), "Config watcher retargeted to workspace");
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
                            _ = cancel.cancelled() => break,
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
                    if cancel.is_cancelled() {
                        break;
                    }

                    info!("Config file changed: {}", changed_path.display());
                    handle_config_change(
                        &changed_path,
                        config_path.as_deref(),
                        &agent,
                        webhook_emitter.as_deref(),
                    )
                    .await;
                }
            }
        }
    })
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

fn config_watch_targets(config_path: Option<&Path>) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    if let Some(path) = config_path {
        targets.push(path.to_path_buf());
    }
    targets.push(echo_agent::paths::user_data_path("hooks.yaml"));
    if let Some(project) = current_workspace_hook_target() {
        targets.push(project);
    }
    targets.sort();
    targets.dedup();
    targets
}

fn current_workspace_hook_target() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".eko").join("hooks.yaml"))
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
    //
    // Model selection, MCP topology, and runtime limits are wired into
    // long-lived subsystems at agent build time and are NOT reloaded here — a
    // restart is required for those to take effect (see module docs).
    let new_config = config_path
        .and_then(Path::to_str)
        .map(|path| echo_agent::config::load_config(Some(path)))
        .unwrap_or_else(|| echo_agent::config::load_config(None));
    let loaded = crate::hook_config_loader::HookConfigLoader::load_merged_from_disk_at(config_path);
    if loaded.errors.is_empty() {
        let definition = loaded.definition;
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
    } else {
        warn!(errors = %loaded.errors.join("; "), "Hook config reload rejected; keeping last known-good hooks");
    }
    if let Some(emitter) = webhook_emitter {
        emitter.reload_from_config(&new_config).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_targets_include_app_global_and_project_hook_files() -> Result<(), String> {
        let current = std::env::current_dir().map_err(|error| error.to_string())?;
        let app = current.join("echo-agent.test.yaml");
        let targets = config_watch_targets(Some(&app));

        assert!(targets.contains(&app));
        assert!(targets.contains(&echo_agent::paths::user_data_path("hooks.yaml")));
        assert!(targets.contains(&current.join(".eko/hooks.yaml")));
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
}
