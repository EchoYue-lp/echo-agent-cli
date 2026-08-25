use std::sync::{Arc, Weak};

use tokio::sync::Mutex;

use super::{
    BootAutoResumeDecision, BootAutoResumeOutcome, TaskRun, TaskRunStatus, TaskRuntimeStore,
};

#[derive(Debug, Clone)]
pub enum TaskRunBootOutcome {
    Resumed(Box<TaskRun>),
    Blocked(Vec<super::store::BootAutoResumeBlocker>),
    Cancelled,
}

/// Store-scoped authority for crash recovery and boot auto-resume admission.
///
/// Product adapters prepare an exact launcher, then call [`Self::resume`].
/// This owner performs recovery once and centralizes policy/deadline handling;
/// it does not own an Agent, renderer, or second TaskRun executor.
pub struct TaskRunBootReconciler {
    store: Weak<TaskRuntimeStore>,
    recovered: Mutex<BootRecoveryState>,
    #[cfg(test)]
    recovery_barrier: std::sync::Mutex<Option<TestRecoveryBarrier>>,
}

#[cfg(test)]
struct TestRecoveryBarrier {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[derive(Default)]
struct BootRecoveryState {
    completed: Option<usize>,
    running: Option<tokio::sync::watch::Receiver<Option<Result<usize, String>>>>,
}

impl TaskRunBootReconciler {
    pub(crate) fn for_store(store: &Arc<TaskRuntimeStore>) -> Arc<Self> {
        store
            .boot_reconciler
            .get_or_init(|| {
                Arc::new(Self {
                    store: Arc::downgrade(store),
                    recovered: Mutex::new(BootRecoveryState::default()),
                    #[cfg(test)]
                    recovery_barrier: std::sync::Mutex::new(None),
                })
            })
            .clone()
    }

    pub async fn recover_once(self: &Arc<Self>) -> Result<usize, String> {
        let mut receipt = {
            let mut recovered = self.recovered.lock().await;
            if let Some(count) = recovered.completed {
                return Ok(count);
            }
            match recovered.running.as_ref() {
                Some(running) => running.clone(),
                None => {
                    let (settled, receipt) = tokio::sync::watch::channel(None);
                    recovered.running = Some(receipt.clone());
                    let owner = Arc::downgrade(self);
                    let store = self.store.clone();
                    tokio::spawn(async move {
                        #[cfg(test)]
                        if let Some(owner) = owner.upgrade() {
                            let barrier = owner
                                .recovery_barrier
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .take();
                            if let Some(barrier) = barrier {
                                let _ = barrier.entered.send(());
                                let _ = barrier.release.await;
                            }
                        }
                        let result =
                            match store.upgrade() {
                                Some(store) => super::TaskRuntimeBlockingAdapter::new(store)
                                    .run_store("recover TaskRuntime at boot", |store| {
                                        store.recover_incomplete()
                                    })
                                    .await
                                    .map_err(|error| error.to_string()),
                                None => Err("TaskRuntimeStore was released before boot recovery"
                                    .to_string()),
                            };
                        if let Some(owner) = owner.upgrade() {
                            let mut state = owner.recovered.lock().await;
                            match &result {
                                Ok(count) => state.completed = Some(*count),
                                Err(_) => state.completed = None,
                            }
                            state.running = None;
                        }
                        let _ = settled.send(Some(result));
                    });
                    receipt
                }
            }
        };
        loop {
            if let Some(result) = receipt.borrow().clone() {
                return result;
            }
            receipt.changed().await.map_err(|_| {
                "TaskRuntime boot recovery owner closed without a receipt".to_string()
            })?;
        }
    }

    #[cfg(test)]
    fn install_recovery_barrier(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self
            .recovery_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(TestRecoveryBarrier {
            entered: entered_tx,
            release: release_rx,
        });
        (entered_rx, release_tx)
    }

    pub async fn paused_candidates(self: &Arc<Self>) -> Result<Vec<TaskRun>, String> {
        self.recover_once().await?;
        let store = self
            .store
            .upgrade()
            .ok_or_else(|| "TaskRuntimeStore was released before candidate listing".to_string())?;
        super::TaskRuntimeBlockingAdapter::new(store)
            .run_store("list boot recovery candidates", |store| {
                store.list_runs_in(&[TaskRunStatus::Paused])
            })
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn decision(
        self: &Arc<Self>,
        run_id: &str,
        launcher_ready: bool,
        interactive_owner_ready: bool,
    ) -> Result<BootAutoResumeDecision, String> {
        self.recover_once().await?;
        let store = self
            .store
            .upgrade()
            .ok_or_else(|| "TaskRuntimeStore was released before boot admission".to_string())?;
        let run_id = run_id.to_string();
        super::TaskRuntimeBlockingAdapter::new(store)
            .run_store("decide boot auto-resume admission", move |store| {
                store.boot_auto_resume_decision(&run_id, launcher_ready, interactive_owner_ready)
            })
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn resume(
        self: &Arc<Self>,
        run_id: &str,
        launcher_ready: bool,
        interactive_owner_ready: bool,
        cancel: &echo_agent::agent::CancellationToken,
    ) -> Result<TaskRunBootOutcome, String> {
        self.recover_once().await?;
        let store = self
            .store
            .upgrade()
            .ok_or_else(|| "TaskRuntimeStore was released before boot admission".to_string())?;
        match self
            .decision(run_id, launcher_ready, interactive_owner_ready)
            .await?
        {
            BootAutoResumeDecision::Blocked(blockers) => {
                return Ok(TaskRunBootOutcome::Blocked(blockers));
            }
            BootAutoResumeDecision::Ready {
                retry_not_before: Some(deadline),
            } => {
                let delay = (deadline - chrono::Utc::now()).to_std().unwrap_or_default();
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(TaskRunBootOutcome::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            BootAutoResumeDecision::Ready { .. } => {}
        }
        let run_id = run_id.to_string();
        let admitted_run_id = run_id.clone();
        let outcome = super::TaskRuntimeBlockingAdapter::new(store)
            .run_store("commit boot auto-resume admission", move |store| {
                store.resume_task_run_after_boot(
                    &admitted_run_id,
                    launcher_ready,
                    interactive_owner_ready,
                )
            })
            .await
            .map_err(|error| error.to_string())?;
        match outcome {
            BootAutoResumeOutcome::Resumed(run) => Ok(TaskRunBootOutcome::Resumed(run)),
            BootAutoResumeOutcome::Blocked(blockers) => Ok(TaskRunBootOutcome::Blocked(blockers)),
            BootAutoResumeOutcome::WaitingUntil(_) => Err(format!(
                "provider retry deadline changed while admitting boot resume for {run_id}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::{
        AttendedMode, DomainProfile, ExecutionMode, PlanTask, TaskPlan,
    };

    fn recoverable_store(attended_mode: AttendedMode) -> Result<Arc<TaskRuntimeStore>, String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let workspace_id = store.active_workspace_id();
        store
            .create_run(
                "ordinary-run",
                &workspace_id,
                "ordinary-conversation",
                "root-message",
                DomainProfile::General,
                "ordinary long task",
                "task",
                attended_mode,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "ordinary-plan".to_string(),
                run_id: "ordinary-run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256("ordinary long task"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "ordinary-task".to_string(),
                    title: "Resume ordinary work".to_string(),
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run("ordinary-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("ordinary-run", true, true, None, None)
            .map_err(|error| error.to_string())?;
        Ok(store)
    }

    #[tokio::test]
    async fn ordinary_unattended_conversation_auto_resumes_after_recovery() -> Result<(), String> {
        let store = recoverable_store(AttendedMode::Unattended)?;
        let reconciler = TaskRunBootReconciler::for_store(&store);
        assert_eq!(reconciler.recover_once().await?, 1);
        let candidates = reconciler.paused_candidates().await?;
        assert_eq!(
            candidates
                .iter()
                .map(|run| run.conversation_id.as_str())
                .collect::<Vec<_>>(),
            vec!["ordinary-conversation"]
        );
        assert!(matches!(
            reconciler
                .resume(
                    "ordinary-run",
                    true,
                    false,
                    &echo_agent::agent::CancellationToken::new(),
                )
                .await?,
            TaskRunBootOutcome::Resumed(run)
                if run.status == TaskRunStatus::Running
                    && run.conversation_id == "ordinary-conversation"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn attended_conversation_waits_for_exact_interactive_owner() -> Result<(), String> {
        let store = recoverable_store(AttendedMode::Attended)?;
        let reconciler = TaskRunBootReconciler::for_store(&store);
        assert_eq!(reconciler.recover_once().await?, 1);
        assert!(matches!(
            reconciler
                .resume(
                    "ordinary-run",
                    true,
                    false,
                    &echo_agent::agent::CancellationToken::new(),
                )
                .await?,
            TaskRunBootOutcome::Blocked(blockers)
                if blockers.contains(
                    &super::super::store::BootAutoResumeBlocker::InteractiveOwnerUnavailable
                )
        ));
        assert_eq!(
            store
                .get_run("ordinary-run")
                .map_err(|error| error.to_string())?
                .map(|run| run.status),
            Some(TaskRunStatus::Paused)
        );
        Ok(())
    }

    #[tokio::test]
    async fn recover_once_is_stable_for_every_adapter() -> Result<(), String> {
        let store = recoverable_store(AttendedMode::Unattended)?;
        let first = TaskRunBootReconciler::for_store(&store);
        let second = TaskRunBootReconciler::for_store(&store);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.recover_once().await?, 1);
        assert_eq!(second.recover_once().await?, 1);
        assert_eq!(
            store
                .recover_incomplete()
                .map_err(|error| error.to_string())?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn transient_recovery_failure_is_not_cached() -> Result<(), String> {
        let store = recoverable_store(AttendedMode::Unattended)?;
        store.fail_next_recovery_commit_for_test();
        let reconciler = TaskRunBootReconciler::for_store(&store);

        let first = reconciler.recover_once().await;
        assert!(matches!(first, Err(error) if error.contains("injected recovery commit failure")));
        assert_eq!(reconciler.recover_once().await?, 1);
        assert_eq!(reconciler.recover_once().await?, 1);
        assert_eq!(
            store
                .get_run("ordinary-run")
                .map_err(|error| error.to_string())?
                .map(|run| run.status),
            Some(TaskRunStatus::Paused)
        );
        Ok(())
    }

    #[tokio::test]
    async fn caller_abort_does_not_cancel_owned_recovery_singleflight() -> Result<(), String> {
        let store = recoverable_store(AttendedMode::Unattended)?;
        let reconciler = TaskRunBootReconciler::for_store(&store);
        let (entered, release) = reconciler.install_recovery_barrier();
        let first_owner = Arc::clone(&reconciler);
        let first = tokio::spawn(async move { first_owner.recover_once().await });
        entered
            .await
            .map_err(|_| "recovery did not reach its owner barrier".to_string())?;
        first.abort();
        let second_owner = Arc::clone(&reconciler);
        let second = tokio::spawn(async move { second_owner.recover_once().await });
        release
            .send(())
            .map_err(|_| "recovery owner dropped its release barrier".to_string())?;
        assert_eq!(
            second.await.map_err(|error| error.to_string())??,
            1,
            "second caller did not share the owner recovery"
        );
        assert_eq!(
            store
                .recover_incomplete()
                .map_err(|error| error.to_string())?,
            0,
            "caller abort caused recovery to execute twice"
        );
        Ok(())
    }
}
