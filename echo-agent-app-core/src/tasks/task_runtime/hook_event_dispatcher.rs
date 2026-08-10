//! HookEventDispatcher — translates the event-sourced `RuntimeEventKind`
//! stream into framework HookEvents.
//!
//! ## Architecture
//!
//! `TaskRuntimeStore` writes every state transition to `events.jsonl` via
//! `FileTaskShadow::append_event_line` (the single chokepoint). After each
//! successful append, `FileTaskShadow` fires the attached event hook with the
//! persisted `RuntimeTaskEvent`. This module's dispatcher is that hook: it
//! maps `RuntimeEventKind` → framework `HookEvent` and fires it through the
//! agent's `TaskHookBridge` / `SubagentHookBridge`.
//!
//! This keeps the store synchronous and pure (no async, no bridge threading
//! through every chokepoint), while giving YAML-configured hooks visibility
//! into the full task/subagent lifecycle. The dispatcher spawns each fire as
//! a detached tokio task so the sync hook callback never blocks the store's
//! per-run write lock.
//!
//! ## Translation (P1 — TaskCreated pending)
//!
//! | RuntimeEventKind | Framework HookEvent |
//! |---|---|
//! | `TaskStarted` | `TaskStarted` (PlanTask first claim) |
//! | `TaskCompleted` | `TaskCompleted(status=Completed)` |
//! | `TaskFailed` | `TaskCompleted(status=Failed)` |
//! | `TaskSkipped` | `TaskCompleted(status=Skipped)` |
//! | `TaskBlocked` | `TaskCompleted(status=Blocked)` |
//! | `SubagentAssigned` | `SubagentStart` |
//! | `SubagentReleased` | `SubagentStop(status)` (from payload) |
//!
//! `PlanRevisionCommitted` → `TaskCreated` (per new node) is deferred: it
//! requires diffing the committed revision against the previous one to find
//! newly-added task ids, which is a richer change than a status transition.
//! Tracked in MASTER-PLAN as P2. For now, consumers that need "task entered
//! the graph" can observe `PlanRevisionCommitted` directly via the event log.

use std::sync::Arc;

use echo_agent::hooks_bridge::{SubagentHookBridge, TaskHookBridge};
use echo_core::hooks::{SubagentStopStatus, TaskTerminalStatus};
use serde_json::Value;

use super::types::{RuntimeEventKind, RuntimeTaskEvent};

/// Dispatcher that observes the RuntimeEventKind stream and fires framework
/// HookEvents through the supplied bridges.
///
/// Cloned cheaply (Arc internals). The bridges are optional so the dispatcher
/// can be attached even before bridges exist; events are simply dropped while
/// bridges are absent (e.g. during early bootstrap / in-memory test stores).
#[derive(Clone)]
pub struct HookEventDispatcher {
    /// Task lifecycle bridge (TaskCreated/Started/Completed). None = task
    /// events dropped (no hook registry wired, e.g. in-memory test store).
    task_bridge: Option<Arc<TaskHookBridge>>,
    /// Subagent lifecycle bridge (SubagentStart/Stop). None = subagent events
    /// dropped.
    subagent_bridge: Option<Arc<SubagentHookBridge>>,
}

impl HookEventDispatcher {
    pub fn new(
        task_bridge: Option<Arc<TaskHookBridge>>,
        subagent_bridge: Option<Arc<SubagentHookBridge>>,
    ) -> Self {
        Self {
            task_bridge,
            subagent_bridge,
        }
    }

    /// A dispatcher with no bridges — all events dropped. Useful as a
    /// placeholder when constructing a store before bridges exist.
    pub fn inactive() -> Self {
        Self {
            task_bridge: None,
            subagent_bridge: None,
        }
    }

    /// The synchronous hook entry point — attached to `FileTaskShadow`.
    ///
    /// Translates the `RuntimeTaskEvent` into the corresponding framework
    /// HookEvent and spawns a detached task to fire it through the bridge.
    /// Must be cheap (spawn-and-detach) because it runs under the per-run
    /// write lock.
    pub fn dispatch(&self, event: &RuntimeTaskEvent) {
        // No bridges → nothing to do. Avoid spawning work that will no-op.
        if self.task_bridge.is_none() && self.subagent_bridge.is_none() {
            return;
        }
        let Some(span) = EventTranslation::from_runtime_event(event) else {
            return;
        };
        let task_bridge = self.task_bridge.clone();
        let subagent_bridge = self.subagent_bridge.clone();
        // Detached spawn: the store's write lock must not wait on async hook
        // execution. A failed/slow hook is logged inside the spawned task and
        // never blocks the event-sourcing write path.
        tokio::spawn(async move {
            span.fire(task_bridge.as_ref(), subagent_bridge.as_ref())
                .await;
        });
    }
}

/// A single translated event waiting to be fired through a bridge.
///
/// Pre-translating (instead of passing the raw RuntimeTaskEvent into the
/// spawned task) keeps the async side free of RuntimeEventKind/payload parsing
/// and lets us early-return (skip) for events we don't translate.
struct EventTranslation {
    kind: TranslatedKind,
    /// Correlation fields copied from the RuntimeTaskEvent / payload.
    task_id: String,
    task_subject: String,
}

enum TranslatedKind {
    TaskStarted,
    TaskCompleted {
        result: String,
        status: TaskTerminalStatus,
    },
    SubagentStart {
        subagent_name: String,
        mode: String,
        task: String,
    },
    SubagentStop {
        subagent_name: String,
        mode: String,
        result: String,
        status: SubagentStopStatus,
    },
}

impl EventTranslation {
    /// Translate a RuntimeTaskEvent into a fireable envelope, or None if the
    /// event kind is not in the dispatch table (e.g. ToolStarted, Note,
    /// PlanRevisionCommitted — see module docs).
    fn from_runtime_event(event: &RuntimeTaskEvent) -> Option<Self> {
        let task_id = event.task_id.clone().unwrap_or_default();
        // `subject`/`title` is carried in the payload for task events;
        // fall back to task_id so the matcher hint is never empty.
        let task_subject = event
            .payload
            .get("summary")
            .or_else(|| event.payload.get("title"))
            .and_then(Value::as_str)
            .unwrap_or(&task_id)
            .to_string();

        let kind = match event.event_type {
            RuntimeEventKind::TaskStarted => TranslatedKind::TaskStarted,
            RuntimeEventKind::TaskCompleted => TranslatedKind::TaskCompleted {
                result: task_subject.clone(),
                status: TaskTerminalStatus::Completed,
            },
            RuntimeEventKind::TaskFailed => TranslatedKind::TaskCompleted {
                result: str_or(&event.payload, "status_detail", "failed"),
                status: TaskTerminalStatus::Failed,
            },
            RuntimeEventKind::TaskSkipped => TranslatedKind::TaskCompleted {
                result: str_or(&event.payload, "status_detail", "skipped"),
                status: TaskTerminalStatus::Skipped,
            },
            RuntimeEventKind::TaskBlocked => TranslatedKind::TaskCompleted {
                result: str_or(&event.payload, "status_detail", "blocked"),
                status: TaskTerminalStatus::Blocked,
            },
            RuntimeEventKind::SubagentAssigned => {
                let subagent_name = event
                    .payload
                    .get("agent_name")
                    .and_then(Value::as_str)
                    .or_else(|| event.payload.get("owner_agent").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                TranslatedKind::SubagentStart {
                    subagent_name,
                    mode: "sync".to_string(),
                    task: task_subject.clone(),
                }
            }
            RuntimeEventKind::SubagentReleased => {
                let subagent_name = event
                    .payload
                    .get("agent_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let status_str = str_or(&event.payload, "status", "completed");
                let status = match status_str.as_str() {
                    "failed" => SubagentStopStatus::Failed,
                    "cancelled" => SubagentStopStatus::Cancelled,
                    "timed_out" => SubagentStopStatus::TimedOut,
                    _ => SubagentStopStatus::Completed,
                };
                let result = str_or(&event.payload, "summary", &status_str);
                TranslatedKind::SubagentStop {
                    subagent_name,
                    mode: "sync".to_string(),
                    result,
                    status,
                }
            }
            // RunCreated / RunStatusChanged / RunCancelled: these are TaskRun
            // (user-goal) level. The framework's Task events are PlanTask
            // level; a future TaskRun hook layer could translate these, but
            // for now they are intentionally not dispatched (consumers observe
            // them via the existing RuntimeEventKind stream / ExecSink).
            // PlanRevisionCommitted → TaskCreated is P2 (see module docs).
            _ => return None,
        };

        Some(Self {
            kind,
            task_id,
            task_subject,
        })
    }

    async fn fire(
        self,
        task_bridge: Option<&Arc<TaskHookBridge>>,
        subagent_bridge: Option<&Arc<SubagentHookBridge>>,
    ) {
        match self.kind {
            TranslatedKind::TaskStarted => {
                if let Some(b) = task_bridge {
                    b.on_before_execute(&self.task_id, &self.task_subject).await;
                }
            }
            TranslatedKind::TaskCompleted { result, status } => {
                if let Some(b) = task_bridge {
                    b.on_after_execute(&self.task_id, &self.task_subject, &result, status)
                        .await;
                }
            }
            TranslatedKind::SubagentStart {
                subagent_name,
                mode,
                task,
            } => {
                if let Some(b) = subagent_bridge {
                    b.on_before_dispatch(&subagent_name, &mode, &task).await;
                }
            }
            TranslatedKind::SubagentStop {
                subagent_name,
                mode,
                result,
                status,
            } => {
                if let Some(b) = subagent_bridge {
                    b.on_after_dispatch(&subagent_name, &mode, &result, status)
                        .await;
                }
            }
        }
    }
}

/// Read a string field from a JSON payload, falling back to `default`.
fn str_or(payload: &Value, field: &str, default: &str) -> String {
    payload
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn make_event(
        event_type: RuntimeEventKind,
        task_id: Option<&str>,
        payload: Value,
    ) -> RuntimeTaskEvent {
        RuntimeTaskEvent {
            seq: 1,
            run_id: "run-1".to_string(),
            task_id: task_id.map(str::to_string),
            step_id: None,
            event_type,
            payload,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn task_started_translates() {
        let ev = make_event(
            RuntimeEventKind::TaskStarted,
            Some("t-1"),
            json!({"summary": "build"}),
        );
        let t = EventTranslation::from_runtime_event(&ev).expect("TaskStarted should translate");
        assert!(matches!(t.kind, TranslatedKind::TaskStarted));
        assert_eq!(t.task_id, "t-1");
    }

    #[test]
    fn task_failed_translates_to_completed_failed() {
        let ev = make_event(
            RuntimeEventKind::TaskFailed,
            Some("t-1"),
            json!({"status_detail": "compile error"}),
        );
        let t = EventTranslation::from_runtime_event(&ev).expect("TaskFailed should translate");
        match t.kind {
            TranslatedKind::TaskCompleted { status, .. } => {
                assert_eq!(status, TaskTerminalStatus::Failed);
            }
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn subagent_released_maps_status() {
        let ev = make_event(
            RuntimeEventKind::SubagentReleased,
            Some("t-1"),
            json!({"agent_name": "coder", "status": "timed_out", "summary": "deadline"}),
        );
        let t =
            EventTranslation::from_runtime_event(&ev).expect("SubagentReleased should translate");
        match t.kind {
            TranslatedKind::SubagentStop {
                status,
                subagent_name,
                result,
                ..
            } => {
                assert_eq!(status, SubagentStopStatus::TimedOut);
                assert_eq!(subagent_name, "coder");
                assert_eq!(result, "deadline");
            }
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn unhandled_events_skip() {
        // ToolStarted / Note / PlanRevisionCommitted are not dispatched.
        let ev = make_event(RuntimeEventKind::ToolStarted, Some("t-1"), json!({}));
        assert!(EventTranslation::from_runtime_event(&ev).is_none());
        let ev = make_event(RuntimeEventKind::Note, None, json!({}));
        assert!(EventTranslation::from_runtime_event(&ev).is_none());
        let ev = make_event(RuntimeEventKind::PlanRevisionCommitted, None, json!({}));
        assert!(EventTranslation::from_runtime_event(&ev).is_none());
    }

    #[test]
    fn inactive_dispatcher_drops_event() {
        // No bridges → dispatch returns without spawning; just verify no panic.
        let d = HookEventDispatcher::inactive();
        let ev = make_event(RuntimeEventKind::TaskStarted, Some("t-1"), json!({}));
        d.dispatch(&ev);
    }
}
