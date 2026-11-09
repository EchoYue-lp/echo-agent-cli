//! 同一Workspace的通信代际。这里只管理连接和请求lifetime，不拥有任务或远端结果。
use super::ProjectSshConnection;
use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

type SshCloseSettlement = Shared<BoxFuture<'static, Result<(), String>>>;

#[derive(Default)]
pub(super) struct RequestAdmission {
    state: Mutex<(usize, bool)>,
    changed: Notify,
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::sandbox::{ResourceLimits, SandboxCommand, SandboxExecutor};
    fn fixture(
        service: &Arc<super::super::TerminalService>,
        id: &str,
    ) -> Result<Arc<ProjectSshConnection>, String> {
        ProjectSshConnection::prepare(
            service,
            id,
            super::super::TerminalTarget::Ssh {
                host: "fixture.invalid".into(),
                user: Some("fixture".into()),
                port: Some(22),
            },
        )
    }

    #[tokio::test]
    async fn generation_cas_substeps_keep_admitted_snapshot_after_probe()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = super::super::TerminalService::new();
        let original = fixture(&service, "cas-before")?;
        let next = fixture(&service, "cas-after")?;
        let target = original.target.clone();
        original
            .effective_target
            .set(target.clone())
            .map_err(|_| "duplicate target")?;
        next.effective_target
            .set(target)
            .map_err(|_| "duplicate target")?;
        let owner = ProjectSshOwner::new(original.clone());
        let snapshot = owner.capture()?;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let invoked = calls.clone();
        let weak_owner = Arc::downgrade(&owner);
        let weak_original = Arc::downgrade(&original);
        *original
            .test_execution_launcher
            .lock()
            .map_err(|e| e.to_string())? = Some(Arc::new(move |_| {
            let first = invoked.fetch_add(1, Ordering::AcqRel) == 0;
            if first {
                let owner = weak_owner
                    .upgrade()
                    .ok_or_else(|| super::super::sftp::unavailable("fixture owner lost"))?;
                let original = weak_original
                    .upgrade()
                    .ok_or_else(|| super::super::sftp::unavailable("fixture original lost"))?;
                owner
                    .publish(&original, next.clone())
                    .map_err(super::super::sftp::unavailable)?;
            }
            let mut child = tokio::process::Command::new("/bin/sh");
            child.args([
                "-c",
                if first {
                    "printf '%s' '{\"result\":\"available\"}'"
                } else {
                    "cat >/dev/null; printf '%s' '{\"result\":\"replaced\"}'"
                },
            ]);
            Ok(child)
        }));
        let result = super::super::cas::compare_and_swap(
            ConnectionSource::Admitted(snapshot.clone()),
            std::path::PathBuf::from("/"),
            std::path::PathBuf::from("/fixture-file"),
            b"before",
            b"after",
        )
        .await;
        *original
            .test_execution_launcher
            .lock()
            .map_err(|e| e.to_string())? = None;
        drop(snapshot);
        owner
            .close(
                &service,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .await?;
        assert!(
            matches!(result, Ok(true)),
            "CAS mutation must use its admitted generation after probe: {result:?}"
        );
        assert_eq!(calls.load(Ordering::Acquire), 2);
        Ok(())
    }

    #[tokio::test]
    async fn generation_snapshots_pin_all_executor_entries_before_poll() -> Result<(), String> {
        let service = super::super::TerminalService::new();
        for mode in 0..4 {
            let original = fixture(&service, "before-poll")?;
            let next = fixture(&service, "after-poll")?;
            let owner = ProjectSshOwner::new(original.clone());
            let sandbox = super::super::sandbox::ProjectSshSandbox::with_source(
                ConnectionSource::Current(owner.clone()),
            );
            let command = SandboxCommand::program("true", Vec::new());
            let pending: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> =
                match mode {
                    0 => {
                        let future = sandbox.execute(command);
                        Box::pin(async move {
                            let _result = future.await;
                        })
                    }
                    1 => {
                        let future = sandbox.execute_stream(command);
                        Box::pin(async move {
                            let _result = future.await;
                        })
                    }
                    2 => {
                        let future =
                            sandbox.execute_with_limits(command, ResourceLimits::unrestricted());
                        Box::pin(async move {
                            let _result = future.await;
                        })
                    }
                    _ => {
                        let future = sandbox.execute_with_limits_and_cancel(
                            command,
                            ResourceLimits::unrestricted(),
                            None,
                        );
                        Box::pin(async move {
                            let _result = future.await;
                        })
                    }
                };
            assert_eq!(
                original.requests.state.lock().map_err(|e| e.to_string())?.0,
                1
            );
            owner.publish(&original, next.clone())?;
            owner.reap_retired(&service).await;
            assert!(
                !original.closing.load(Ordering::Acquire),
                "an admitted unpolled request must not be retired"
            );
            let new = owner.capture()?;
            assert!(Arc::ptr_eq(&new.connection, &next));
            assert_eq!(next.requests.state.lock().map_err(|e| e.to_string())?.0, 1);
            drop(new);
            drop(pending); // 尚未poll：不启动本机ssh或任何远端命令。
            assert_eq!(
                original.requests.state.lock().map_err(|e| e.to_string())?.0,
                0
            );
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while owner.connections().len() != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| "retired generation was not automatically reaped".to_string())?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn generation_pending_cancelled_close_reaps_known_retired_after_actual_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = super::super::TerminalService::new();
        let old = fixture(&service, "cancel-reap-unknown")?;
        let owner = ProjectSshOwner::new(old.clone());
        let old_snapshot = owner.capture()?;
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()?;
        let resource =
            old.retain_remote_operation(&child, old.target.clone(), old.operation()?, Vec::new());
        let proof = child.wait().await?;
        resource.mark_stop_unknown("self-owned fixture stop observation intentionally withheld");
        let known = fixture(&service, "cancel-reap-known")?;
        owner.publish(&old, known.clone())?;
        let known_snapshot = owner.capture()?;
        let current = fixture(&service, "cancel-reap-current")?;
        owner.publish(&known, current.clone())?;
        drop(old_snapshot);
        let observed_owner = owner.clone();
        let observed_service = service.clone();
        let observer = tokio::spawn(async move {
            observed_owner
                .close(
                    &observed_service,
                    tokio::time::Instant::now() + std::time::Duration::from_secs(2),
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while owner.cleanup.try_lock().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        observer.abort();
        let _observation = observer.await;
        drop(known_snapshot);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while owner.reap_scheduled.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        // 直接等原共享物理结算，不调用外层close/reap制造额外触发。
        let actual = owner
            .close_settlement
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("original close settlement missing")?;
        assert!(actual.await.is_err());
        let reaped = tokio::time::timeout(std::time::Duration::from_millis(500), async {
            while owner.connections().iter().any(|c| Arc::ptr_eq(c, &known)) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        let unknown_kept = resource.stop_is_unknown() && !resource.is_settled();
        assert!(proof.success());
        resource.complete_with_proof(); // 唯一自有本机fixture清理证明，不是SSH恢复API。
        owner
            .close(
                &service,
                tokio::time::Instant::now() + std::time::Duration::from_secs(2),
            )
            .await?;
        assert!(
            reaped && unknown_kept,
            "cancelled observer lost known-retired progress while another generation remained unknown"
        );
        Ok(())
    }

    #[tokio::test]
    async fn generation_close_shares_deadline_and_keeps_admitted_substeps() -> Result<(), String> {
        let service = super::super::TerminalService::new();
        let original = fixture(&service, "lifetime-original")?;
        let owner = ProjectSshOwner::new(original.clone());
        let snapshot = owner.capture()?;
        let next = fixture(&service, "lifetime-next")?;
        owner.publish(&original, next.clone())?;
        assert!(
            ConnectionSource::Physical(original.clone())
                .capture()
                .is_err()
        );
        let pinned = ConnectionSource::Admitted(snapshot.clone()).capture()?;
        assert!(Arc::ptr_eq(&pinned.connection, &original));
        let second = fixture(&service, "lifetime-second")?;
        let next_snapshot = owner.capture()?;
        owner.publish(&next, second.clone())?;
        let started = std::time::Instant::now();
        let result = owner
            .close(
                &service,
                tokio::time::Instant::now() + std::time::Duration::from_millis(40),
            )
            .await;
        assert!(result.is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(!original.closing.load(Ordering::Acquire));
        let current = owner.capture()?;
        assert!(Arc::ptr_eq(&current.connection, &second));
        assert!(
            original.operation().is_ok(),
            "accepted composite substeps still use original physical connection"
        );
        drop((current, snapshot, pinned, next_snapshot));
        owner
            .close(
                &service,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .await?;
        Ok(())
    }
}

struct RequestLifetime(Arc<ProjectSshConnection>);

impl Drop for RequestLifetime {
    fn drop(&mut self) {
        let mut state = self
            .0
            .requests
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.0 = state.0.saturating_sub(1);
        drop(state);
        self.0.requests.changed.notify_waiters();
        self.0.note_request_progress();
    }
}

#[derive(Clone)]
pub(super) struct RequestSnapshot {
    pub(super) connection: Arc<ProjectSshConnection>,
    _lifetime: Arc<RequestLifetime>,
}

impl RequestSnapshot {
    fn capture(connection: Arc<ProjectSshConnection>) -> Result<Self, String> {
        let mut state = connection
            .requests
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.1 {
            return Err("SSH generation no longer admits new requests".into());
        }
        state.0 = state
            .0
            .checked_add(1)
            .ok_or_else(|| "SSH request counter exhausted".to_string())?;
        drop(state);
        Ok(Self {
            connection: connection.clone(),
            _lifetime: Arc::new(RequestLifetime(connection)),
        })
    }
}

#[derive(Clone)]
pub(super) enum ConnectionSource {
    Physical(Arc<ProjectSshConnection>),
    Current(Arc<ProjectSshOwner>),
    Admitted(RequestSnapshot),
}

impl ConnectionSource {
    pub(super) fn capture(&self) -> Result<RequestSnapshot, String> {
        match self {
            Self::Physical(connection) => RequestSnapshot::capture(connection.clone()),
            Self::Current(owner) => owner.capture(),
            Self::Admitted(snapshot) => Ok(snapshot.clone()),
        }
    }
    pub(super) fn connections(&self) -> Vec<Arc<ProjectSshConnection>> {
        match self {
            Self::Current(owner) => owner.connections(),
            Self::Physical(connection) => vec![connection.clone()],
            Self::Admitted(snapshot) => vec![snapshot.connection.clone()],
        }
    }
}

struct OwnerState {
    current: Arc<ProjectSshConnection>,
    retired: Vec<Arc<ProjectSshConnection>>,
    closing: bool,
    published_workspace: bool,
}

pub(in crate::terminal) struct ProjectSshOwner {
    state: Mutex<OwnerState>,
    cleanup: tokio::sync::Mutex<()>,
    close_settlement: Mutex<Option<SshCloseSettlement>>,
    reap_scheduled: AtomicBool,
    reap_revision: AtomicUsize,
    runtime: Option<tokio::runtime::Handle>,
}

impl ProjectSshOwner {
    pub(in crate::terminal) fn new(connection: Arc<ProjectSshConnection>) -> Arc<Self> {
        let owner = Arc::new(Self {
            state: Mutex::new(OwnerState {
                current: connection.clone(),
                retired: Vec::new(),
                closing: false,
                published_workspace: false,
            }),
            cleanup: tokio::sync::Mutex::new(()),
            close_settlement: Mutex::new(None),
            reap_scheduled: AtomicBool::new(false),
            reap_revision: AtomicUsize::new(0),
            runtime: tokio::runtime::Handle::try_current().ok(),
        });
        *connection
            .request_owner
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&owner);
        owner
    }

    // 本地并行认证/取消也会误移其它Workspace的资源，故核owner归属；不检查用户权限模式。
    pub(super) fn ensure_draft_open(
        &self,
        expected: &Arc<ProjectSshConnection>,
    ) -> Result<(), String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing
            || state.published_workspace
            || !state.retired.is_empty()
            || !Arc::ptr_eq(&state.current, expected)
            || state
                .current
                .requests
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .1
        {
            return Err("SSH source is closing or already belongs to a Workspace; an independent authentication draft is required".into());
        }
        Ok(())
    }

    pub(super) fn mark_workspace(
        &self,
        expected: &Arc<ProjectSshConnection>,
    ) -> Result<(), String> {
        self.ensure_draft_open(expected)?;
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .published_workspace = true;
        Ok(())
    }
    pub(in crate::terminal) fn current(&self) -> Arc<ProjectSshConnection> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .clone()
    }
    pub(in crate::terminal) fn connections(&self) -> Vec<Arc<ProjectSshConnection>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        std::iter::once(state.current.clone())
            .chain(state.retired.iter().cloned())
            .collect()
    }
    pub(in crate::terminal) fn contains(&self, connection: &Arc<ProjectSshConnection>) -> bool {
        self.connections()
            .iter()
            .any(|candidate| Arc::ptr_eq(candidate, connection))
    }
    pub(in crate::terminal) fn master_ids(&self) -> Vec<String> {
        self.connections()
            .iter()
            .map(|c| c.master_terminal_id().to_string())
            .collect()
    }
    pub(super) fn capture(&self) -> Result<RequestSnapshot, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return Err("project SSH owner is closing; request not admitted".into());
        }
        // 与publish同锁：不能读出旧current后在退休关闭已开始时才登记请求。
        RequestSnapshot::capture(state.current.clone())
    }
    /// caller持原创建和会话锁，并已验证目录/目标及draft master身份。
    pub(super) fn publish(
        self: &Arc<Self>,
        expected: &Arc<ProjectSshConnection>,
        next: Arc<ProjectSshConnection>,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing || !Arc::ptr_eq(&state.current, expected) {
            return Err(
                "project SSH generation changed or is closing; reconnect not published".into(),
            );
        }
        // 只封旧代际的新请求；已admitted快照的子步骤仍可用原physical连接。
        expected
            .requests
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1 = true;
        let previous = std::mem::replace(&mut state.current, next);
        *state
            .current
            .request_owner
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(self);
        state.retired.push(previous);
        Ok(())
    }
    pub(in crate::terminal) fn begin_shutdown(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closing = true;
    }
    fn reopen_current(&self, service: &super::TerminalService) {
        if service.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current.reopen_after_failed_close();
        state
            .current
            .requests
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1 = false;
        state.closing = false;
    }
    async fn idle(
        connection: &Arc<ProjectSshConnection>,
        deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        loop {
            let changed = connection.requests.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let count = {
                let mut state = connection
                    .requests
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                state.1 = true;
                state.0
            };
            if count == 0 {
                break;
            }
            tokio::time::timeout_at(deadline, changed)
                .await
                .map_err(|_| "original SSH request snapshot remains active".to_string())?;
        }
        connection.observe_idle_before(deadline).await
    }
    pub(in crate::terminal) async fn close(
        self: &Arc<Self>,
        service: &super::TerminalService,
        deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        let original_service = self
            .current()
            .service
            .upgrade()
            .ok_or_else(|| "original SSH service no longer exists".to_string())?;
        if !std::ptr::eq(service, original_service.as_ref()) {
            return Err("SSH close belongs to another service owner".into());
        }
        let settlement = {
            let mut slot = self
                .close_settlement
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(previous) = slot.as_ref().filter(|s| !matches!(s.peek(), Some(Err(_)))) {
                previous.clone()
            } else {
                self.begin_shutdown();
                let service = original_service;
                let owner = self.clone();
                let task = tokio::spawn(async move {
                    let result = owner.close_inner(&service, deadline).await;
                    // 实际结算持有后续回收责任，不能依赖仍有人轮询close观察者。
                    if result.is_err() && !service.shutting_down.load(Ordering::Acquire) {
                        owner.schedule_reap();
                    }
                    result
                });
                let settlement = async move {
                    task.await
                        .map_err(|e| format!("original SSH close task did not settle: {e}"))?
                }
                .boxed()
                .shared();
                *slot = Some(settlement.clone());
                settlement
            }
        };
        settlement.await
    }

    async fn close_inner(
        &self,
        service: &super::TerminalService,
        deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        self.begin_shutdown();
        let _cleanup = match tokio::time::timeout_at(deadline, self.cleanup.lock()).await {
            Ok(guard) => guard,
            Err(_) => {
                self.reopen_current(service);
                return Err("SSH close is already being observed".into());
            }
        };
        let connections = self.connections();
        let results =
            futures::future::join_all(connections.iter().map(|c| Self::idle(c, deadline))).await;
        let mut errors = results
            .into_iter()
            .zip(&connections)
            .filter_map(|(result, c)| {
                result
                    .err()
                    .map(|e| format!("generation {}: {e}", c.generation()))
            })
            .collect::<Vec<_>>();
        // 先证明所有代际空闲，再关master；旧unknown不应使仍可查询的current被误关。
        if errors.is_empty() {
            for result in futures::future::join_all(connections.iter().map(|c| async move {
                tokio::time::timeout_at(deadline, service.close_session(c.master_terminal_id()))
                    .await
                    .map_err(|_| "original SSH master close remains unconfirmed".to_string())?
                    .map(|_| ())
            }))
            .await
            {
                if let Err(error) = result {
                    errors.push(error);
                }
            }
        }
        if !errors.is_empty() {
            self.reopen_current(service);
            return Err(errors.join("; "));
        }
        Ok(())
    }
    pub(super) async fn reap_retired(&self, service: &super::TerminalService) {
        let Ok(_cleanup) = self.cleanup.try_lock() else {
            return;
        };
        let candidates = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retired
            .iter()
            .filter(|c| {
                c.requests.state.lock().unwrap_or_else(|e| e.into_inner()).0 == 0
                    && c.unsettled_remote_operation().is_none()
            })
            .cloned()
            .collect::<Vec<_>>();
        let deadline = tokio::time::Instant::now() + super::super::CLOSE_OBSERVATION_TIMEOUT;
        for (connection, result) in
            futures::future::join_all(candidates.into_iter().map(|connection| async move {
                let result = async {
                    Self::idle(&connection, deadline).await?;
                    tokio::time::timeout_at(
                        deadline,
                        service.close_session(connection.master_terminal_id()),
                    )
                    .await
                    .map_err(|_| "retired master close observation expired".to_string())??;
                    Ok::<(), String>(())
                }
                .await;
                (connection, result)
            }))
            .await
        {
            if result.is_ok() {
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retired
                    .retain(|c| !Arc::ptr_eq(c, &connection));
            } else {
                tracing::warn!(generation=%connection.generation(), "retired SSH connection remains owned after close observation");
            }
        }
    }

    pub(super) fn schedule_reap(self: &Arc<Self>) {
        if !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retired
            .iter()
            .any(|c| {
                c.requests.state.lock().unwrap_or_else(|e| e.into_inner()).0 == 0
                    && c.unsettled_remote_operation().is_none()
            })
        {
            return;
        }
        let _revision = self
            .reap_revision
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_add(1))
            });
        if self
            .reap_scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let runtime = self
            .runtime
            .clone()
            .or_else(|| tokio::runtime::Handle::try_current().ok());
        let Some(runtime) = runtime else {
            self.reap_scheduled.store(false, Ordering::Release);
            return;
        };
        let service = self.current().service.upgrade();
        let Some(service) = service else {
            self.reap_scheduled.store(false, Ordering::Release);
            return;
        };
        let owner = self.clone();
        runtime.spawn(async move {
            let revision = loop {
                let revision = owner.reap_revision.load(Ordering::Acquire);
                owner.reap_retired(&service).await;
                if owner.reap_revision.load(Ordering::Acquire) == revision {
                    break revision;
                }
            };
            owner.reap_scheduled.store(false, Ordering::Release);
            if owner.reap_revision.load(Ordering::Acquire) != revision {
                owner.schedule_reap();
            }
        });
    }
}
