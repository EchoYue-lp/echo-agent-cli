//! Configuration file watcher — monitors echo-agent.yaml for changes
//! and fires ConfigChange hooks + reloads user hooks.
//!
//! Uses the `notify` crate for filesystem event monitoring with 500ms
//! debouncing to avoid firing multiple events for a single save operation
//! (many editors write files in multiple steps: write + rename).

use std::path::PathBuf;
use std::time::Duration;

use notify::{Config, EventKind, RecursiveMode, Watcher};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent_handle::AgentHandle;
use crate::infra;

/// Resolve the config file path that was actually loaded.
///
/// Returns the first existing path from the search list, or the explicit
/// override path if provided.
pub fn resolve_config_path(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    for path in echo_agent::config::config_search_paths() {
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// Spawn a background task that watches the config file for changes.
///
/// When the file changes, it:
/// 1. Fires `ConfigChange` hook with the file path as matcher context
/// 2. Reloads the config and re-registers user hooks
///
/// The watcher stops when the cancellation token is triggered.
pub fn spawn_config_watcher(
    config_path: PathBuf,
    agent: AgentHandle,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Use a bounded async channel to receive filesystem events
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

        if let Err(e) = watcher.watch(&config_path, RecursiveMode::NonRecursive) {
            warn!(
                "Failed to watch config file {}: {}",
                config_path.display(),
                e
            );
            return;
        }

        info!("Config watcher started for {}", config_path.display());

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("Config watcher shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Some(event) => {
                            match event {
                                Ok(notify_event) => {
                                    // Only react to data modification events
                                    if !matches!(notify_event.kind, EventKind::Modify(_)) {
                                        continue;
                                    }

                                    // Debounce: wait 500ms to avoid multiple events from a single save
                                    tokio::time::sleep(Duration::from_millis(500)).await;

                                    // Check if cancellation happened during debounce
                                    if cancel.is_cancelled() {
                                        break;
                                    }

                                    info!("Config file changed: {}", config_path.display());
                                    handle_config_change(&config_path, &agent).await;
                                }
                                Err(e) => {
                                    warn!("Config watch error: {}", e);
                                }
                            }
                        }
                        None => {
                            // Channel closed — exit watcher
                            debug!("Config watcher channel closed");
                            break;
                        }
                    }
                }
            }
        }
    })
}

async fn handle_config_change(config_path: &PathBuf, agent: &AgentHandle) {
    let path_str = config_path.to_str().unwrap_or("").to_string();

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

    // 2. Reload config and re-register user hooks
    let new_config = echo_agent::config::load_config(Some(&path_str));

    infra::load_user_hooks(agent, &new_config).await;
}
