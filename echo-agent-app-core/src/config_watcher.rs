//! Hooks-config file watcher — monitors `echo-agent.yaml` for changes and
//! hot-reloads user **hooks and webhook endpoints** (and fires the
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
use std::time::Duration;

use notify::{Config, EventKind, RecursiveMode, Watcher};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent_handle::AgentHandle;
use crate::infra;

/// Quiet window for the resettable debounce. A save is considered "settled"
/// when no qualifying event has arrived for this long.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);

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
    config_path: PathBuf,
    agent: AgentHandle,
    webhook_emitter: Option<std::sync::Arc<crate::webhook::WebhookEmitter>>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(parent) = config_path.parent() else {
            warn!(
                "Config path {} has no parent directory; cannot watch",
                config_path.display()
            );
            return;
        };

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

        // Watch the PARENT directory (recursive=false). This survives the
        // atomic-write-temp-then-rename pattern used by most editors, which
        // would invalidate a direct file watch. We filter to our target below.
        if let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive) {
            warn!(
                "Failed to watch config parent directory {}: {}",
                parent.display(),
                e
            );
            return;
        }

        info!(
            "Config watcher started: watching {} for changes to {}",
            parent.display(),
            config_path.display()
        );

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("Config watcher shutting down");
                    break;
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
                    // Filter: only react to data-modify events that touch our
                    // target config file specifically.
                    if !is_config_write_event(&notify_event.kind) {
                        continue;
                    }
                    if !event_touches_target(&notify_event, &config_path) {
                        continue;
                    }

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
                                if is_config_write_event(&ev.kind)
                                    && event_touches_target(&ev, &config_path)
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

                    info!("Config file changed: {}", config_path.display());
                    handle_config_change(&config_path, &agent, webhook_emitter.as_deref()).await;
                }
            }
        }
    })
}

fn is_config_write_event(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Modify(_))
}

/// True when `event` modifies the file at `target` (compared by canonical path
/// when possible, falling back to a suffix/contains match on the event paths).
///
/// Path comparison is character-safe (uses `Path` API only).
fn event_touches_target(event: &notify::Event, target: &Path) -> bool {
    // Canonicalize both sides when possible to normalize symlinks/relative paths.
    let target_canon = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    for p in &event.paths {
        let p_canon = p.canonicalize().unwrap_or_else(|_| p.clone());
        if p_canon == target_canon {
            return true;
        }
        // Fallback: compare by final component (covers the case where the file
        // was just created and not yet canonicalizable).
        if p.file_name() == target.file_name() {
            return true;
        }
    }
    false
}

async fn handle_config_change(
    config_path: &std::path::Path,
    agent: &AgentHandle,
    webhook_emitter: Option<&crate::webhook::WebhookEmitter>,
) {
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

    // 2. Reload config and re-register the live-reloadable domains.
    //
    // Model selection, MCP topology, and runtime limits are wired into
    // long-lived subsystems at agent build time and are NOT reloaded here — a
    // restart is required for those to take effect (see module docs).
    let new_config = echo_agent::config::load_config(Some(&path_str));

    infra::load_user_hooks(agent, &new_config).await;
    if let Some(emitter) = webhook_emitter {
        emitter.reload_from_config(&new_config).await;
    }
}
