//! 连接级 SFTP 车道池：在一条已认证的 OpenSSH 连接上复用固定数量的
//! 标准 SFTP 通道。
//!
//! 原实现为每次文件操作重新启动一个 `ssh -s sftp` 子进程并重做 SFTP
//! 初始化；这在高延迟下把固定成本叠加到每个小操作上。车道池只消除这部
//! 分成本，不改变 SFTP 协议、不引入远端服务。
//!
//! 两条硬不变式：
//!
//! 1. 车道独占：一条车道同一时刻只服务一个操作，因此
//!    `SftpChannel::finish` 依赖的“请求全部结算”证据仍然成立。
//! 2. 可疑即弃：只要一次操作可能破坏通道（传输错误、未知副作用负债，
//!    或操作方仍持有客户端克隆），该车道立即关闭并且不再入池；下一次操作
//!    重新开一条。复用只发生在“这条通道确定健康”的情况。

use super::channel::{SftpChannel, SftpClient};
use super::sftp::{remote_failure, unavailable};
use super::{ProjectSshConnection, RemoteOperationResources};
use echo_agent::error::{ReactError, Result};
use futures::future::BoxFuture;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// 打开一条新车道。生产实现启动标准 SFTP 子系统；测试可注入本地夹具，
/// 因此“复用 / 弃用 / 关闭结算”可以在没有 SSH 服务器的环境下被真实覆盖。
pub(in crate::terminal) type LaneOpener = Arc<
    dyn Fn(Arc<OnceLock<Arc<RemoteOperationResources>>>) -> BoxFuture<'static, Result<SftpChannel>>
        + Send
        + Sync,
>;

/// 车道上限。每条车道是一个常驻的标准 SFTP 子进程；2 条足够覆盖
/// “一个长读 + 一个普通操作”，同时限制故障面与退出结算范围。
pub(super) const LANE_CAPACITY: usize = 2;

pub(in crate::terminal) struct SftpLanes {
    opener: LaneOpener,
    permits: Arc<Semaphore>,
    idle: Mutex<Vec<SftpChannel>>,
    closing: AtomicBool,
    // 结构化观测：长期保留，用于回答“这条连接到底省掉了多少次进程启动”。
    operations: AtomicUsize,
    opened: AtomicUsize,
    reused: AtomicUsize,
}

impl SftpLanes {
    pub(super) fn new(connection: &Arc<ProjectSshConnection>) -> Self {
        // 只弱引用连接：车道池由连接持有，强引用会形成环。
        let connection = Arc::downgrade(connection);
        Self::with_opener(Arc::new(move |registration| {
            let connection = connection.clone();
            Box::pin(async move {
                let connection = connection.upgrade().ok_or_else(|| {
                    unavailable("project SSH connection was dropped before lane checkout")
                })?;
                SftpChannel::open(
                    connection,
                    registration,
                    super::SshChannelPurpose::OrdinarySftp,
                )
                .await
            })
        }))
    }

    fn with_opener(opener: LaneOpener) -> Self {
        Self {
            opener,
            permits: Arc::new(Semaphore::new(LANE_CAPACITY)),
            idle: Mutex::new(Vec::new()),
            closing: AtomicBool::new(false),
            operations: AtomicUsize::new(0),
            opened: AtomicUsize::new(0),
            reused: AtomicUsize::new(0),
        }
    }

    /// 在一条车道上执行一次标准 SFTP 操作。
    ///
    /// 物理操作在独立任务里完成：调用方的 future 被丢弃（取消、超时、
    /// join 失败）不会让已在途的请求失去观察者，也不会让车道停在半途。
    pub(super) async fn run<T, F, Fut>(self: &Arc<Self>, mutating: bool, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<SftpClient>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let permit = self.acquire().await?;
        let registration = Arc::new(OnceLock::<Arc<RemoteOperationResources>>::new());
        let observed = Arc::clone(&registration);
        let lanes = Arc::clone(self);
        let physical = tokio::spawn(async move {
            lanes
                .run_physical(permit, mutating, operation, registration)
                .await
        });
        match physical.await {
            Ok(result) => result,
            Err(error) => {
                if let Some(resources) = observed.get()
                    && !resources.is_settled()
                {
                    resources.mark_stop_unknown(
                        "original SFTP lane observation was lost after checkout",
                    );
                    // 与单通道路径一致：负债只能有界观察，超时保留 unknown。
                    if !super::wait_for_unknown_settlement(
                        resources,
                        super::super::CLOSE_OBSERVATION_TIMEOUT,
                    )
                    .await
                    {
                        tracing::warn!(
                            "project SSH SFTP lane settlement observation timed out; unknown debt retained"
                        );
                    }
                }
                Err(remote_failure(
                    format!("SFTP lane result observation failed: {error}"),
                    mutating,
                ))
            }
        }
    }

    pub(super) fn begin_shutdown(&self) {
        self.closing.store(true, Ordering::Release);
    }

    /// 关闭观察失败后由原连接 owner 显式恢复准入，不重放在途操作。
    pub(super) fn reopen_after_failed_close(&self) {
        self.closing.store(false, Ordering::Release);
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(super) async fn shutdown(&self) -> Result<()> {
        self.shutdown_before(tokio::time::Instant::now() + super::super::CLOSE_OBSERVATION_TIMEOUT)
            .await
    }

    /// 同一绝对期限覆盖排队和物理关闭观察。超时只结束观察，不丢弃
    /// 正在执行的文件请求或关闭它们的原物理任务。
    pub(super) async fn shutdown_before(&self, deadline: tokio::time::Instant) -> Result<()> {
        self.begin_shutdown();
        let drained = tokio::time::timeout_at(
            deadline,
            Arc::clone(&self.permits).acquire_many_owned(LANE_CAPACITY as u32),
        )
        .await
        .map_err(|_| {
            unavailable("SFTP lane drain observation timed out; original operations retained")
        })?
        .map_err(|_| unavailable("project SSH SFTP lanes are already closed"))?;
        let lanes = std::mem::take(&mut *self.idle.lock().await);
        let (operations, opened, reused) = self.stats();
        tracing::info!(
            operations,
            opened,
            reused,
            closed_lanes = lanes.len(),
            "project SSH SFTP lane close observation started"
        );
        let physical = tokio::spawn(async move {
            // 全部 permit 随物理关闭保留；再次关闭不能越过仍未结算的
            // 原任务而假报成功。丢弃 JoinHandle 只分离观察者，不取消任务。
            let _drained = drained;
            let mut failure = None;
            for channel in lanes {
                if let Err(error) = channel.finish(false).await {
                    tracing::warn!(%error, "original SFTP lane failed to close");
                    failure.get_or_insert(error);
                }
            }
            match failure {
                Some(error) => Err(error),
                None => Ok(()),
            }
        });
        tokio::time::timeout_at(deadline, physical)
            .await
            .map_err(|_| {
                unavailable("SFTP close observation timed out; original close task retained")
            })?
            .map_err(|error| unavailable(format!("SFTP close task observation failed: {error}")))?
    }

    async fn acquire(&self) -> Result<OwnedSemaphorePermit> {
        if self.closing.load(Ordering::Acquire) {
            return Err(unavailable("project SSH SFTP lanes are closing"));
        }
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| unavailable("project SSH SFTP lanes are closed"))?;
        if self.closing.load(Ordering::Acquire) {
            drop(permit);
            return Err(unavailable("project SSH SFTP lanes are closing"));
        }
        Ok(permit)
    }

    async fn run_physical<T, F, Fut>(
        &self,
        permit: OwnedSemaphorePermit,
        mutating: bool,
        operation: F,
        registration: Arc<OnceLock<Arc<RemoteOperationResources>>>,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<SftpClient>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        // permit 在这个作用域结束时归还；车道在此之前完成入池或结算。
        let _permit = permit;
        let started = std::time::Instant::now();
        let (channel, lane_reused) = self.checkout(registration).await?;
        let client = channel.client();
        let result = operation(client).await;
        tracing::debug!(
            lane_reused,
            elapsed_ms = started.elapsed().as_millis(),
            ok = result.is_ok(),
            "project SSH SFTP lane operation settled"
        );
        if lane_is_reusable(channel.can_reuse(), channel.has_unknown_debt(), &result) {
            self.idle.lock().await.push(channel);
            return result;
        }
        // 车道带负债时必须按“可能已产生副作用”结算，不能借只读标志清债。
        let finish_mutating = mutating || channel.has_unknown_debt();
        let settled = channel.finish(finish_mutating).await;
        match (result, settled) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), settlement) => {
                if let Err(close_error) = settlement {
                    tracing::warn!(%close_error, "discarded SFTP lane also failed to settle");
                }
                Err(error)
            }
            (Ok(_), Err(error)) => Err(error),
        }
    }

    /// 取一条车道，第二个返回值表示是否命中空闲复用（供结构化日志使用）。
    async fn checkout(
        &self,
        registration: Arc<OnceLock<Arc<RemoteOperationResources>>>,
    ) -> Result<(SftpChannel, bool)> {
        self.operations.fetch_add(1, Ordering::AcqRel);
        if let Some(channel) = self.idle.lock().await.pop() {
            self.reused.fetch_add(1, Ordering::AcqRel);
            return Ok((channel, true));
        }
        let channel = (self.opener)(registration).await?;
        self.opened.fetch_add(1, Ordering::AcqRel);
        Ok((channel, false))
    }

    /// 累计 (操作数, 新开通道数, 复用次数)。长期观测用，不作为正确性依据。
    pub(super) fn stats(&self) -> (usize, usize, usize) {
        (
            self.operations.load(Ordering::Acquire),
            self.opened.load(Ordering::Acquire),
            self.reused.load(Ordering::Acquire),
        )
    }
}

/// 车道复用判定：独占、无负债、且错误没有破坏通道，三者同时成立才复用。
/// 单独抽出来是为了能在没有真实 SSH 服务器的情况下固定这条策略。
fn lane_is_reusable<T>(exclusive: bool, has_unknown_debt: bool, result: &Result<T>) -> bool {
    exclusive && !has_unknown_debt && !error_may_have_broken_lane(result)
}

/// 这个错误是否可能已经破坏了通道。
///
/// 服务端明确回复的“不存在/无权限”只是请求级结果，通道仍然健康，不值得
/// 为它丢弃一条车道（搜索时缺失文件很常见）。其余错误一律按可疑处理。
fn error_may_have_broken_lane<T>(result: &Result<T>) -> bool {
    match result {
        Ok(_) => false,
        Err(ReactError::Io(error)) => !matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
        ),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::error::ToolError;

    fn unreadable() -> Result<()> {
        Err(ToolError::RemoteFailure {
            tool: "project_ssh".to_string(),
            message: "channel lost".to_string(),
            http_status: None,
            failure: Box::new(echo_agent::tools::ToolFailure::new(
                echo_agent::tools::ToolFailureCategory::Unavailable,
            )),
        }
        .into())
    }

    #[test]
    fn healthy_lane_is_reused_for_ordinary_results() {
        assert!(lane_is_reusable(true, false, &Ok::<(), ReactError>(())));
        // 搜索时缺失文件很常见，服务端明确回复不能导致车道反复重开。
        let missing: Result<()> =
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into());
        assert!(lane_is_reusable(true, false, &missing));
        let denied: Result<()> =
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "no").into());
        assert!(lane_is_reusable(true, false, &denied));
    }

    #[test]
    fn suspect_lane_is_never_reused() {
        assert!(!lane_is_reusable(true, true, &Ok::<(), ReactError>(())));
        assert!(!lane_is_reusable(false, false, &Ok::<(), ReactError>(())));
        assert!(!lane_is_reusable(true, false, &unreadable()));
        let io_other: Result<()> = Err(std::io::Error::other("broken pipe").into());
        assert!(!lane_is_reusable(true, false, &io_other));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod fixture_tests {
    //! 真实标准 SFTP 协议经本地 stdio 夹具；不认证、不访问真实 SSH 服务器。

    use super::*;
    use crate::terminal::{TerminalService, TerminalTarget};
    use std::sync::atomic::AtomicUsize;

    struct Fixture {
        lanes: Arc<SftpLanes>,
        opened: Arc<AtomicUsize>,
        connection: Arc<ProjectSshConnection>,
        _service: Arc<TerminalService>,
        directory: std::path::PathBuf,
        _temporary: tempfile::TempDir,
    }

    async fn fixture(owner: &str) -> Result<Fixture> {
        fixture_with_close_delay(owner, false).await
    }

    async fn fixture_with_close_delay(owner: &str, delayed_close: bool) -> Result<Fixture> {
        let service = TerminalService::new();
        let target = TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let connection =
            ProjectSshConnection::prepare(&service, owner, target.clone()).map_err(unavailable)?;
        connection
            .effective_target
            .set(target.clone())
            .map_err(|_| unavailable("duplicate fixture target"))?;
        let temporary = tempfile::tempdir()?;
        let directory = temporary.path().to_path_buf();

        let opened = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opened);
        let weak = Arc::downgrade(&connection);
        let opener: LaneOpener = Arc::new(move |registration| {
            let weak = weak.clone();
            let counter = Arc::clone(&counter);
            let target = target.clone();
            Box::pin(async move {
                let connection = weak
                    .upgrade()
                    .ok_or_else(|| unavailable("fixture connection was dropped"))?;
                counter.fetch_add(1, Ordering::SeqCst);
                let operation = connection.operation().map_err(unavailable)?;
                let mut command = if delayed_close {
                    // 标准协议不变；服务端EOF后本地原child还需一秒才退出，
                    // 用于观察关闭超时时是否仍由原物理任务持有。
                    let mut command = tokio::process::Command::new("/bin/sh");
                    command.args(["-c", "/usr/libexec/sftp-server; /bin/sleep 1"]);
                    command
                } else {
                    tokio::process::Command::new("/usr/libexec/sftp-server")
                };
                let child = command
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()?;
                SftpChannel::from_spawned(
                    connection,
                    registration,
                    child,
                    operation,
                    target,
                    super::super::SshChannelPurpose::OrdinarySftp,
                )
                .await
            })
        });
        Ok(Fixture {
            lanes: Arc::new(SftpLanes::with_opener(opener)),
            opened,
            connection,
            _service: service,
            directory,
            _temporary: temporary,
        })
    }

    fn metadata_of(
        lanes: &Arc<SftpLanes>,
        directory: std::path::PathBuf,
    ) -> impl Future<Output = Result<()>> + use<> {
        let lanes = Arc::clone(lanes);
        async move {
            lanes
                .run(false, move |client| async move {
                    client
                        .fs()
                        .metadata(directory)
                        .await
                        .map(|_| ())
                        .map_err(|error| client.error(error, false))
                })
                .await
        }
    }

    #[tokio::test]
    async fn idle_lane_is_reused_across_operations() -> Result<()> {
        let fixture = fixture("lane-reuse").await?;
        for _ in 0..3 {
            metadata_of(&fixture.lanes, fixture.directory.clone()).await?;
        }
        assert_eq!(
            fixture.opened.load(Ordering::SeqCst),
            1,
            "三次独立操作必须复用同一条车道"
        );
        assert_eq!(
            fixture.lanes.stats(),
            (3, 1, 2),
            "一次新开、两次复用，供长期结构化日志核对"
        );
        assert_eq!(
            fixture
                .connection
                .spawn_counts
                .ordinary_sftp
                .load(Ordering::Acquire),
            1
        );
        assert_eq!(
            fixture
                .connection
                .spawn_counts
                .preparation_sftp
                .load(Ordering::Acquire),
            0
        );
        assert_eq!(
            fixture
                .connection
                .spawn_counts
                .streaming_sftp
                .load(Ordering::Acquire),
            0
        );
        assert_eq!(
            fixture
                .connection
                .spawn_counts
                .execution
                .load(Ordering::Acquire),
            0
        );
        fixture.lanes.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn warm_pool_cannot_bypass_original_service_shutdown() -> Result<()> {
        use echo_agent::tools::filesystem::FileSystem;

        let fixture = fixture("lane-shutdown-admission").await?;
        metadata_of(&fixture.lanes, fixture.directory.clone()).await?;
        fixture
            .connection
            .sftp_lanes
            .set(fixture.lanes.clone())
            .map_err(|_| unavailable("duplicate fixture lane pool"))?;
        let files = fixture
            .connection
            .file_system(fixture.directory.to_string_lossy().into_owned())
            .map_err(unavailable)?;
        // 仅切换原service的准入；暖池自身未关闭，才能证明每次复用也
        // 经过连接门禁。复用失败不得新开网络连接或再次执行文件操作。
        fixture
            ._service
            .shutting_down
            .store(true, Ordering::Release);
        let directory = fixture.directory.clone();
        let attempted = tokio::task::spawn_blocking(move || files.metadata(&directory))
            .await
            .map_err(|error| unavailable(error.to_string()))?;
        fixture
            ._service
            .shutting_down
            .store(false, Ordering::Release);
        fixture.lanes.shutdown().await?;
        assert!(attempted.is_err());
        assert_eq!(fixture.lanes.stats(), (1, 1, 0));
        assert_eq!(
            fixture.connection.active_operations.load(Ordering::Acquire),
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn close_timeout_keeps_physical_close_and_blocks_false_retry_success() -> Result<()> {
        let fixture = fixture_with_close_delay("lane-close-physical", true).await?;
        metadata_of(&fixture.lanes, fixture.directory.clone()).await?;
        let first = fixture
            .lanes
            .shutdown_before(tokio::time::Instant::now() + std::time::Duration::from_millis(50))
            .await;
        let retained = fixture.connection.active_operations.load(Ordering::Acquire) > 0;
        let second = fixture
            .lanes
            .shutdown_before(tokio::time::Instant::now() + std::time::Duration::from_millis(50))
            .await;
        // 原物理任务自然退出后，重试只观察它，不重发任何文件请求。
        fixture.lanes.shutdown().await?;
        assert!(first.is_err());
        assert!(retained);
        assert!(second.is_err());
        assert_eq!(fixture.lanes.stats(), (1, 1, 0));
        assert_eq!(
            fixture.connection.active_operations.load(Ordering::Acquire),
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn suspicious_lane_is_discarded_instead_of_reused() -> Result<()> {
        let fixture = fixture("lane-discard").await?;
        let failed = fixture
            .lanes
            .run(false, move |_client| async move {
                Err::<(), ReactError>(remote_failure("fixture transport failure", false))
            })
            .await;
        assert!(failed.is_err(), "夹具错误必须传播，不能静默降级");
        assert_eq!(fixture.opened.load(Ordering::SeqCst), 1);

        metadata_of(&fixture.lanes, fixture.directory.clone()).await?;
        assert_eq!(
            fixture.opened.load(Ordering::SeqCst),
            2,
            "可疑车道必须弃用，下一次操作重新开一条"
        );
        assert_eq!(fixture.lanes.stats(), (2, 2, 0), "可疑车道不得计入复用");
        fixture.lanes.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_settles_idle_lanes_and_refuses_new_operations() -> Result<()> {
        let fixture = fixture("lane-shutdown").await?;
        metadata_of(&fixture.lanes, fixture.directory.clone()).await?;
        assert_eq!(
            fixture.connection.active_operations.load(Ordering::Acquire),
            1
        );

        fixture.lanes.shutdown().await?;
        assert_eq!(
            fixture.connection.active_operations.load(Ordering::Acquire),
            0,
            "关闭空闲车道必须释放它持有的原操作租约"
        );
        assert!(
            metadata_of(&fixture.lanes, fixture.directory.clone())
                .await
                .is_err(),
            "关闭后的车道池不得再接受新操作"
        );
        Ok(())
    }
    #[tokio::test]
    async fn shutdown_observation_is_bounded_and_keeps_inflight_operation() -> Result<()> {
        let fixture = fixture("lane-close-inflight").await?;
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, blocked) = tokio::sync::oneshot::channel();
        let lanes = fixture.lanes.clone();
        let directory = fixture.directory.clone();
        let active = tokio::spawn(async move {
            lanes
                .run(false, move |client| async move {
                    let _ = entered.send(());
                    blocked
                        .await
                        .map_err(|error| unavailable(error.to_string()))?;
                    client
                        .fs()
                        .metadata(directory)
                        .await
                        .map(|_| ())
                        .map_err(|error| client.error(error, false))
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), started)
            .await
            .map_err(|_| unavailable("original lane did not start"))?
            .map_err(|error| unavailable(error.to_string()))?;
        let observation = tokio::time::timeout(
            crate::terminal::CLOSE_OBSERVATION_TIMEOUT + std::time::Duration::from_secs(2),
            fixture.lanes.shutdown(),
        )
        .await;
        let physically_owned = fixture.connection.active_operations.load(Ordering::Acquire) > 0;
        // 先释放原操作并完成真实结算，再报告断言失败，不能把夹具留作孤儿进程。
        let _ = release.send(());
        active
            .await
            .map_err(|error| unavailable(error.to_string()))??;
        fixture.lanes.shutdown().await?;
        assert!(
            observation.is_ok(),
            "pool close must return its own bounded observation, not hang until the caller times out"
        );
        assert!(
            observation
                .map_err(|_| unavailable("outer observation timed out"))?
                .is_err(),
            "an in-flight operation is not a confirmed close"
        );
        assert!(
            physically_owned,
            "failed close observation must retain the original physical operation"
        );
        assert_eq!(
            fixture.connection.active_operations.load(Ordering::Acquire),
            0
        );
        Ok(())
    }
}
