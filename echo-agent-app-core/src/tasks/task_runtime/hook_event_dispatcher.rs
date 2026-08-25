//! HookEventDispatcher — translates the event-sourced `RuntimeEventKind`
//! stream into framework HookEvents.
//!
//! ## Architecture
//!
//! `TaskRuntimeStore` writes every state transition to `events.jsonl` via
//! `TaskRuntimeStore` commit helper (the product mutation chokepoint). After
//! each successful append, `FileTaskShadow` fires the attached event hook with the
//! persisted `RuntimeTaskEvent`. This module's dispatcher is that hook: it
//! maps `RuntimeEventKind` → framework `HookEvent` and fires it through the
//! agent's `TaskHookBridge` / `SubagentHookBridge`.
//!
//! This keeps the store synchronous while giving YAML-configured hooks
//! visibility into the lifecycle. A bounded queue applies backpressure to the
//! synchronous producer and one dedicated consumer fires hooks in persisted
//! event order. Independent `tokio::spawn` calls are deliberately avoided
//! because they can deliver Completed before Started.
//!
//! ## Translation
//!
//! | RuntimeEventKind | Framework HookEvent |
//! |---|---|
//! | `PlanRevisionCommitted` (new node ids) | `TaskCreated` |
//! | `TaskStarted` | `TaskStarted` (PlanTask first claim) |
//! | `TaskCompleted` | `TaskCompleted(status=Completed)` |
//! | `TaskFailed` | `TaskCompleted(status=Failed)` |
//! | `TaskCancelled` | `TaskCompleted(status=Cancelled)` |
//! | `TaskTimedOut` | `TaskCompleted(status=TimedOut)` |
//! | `TaskSkipped` | `TaskCompleted(status=Skipped)` |
//! | `SubagentAssigned` (`dispatch_hook=true`) | `SubagentStart` |
//! | `SubagentReleased` (`dispatch_hook=true`) | `SubagentStop(status)` |

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use echo_agent::hooks::{SubagentStopStatus, TaskTerminalStatus};
use echo_agent::hooks_bridge::{HookCorrelation, SubagentHookBridge, TaskHookBridge};
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
    inner: Option<Arc<DispatcherInner>>,
}

struct DispatcherInner {
    sender: std::sync::Mutex<Option<std::sync::mpsc::SyncSender<QueueCommand>>>,
    shutdown: AtomicBool,
}

enum QueueCommand {
    Event(EventTranslation),
    Flush(tokio::sync::oneshot::Sender<()>),
    Shutdown(tokio::sync::oneshot::Sender<()>),
    #[cfg(test)]
    Pause {
        started: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
}

impl HookEventDispatcher {
    const DEFAULT_QUEUE_CAPACITY: usize = 256;

    pub fn new(
        task_bridge: Option<Arc<TaskHookBridge>>,
        subagent_bridge: Option<Arc<SubagentHookBridge>>,
    ) -> Result<Self, String> {
        Self::new_with_capacity(task_bridge, subagent_bridge, Self::DEFAULT_QUEUE_CAPACITY)
    }

    fn new_with_capacity(
        task_bridge: Option<Arc<TaskHookBridge>>,
        subagent_bridge: Option<Arc<SubagentHookBridge>>,
        capacity: usize,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Failed to build HookEventDispatcher runtime: {error}"))?;
        let (sender, receiver) = std::sync::mpsc::sync_channel::<QueueCommand>(capacity.max(1));
        std::thread::Builder::new()
            .name("eko-hook-dispatcher".to_string())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        QueueCommand::Event(event) => runtime
                            .block_on(event.fire(task_bridge.as_ref(), subagent_bridge.as_ref())),
                        QueueCommand::Flush(acknowledge) => {
                            let _ = acknowledge.send(());
                        }
                        QueueCommand::Shutdown(acknowledge) => {
                            let _ = acknowledge.send(());
                            break;
                        }
                        #[cfg(test)]
                        QueueCommand::Pause { started, release } => {
                            let _ = started.send(());
                            let _ = release.recv();
                        }
                    }
                }
            })
            .map_err(|error| format!("Failed to start HookEventDispatcher: {error}"))?;
        Ok(Self {
            inner: Some(Arc::new(DispatcherInner {
                sender: std::sync::Mutex::new(Some(sender)),
                shutdown: AtomicBool::new(false),
            })),
        })
    }

    /// A dispatcher with no bridges and no queue.
    pub fn inactive() -> Self {
        Self { inner: None }
    }

    /// Enqueue every translation in persisted order.
    ///
    /// The bounded channel blocks when full. This is deliberate backpressure:
    /// lifecycle delivery is never silently dropped merely because hooks are
    /// slower than the task event producer.
    pub fn dispatch(&self, event: &RuntimeTaskEvent) -> Result<(), String> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };
        if inner.shutdown.load(Ordering::Acquire) {
            return Err("HookEventDispatcher is shut down".to_string());
        }
        let sender = inner
            .sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sender = sender
            .as_ref()
            .ok_or_else(|| "HookEventDispatcher is shut down".to_string())?;
        for translation in EventTranslation::from_runtime_event(event) {
            sender
                .send(QueueCommand::Event(translation))
                .map_err(|_| "HookEventDispatcher consumer stopped".to_string())?;
        }
        Ok(())
    }

    /// Wait until every event enqueued before this call has finished firing.
    pub async fn flush(&self) -> Result<(), String> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };
        let (acknowledge, received) = tokio::sync::oneshot::channel();
        let inner = inner.clone();
        tokio::task::spawn_blocking(move || {
            let sender = inner
                .sender
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match sender.as_ref() {
                Some(sender) => sender
                    .send(QueueCommand::Flush(acknowledge))
                    .map_err(|_| "HookEventDispatcher consumer stopped before flush".to_string()),
                None => {
                    let _ = acknowledge.send(());
                    Ok(())
                }
            }
        })
        .await
        .map_err(|error| format!("HookEventDispatcher flush join failed: {error}"))??;
        received
            .await
            .map_err(|_| "HookEventDispatcher flush acknowledgement was lost".to_string())
    }

    /// Drain prior events and stop the consumer. Idempotent across clones.
    pub async fn shutdown(&self) -> Result<(), String> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };
        if inner.shutdown.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let (acknowledge, received) = tokio::sync::oneshot::channel();
        let inner = inner.clone();
        tokio::task::spawn_blocking(move || {
            let sender = inner
                .sender
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            match sender {
                Some(sender) => sender
                    .send(QueueCommand::Shutdown(acknowledge))
                    .map_err(|_| {
                        "HookEventDispatcher consumer stopped before shutdown".to_string()
                    }),
                None => {
                    let _ = acknowledge.send(());
                    Ok(())
                }
            }
        })
        .await
        .map_err(|error| format!("HookEventDispatcher shutdown join failed: {error}"))??;
        received
            .await
            .map_err(|_| "HookEventDispatcher shutdown acknowledgement was lost".to_string())
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
    run_id: String,
    plan_revision: Option<String>,
    subagent_run_id: Option<String>,
    attempt: Option<u32>,
}

enum TranslatedKind {
    TaskCreated,
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
    fn from_runtime_event(event: &RuntimeTaskEvent) -> Vec<Self> {
        if event.event_type == RuntimeEventKind::PlanRevisionCommitted {
            return task_created_translations(event);
        }

        let task_id = event.task_id.clone().unwrap_or_default();
        // `subject`/`title` is carried in the payload for task events;
        // fall back to task_id so the matcher hint is never empty.
        let task_subject = event
            .payload
            .get("title")
            .or_else(|| event.payload.get("summary"))
            .and_then(Value::as_str)
            .unwrap_or(&task_id)
            .to_string();

        let kind = match event.event_type {
            RuntimeEventKind::TaskStarted => TranslatedKind::TaskStarted,
            RuntimeEventKind::TaskCompleted => TranslatedKind::TaskCompleted {
                result: first_str_or(&event.payload, &["summary"], &task_subject),
                status: TaskTerminalStatus::Completed,
            },
            RuntimeEventKind::TaskFailed => TranslatedKind::TaskCompleted {
                result: first_str_or(&event.payload, &["status_detail", "summary"], "failed"),
                status: TaskTerminalStatus::Failed,
            },
            RuntimeEventKind::TaskCancelled => TranslatedKind::TaskCompleted {
                result: first_str_or(&event.payload, &["status_detail", "summary"], "cancelled"),
                status: TaskTerminalStatus::Cancelled,
            },
            RuntimeEventKind::TaskTimedOut => TranslatedKind::TaskCompleted {
                result: first_str_or(&event.payload, &["status_detail", "summary"], "timed out"),
                status: TaskTerminalStatus::TimedOut,
            },
            RuntimeEventKind::TaskSkipped => TranslatedKind::TaskCompleted {
                result: first_str_or(&event.payload, &["status_detail", "summary"], "skipped"),
                status: TaskTerminalStatus::Skipped,
            },
            RuntimeEventKind::SubagentAssigned => {
                if !event
                    .payload
                    .get("dispatch_hook")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return Vec::new();
                }
                let subagent_name = event
                    .payload
                    .get("agent_name")
                    .and_then(Value::as_str)
                    .or_else(|| event.payload.get("owner_agent").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                TranslatedKind::SubagentStart {
                    subagent_name,
                    mode: "direct".to_string(),
                    task: task_subject.clone(),
                }
            }
            RuntimeEventKind::SubagentReleased => {
                if !event
                    .payload
                    .get("dispatch_hook")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return Vec::new();
                }
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
                    mode: "direct".to_string(),
                    result,
                    status,
                }
            }
            // RunCreated / RunStatusChanged / RunCancelled: these are TaskRun
            // (user-goal) level. The framework's Task events are PlanTask
            // level; a future TaskRun hook layer could translate these, but
            // for now they are intentionally not dispatched (consumers observe
            // them via the existing RuntimeEventKind stream / ExecSink).
            _ => return Vec::new(),
        };

        let plan_revision = event
            .payload
            .get("plan_revision")
            .and_then(Value::as_u64)
            .or_else(|| {
                event
                    .payload
                    .get("claim")
                    .and_then(|claim| claim.get("revision"))
                    .and_then(Value::as_u64)
            })
            .map(|revision| revision.to_string());
        let subagent_run_id = event
            .payload
            .get("execution_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let attempt = event
            .payload
            .get("attempt")
            .and_then(Value::as_u64)
            .or_else(|| {
                event
                    .payload
                    .get("claim")
                    .and_then(|claim| claim.get("attempt"))
                    .and_then(Value::as_u64)
            })
            .and_then(|attempt| u32::try_from(attempt).ok());

        vec![Self {
            kind,
            task_id,
            task_subject,
            run_id: event.run_id.clone(),
            plan_revision,
            subagent_run_id,
            attempt,
        }]
    }

    async fn fire(
        self,
        task_bridge: Option<&Arc<TaskHookBridge>>,
        subagent_bridge: Option<&Arc<SubagentHookBridge>>,
    ) {
        let Self {
            kind,
            task_id,
            task_subject,
            run_id,
            plan_revision,
            subagent_run_id,
            attempt,
        } = self;
        let correlation = HookCorrelation {
            run_id: Some(&run_id),
            plan_revision: plan_revision.as_deref(),
            subagent_run_id: subagent_run_id.as_deref(),
            attempt,
        };
        match kind {
            TranslatedKind::TaskCreated => {
                if let Some(b) = task_bridge {
                    b.on_created_with_correlation(&task_id, &task_subject, correlation)
                        .await;
                }
            }
            TranslatedKind::TaskStarted => {
                if let Some(b) = task_bridge {
                    b.on_before_execute_with_correlation(&task_id, &task_subject, correlation)
                        .await;
                }
            }
            TranslatedKind::TaskCompleted { result, status } => {
                if let Some(b) = task_bridge {
                    b.on_after_execute_with_correlation(
                        &task_id,
                        &task_subject,
                        &result,
                        status,
                        correlation,
                    )
                    .await;
                }
            }
            TranslatedKind::SubagentStart {
                subagent_name,
                mode,
                task,
            } => {
                if let Some(b) = subagent_bridge {
                    b.on_before_dispatch_with_correlation(
                        &subagent_name,
                        &mode,
                        &task,
                        correlation,
                    )
                    .await;
                }
            }
            TranslatedKind::SubagentStop {
                subagent_name,
                mode,
                result,
                status,
            } => {
                if let Some(b) = subagent_bridge {
                    b.on_after_dispatch_with_correlation(
                        &subagent_name,
                        &mode,
                        &result,
                        status,
                        correlation,
                    )
                    .await;
                }
            }
        }
    }
}

fn task_created_translations(event: &RuntimeTaskEvent) -> Vec<EventTranslation> {
    let created_ids = event
        .payload
        .get("created_task_ids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let tasks = event
        .payload
        .get("plan")
        .and_then(|plan| plan.get("tasks"))
        .and_then(Value::as_array);
    let plan_revision = event
        .payload
        .get("plan")
        .and_then(|plan| plan.get("revision"))
        .and_then(Value::as_u64)
        .map(|revision| revision.to_string());

    created_ids
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .map(|task_id| {
            let task_subject = tasks
                .and_then(|tasks| {
                    tasks.iter().find(|task| {
                        task.get("id").and_then(Value::as_str) == Some(task_id.as_str())
                    })
                })
                .and_then(|task| task.get("title").and_then(Value::as_str))
                .unwrap_or(&task_id)
                .to_string();
            EventTranslation {
                kind: TranslatedKind::TaskCreated,
                task_id,
                task_subject,
                run_id: event.run_id.clone(),
                plan_revision: plan_revision.clone(),
                subagent_run_id: None,
                attempt: None,
            }
        })
        .collect()
}

/// Read a string field from a JSON payload, falling back to `default`.
fn str_or(payload: &Value, field: &str, default: &str) -> String {
    payload
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

fn first_str_or(payload: &Value, fields: &[&str], default: &str) -> String {
    fields
        .iter()
        .find_map(|field| payload.get(*field).and_then(Value::as_str))
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
        let translated = EventTranslation::from_runtime_event(&ev);
        let Some(t) = translated.first() else {
            assert!(!translated.is_empty(), "TaskStarted should translate");
            return;
        };
        assert!(matches!(t.kind, TranslatedKind::TaskStarted));
        assert_eq!(t.task_id, "t-1");
        assert_eq!(t.run_id, "run-1");
    }

    #[test]
    fn task_failed_translates_to_completed_failed() {
        let ev = make_event(
            RuntimeEventKind::TaskFailed,
            Some("t-1"),
            json!({"status_detail": "compile error"}),
        );
        let translated = EventTranslation::from_runtime_event(&ev);
        assert!(matches!(
            translated.first().map(|translation| &translation.kind),
            Some(TranslatedKind::TaskCompleted {
                status: TaskTerminalStatus::Failed,
                ..
            })
        ));
    }

    #[test]
    fn task_completed_preserves_summary_as_result() {
        let ev = make_event(
            RuntimeEventKind::TaskCompleted,
            Some("t-1"),
            json!({"title": "build", "summary": "built three artifacts"}),
        );
        let translated = EventTranslation::from_runtime_event(&ev);
        assert!(matches!(
            translated.first().map(|translation| &translation.kind),
            Some(TranslatedKind::TaskCompleted { result, .. })
                if result == "built three artifacts"
        ));
    }

    #[test]
    fn task_cancelled_status_is_not_collapsed_to_skipped() {
        let ev = make_event(
            RuntimeEventKind::TaskCancelled,
            Some("t-1"),
            json!({
                "title": "build",
                "status_detail": "cancelled with parent run"
            }),
        );
        let translated = EventTranslation::from_runtime_event(&ev);
        assert!(matches!(
            translated.first().map(|translation| &translation.kind),
            Some(TranslatedKind::TaskCompleted {
                status: TaskTerminalStatus::Cancelled,
                ..
            })
        ));
    }

    #[test]
    fn task_timed_out_status_is_not_collapsed_to_failed() {
        let event = make_event(
            RuntimeEventKind::TaskTimedOut,
            Some("t-1"),
            json!({"status_detail": "provider deadline elapsed"}),
        );
        let translated = EventTranslation::from_runtime_event(&event);
        assert!(matches!(
            translated.first().map(|translation| &translation.kind),
            Some(TranslatedKind::TaskCompleted {
                status: TaskTerminalStatus::TimedOut,
                ..
            })
        ));
    }

    #[test]
    fn plan_commit_translates_each_new_node_to_task_created() {
        let ev = make_event(
            RuntimeEventKind::PlanRevisionCommitted,
            None,
            json!({
                "created_task_ids": ["t-2"],
                "plan": {
                    "revision": 3,
                    "tasks": [
                        {"id": "t-1", "title": "existing"},
                        {"id": "t-2", "title": "new task"}
                    ]
                }
            }),
        );
        let translated = EventTranslation::from_runtime_event(&ev);
        assert_eq!(translated.len(), 1);
        assert!(matches!(
            translated.first().map(|translation| &translation.kind),
            Some(TranslatedKind::TaskCreated)
        ));
        assert_eq!(
            translated
                .first()
                .map(|translation| translation.task_id.as_str()),
            Some("t-2")
        );
        assert_eq!(
            translated
                .first()
                .and_then(|translation| translation.plan_revision.as_deref()),
            Some("3")
        );
    }

    #[test]
    fn subagent_released_maps_status() {
        let ev = make_event(
            RuntimeEventKind::SubagentReleased,
            Some("t-1"),
            json!({
                "agent_name": "coder",
                "status": "timed_out",
                "summary": "deadline",
                "dispatch_hook": true,
                "execution_id": "run-1:t-1:2:3",
                "plan_revision": 2,
                "attempt": 3
            }),
        );
        let translated = EventTranslation::from_runtime_event(&ev);
        let Some(t) = translated.first() else {
            assert!(!translated.is_empty(), "SubagentReleased should translate");
            return;
        };
        assert!(matches!(
            &t.kind,
            TranslatedKind::SubagentStop {
                status: SubagentStopStatus::TimedOut,
                subagent_name,
                result,
                ..
            } if subagent_name == "coder" && result == "deadline"
        ));
        assert_eq!(t.subagent_run_id.as_deref(), Some("run-1:t-1:2:3"));
        assert_eq!(t.plan_revision.as_deref(), Some("2"));
        assert_eq!(t.attempt, Some(3));
    }

    #[test]
    fn framework_owned_subagent_events_are_not_dispatched_again() {
        let ev = make_event(
            RuntimeEventKind::SubagentAssigned,
            Some("t-1"),
            json!({"agent_name": "coder", "dispatch_hook": false}),
        );
        assert!(EventTranslation::from_runtime_event(&ev).is_empty());
    }

    #[test]
    fn unhandled_events_skip() {
        // ToolStarted / Note and plan commits with no new nodes are skipped.
        let ev = make_event(RuntimeEventKind::ToolStarted, Some("t-1"), json!({}));
        assert!(EventTranslation::from_runtime_event(&ev).is_empty());
        let ev = make_event(RuntimeEventKind::Note, None, json!({}));
        assert!(EventTranslation::from_runtime_event(&ev).is_empty());
        let ev = make_event(RuntimeEventKind::PlanRevisionCommitted, None, json!({}));
        assert!(EventTranslation::from_runtime_event(&ev).is_empty());
    }

    #[test]
    fn inactive_dispatcher_drops_event() {
        // No bridges → dispatch returns without spawning; just verify no panic.
        let d = HookEventDispatcher::inactive();
        let ev = make_event(RuntimeEventKind::TaskStarted, Some("t-1"), json!({}));
        assert!(d.dispatch(&ev).is_ok());
    }

    #[tokio::test]
    async fn flush_and_shutdown_are_explicit_and_idempotent() -> Result<(), String> {
        let dispatcher = HookEventDispatcher::new_with_capacity(None, None, 1)?;
        let event = make_event(RuntimeEventKind::TaskStarted, Some("t-1"), json!({}));
        dispatcher.dispatch(&event)?;
        dispatcher.flush().await?;
        dispatcher.shutdown().await?;
        dispatcher.shutdown().await?;
        assert!(dispatcher.dispatch(&event).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn bounded_queue_applies_backpressure_without_dropping() -> Result<(), String> {
        let dispatcher = HookEventDispatcher::new_with_capacity(None, None, 1)?;
        let inner = dispatcher
            .inner
            .as_ref()
            .ok_or_else(|| "active dispatcher has no inner state".to_string())?;
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        inner
            .sender
            .lock()
            .map_err(|_| "dispatcher sender lock poisoned".to_string())?
            .as_ref()
            .ok_or_else(|| "dispatcher sender missing".to_string())?
            .send(QueueCommand::Pause {
                started: started_tx,
                release: release_rx,
            })
            .map_err(|_| "failed to pause dispatcher".to_string())?;
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .map_err(|error| format!("dispatcher did not pause: {error}"))?;

        let event = make_event(RuntimeEventKind::TaskStarted, Some("t-1"), json!({}));
        dispatcher.dispatch(&event)?;
        let blocked_dispatcher = dispatcher.clone();
        let (finished_tx, mut finished_rx) = tokio::sync::oneshot::channel();
        let producer = std::thread::spawn(move || {
            let result = blocked_dispatcher.dispatch(&event);
            let _ = finished_tx.send(result);
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut finished_rx)
                .await
                .is_err(),
            "dispatch unexpectedly bypassed a saturated queue"
        );

        release_tx
            .send(())
            .map_err(|_| "failed to release dispatcher".to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut finished_rx)
            .await
            .map_err(|_| "blocked dispatch did not resume".to_string())?
            .map_err(|_| "blocked dispatch result was lost".to_string())??;
        producer
            .join()
            .map_err(|_| "blocked dispatch thread panicked".to_string())?;
        dispatcher.shutdown().await?;
        Ok(())
    }
}
