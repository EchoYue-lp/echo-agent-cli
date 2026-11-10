//! The existing SandboxExecutor contract over the owned OpenSSH connection.

use super::generations::{ConnectionSource, RequestSnapshot};
use super::sftp::{remote_failure, unavailable};
use super::{ProjectSshConnection, RemoteOperationResources, remote_path, shell_quote};
use echo_agent::error::{Result, SandboxError};
use echo_agent::sandbox::{
    CommandKind, ExecutionResult, IsolationLevel, ResourceLimits, SandboxCommand, SandboxExecutor,
};
use futures::future::BoxFuture;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const DEFAULT_OUTPUT_BYTES: u64 = 1024 * 1024;
const GROUP_RECEIPT_TAIL_BYTES: usize = 256;

#[derive(Clone, Copy, Debug)]
struct RemoteGroupReceipt {
    exit_code: i32,
    group_id: Option<u32>,
}

struct RemoteStderrCapture {
    text: String,
    bytes: u64,
    truncated: bool,
    receipt: Option<RemoteGroupReceipt>,
}

pub struct ProjectSshSandbox {
    connection: ConnectionSource,
}

impl ProjectSshSandbox {
    pub(super) fn new(connection: Arc<ProjectSshConnection>) -> Self {
        Self::with_source(ConnectionSource::Physical(connection))
    }

    pub(super) fn with_source(connection: ConnectionSource) -> Self {
        Self { connection }
    }

    async fn execute_selected(
        snapshot: std::result::Result<RequestSnapshot, String>,
        command: SandboxCommand,
        limits: ResourceLimits,
        cancel: Option<Arc<CancellationToken>>,
    ) -> Result<ExecutionResult> {
        let snapshot = snapshot.map_err(unavailable)?;
        let result =
            Self::execute_owned(snapshot.connection.clone(), command, limits, cancel).await;
        drop(snapshot);
        result
    }

    async fn execute_owned(
        connection: Arc<ProjectSshConnection>,
        command: SandboxCommand,
        limits: ResourceLimits,
        cancel: Option<Arc<CancellationToken>>,
    ) -> Result<ExecutionResult> {
        Self::execute_with_launcher(connection, command, limits, cancel, |connection, script| {
            // 与现有execute_with_launcher同一物理启动接缝；夹具只替换进程，
            // 原快照、CAS、wait/管道/guard与错误路径仍走生产实现，不连接网络。
            #[cfg(test)]
            if let Some(launcher) = connection
                .test_execution_launcher
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
            {
                return launcher(script);
            }
            let mut ssh = connection.slave_command().map_err(unavailable)?;
            ssh.arg("-T");
            connection.add_destination(&mut ssh).map_err(unavailable)?;
            ssh.arg(script);
            Ok(ssh)
        })
        .await
    }

    // Tests inject a self-owned local command at the existing process launch
    // seam. All resource registration, physical wait and observer code below
    // is the production path; this is not another executor or runtime.
    async fn execute_with_launcher<F>(
        connection: Arc<ProjectSshConnection>,
        command: SandboxCommand,
        limits: ResourceLimits,
        cancel: Option<Arc<CancellationToken>>,
        launcher: F,
    ) -> Result<ExecutionResult>
    where
        F: FnOnce(&ProjectSshConnection, String) -> Result<tokio::process::Command>
            + Send
            + 'static,
    {
        validate_capabilities(&command, &limits)?;
        let command_cancel = command.cancel.clone();
        if cancel.as_ref().is_some_and(|token| token.is_cancelled())
            || command_cancel
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
        {
            return Err(SandboxError::Cancelled(
                "owning run cancelled before SSH execution started".into(),
            )
            .into());
        }
        // 期限监督在目标机器执行；本机预算到达/取消请求不等同于远端停止证明。
        let effective_deadline_secs = timeout_secs(&command, &limits);
        let timeout = limits
            .cpu_time_secs
            .map(Duration::from_secs)
            .map(|limit| limit.min(command.timeout))
            .unwrap_or(command.timeout);
        if Instant::now().checked_add(timeout).is_none() {
            return Err(unavailable(
                "remote timeout exceeds the monotonic clock range",
            ));
        }
        let closure_nonce = command
            .require_physical_resource_ownership
            .then(|| uuid::Uuid::new_v4().simple().to_string());
        let payload = command_script(&command)?;
        let script = match closure_nonce.as_deref() {
            Some(nonce) => owned_group_deadline_script(&payload, timeout, nonce)?,
            None => deadline_script(&payload, effective_deadline_secs)?,
        };
        let target = connection.effective_target().map_err(unavailable)?.clone();
        let lease = connection.operation().map_err(unavailable)?;
        let output_limit = limits.max_output_bytes.unwrap_or(DEFAULT_OUTPUT_BYTES);
        let start = Instant::now();
        let physical_cancel = cancel.clone();
        let physical_command_cancel = command_cancel.clone();
        let original_resources = Arc::new(OnceLock::<Arc<RemoteOperationResources>>::new());
        let registration = Arc::clone(&original_resources);
        // The existing physical invocation is never aborted by its observer.
        // The connection retains its resources before the first post-spawn
        // await, including if this task unwinds or its JoinHandle is lost.
        let mut physical = tokio::spawn(async move {
            if physical_cancel
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
                || physical_command_cancel
                    .as_ref()
                    .is_some_and(|token| token.is_cancelled())
            {
                return Err(SandboxError::Cancelled(
                    "owning run cancelled before SSH process spawn".into(),
                )
                .into());
            }
            let mut ssh = launcher(&connection, script)?;
            let mut child = ssh
                .stdin(if command.stdin.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| {
                    unavailable(format!("cannot start system SSH execution: {error}"))
                })?;
            connection.record_channel_spawn(super::SshChannelPurpose::Execution);
            let resources = registration
                .get_or_init(|| {
                    connection.retain_remote_operation(
                        &child,
                        target,
                        lease,
                        command.resource_guards,
                    )
                })
                .clone();
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let stdout = tokio::spawn(async move {
                match stdout {
                    Some(stdout) => read_bounded(stdout, output_limit).await,
                    None => Err(std::io::Error::other("SSH stdout pipe was not prepared")),
                }
            });
            let require_group_receipt = closure_nonce.is_some();
            let stderr = tokio::spawn(async move {
                match stderr {
                    Some(stderr) => read_stderr_bounded(stderr, output_limit, closure_nonce).await,
                    None => Err(std::io::Error::other("SSH stderr pipe was not prepared")),
                }
            });
            let input = child.stdin.take();
            let input_bytes = command.stdin;
            let input = tokio::spawn(async move {
                match (input, input_bytes) {
                    (Some(mut stdin), Some(bytes)) => {
                        stdin.write_all(bytes.as_bytes()).await?;
                        stdin.shutdown().await?;
                    }
                    (None, Some(_)) => {
                        return Err(std::io::Error::other("SSH input pipe was not prepared"));
                    }
                    _ => {}
                }
                Ok::<(), std::io::Error>(())
            });
            // Re-observe only this actual child on wait failure. An outer
            // Task/Cell/ProductDataIO observer may stop waiting independently.
            let mut wait_error = None;
            let status = loop {
                match child.wait().await {
                    Ok(status) => break status,
                    Err(error) => {
                        if wait_error.is_none() {
                            wait_error = Some(error.to_string());
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            };
            let stdout = stdout.await;
            let stderr = stderr.await;
            let input = input.await;
            #[cfg(test)]
            resources
                .test_local_exit_observed
                .store(true, std::sync::atomic::Ordering::Release);
            let group_receipt = stderr
                .as_ref()
                .ok()
                .and_then(|result| result.as_ref().ok())
                .and_then(|capture| capture.receipt);
            let exit_code = if require_group_receipt {
                let Some(receipt) = group_receipt else {
                    // Root exit/EOF/non255 cannot replace a receipt from this original owned-group controller.
                    resources.mark_stop_unknown(
                        "owned remote process-group closure receipt was missing or truncated",
                    );
                    resources.wait_for_settlement().await;
                    return Err(remote_failure(
                        "original remote owned-group closure was unknown; verify its effects before retry",
                        true,
                    ));
                };
                resources.complete_with_proof();
                if status.code() != Some(receipt.exit_code) {
                    return Err(remote_failure(
                        "remote group closure was proven but the original SSH exit observation disagreed",
                        true,
                    ));
                }
                if receipt.group_id.is_none() && receipt.exit_code == 125 {
                    return Err(unavailable(
                        "remote Python3/POSIX process-group ownership primitives are unavailable; payload was not started",
                    ));
                }
                receipt.exit_code
            } else {
                match status.code() {
                    Some(code) if code != 255 => code,
                    _ => {
                        resources.mark_stop_unknown(
                            "remote exit receipt was lost (SSH255 or no exit status)",
                        );
                        resources.wait_for_settlement().await;
                        return Err(remote_failure(
                            "original remote execution stop was unknown; verify its effects before retry",
                            true,
                        ));
                    }
                }
            };
            // Legacy no-requirement path keeps its original exit receipt contract; Some requires the group proof above.
            if !require_group_receipt {
                resources.complete_with_proof();
            }
            let (stdout, stdout_bytes, stdout_truncated) = stdout
                .map_err(|error| {
                    remote_failure(
                        format!("SSH stdout observation failed after remote exit: {error}"),
                        true,
                    )
                })?
                .map_err(|error| {
                    remote_failure(
                        format!("SSH stdout read failed after remote exit: {error}"),
                        true,
                    )
                })?;
            let RemoteStderrCapture {
                text: stderr,
                bytes: stderr_bytes,
                truncated: stderr_truncated,
                ..
            } = stderr
                .map_err(|error| {
                    remote_failure(
                        format!("SSH stderr observation failed after remote exit: {error}"),
                        true,
                    )
                })?
                .map_err(|error| {
                    remote_failure(
                        format!("SSH stderr read failed after remote exit: {error}"),
                        true,
                    )
                })?;
            input.map_err(|error| remote_failure(format!("SSH input observation failed after remote exit: {error}"), true))?
                .map_err(|error| remote_failure(format!("SSH input may have been partially delivered before remote exit: {error}"), true))?;
            if let Some(error) = wait_error {
                return Err(remote_failure(
                    format!(
                        "original SSH child completed after a failed local wait: {error}; verify effects before retry"
                    ),
                    true,
                ));
            }
            if matches!(exit_code, 125..=127) {
                return Err(remote_failure(
                    format!(
                        "remote command completed with exit {exit_code}; bootstrap/exec failure and program return may be indistinguishable, verify effects before retry"
                    ),
                    true,
                ));
            }
            let mut result = ExecutionResult {
                exit_code,
                stdout,
                stderr,
                duration: start.elapsed(),
                sandbox_type: "ssh".to_string(),
                timed_out: false,
                cancelled: false,
                output_truncated: stdout_truncated || stderr_truncated,
                stdout_bytes,
                stderr_bytes,
            };
            result.enforce_output_limit(output_limit);
            Ok(result)
        });
        let cancellation = async {
            let explicit = async {
                match cancel {
                    Some(cancel) => cancel.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            let invocation = async {
                match command_cancel {
                    Some(cancel) => cancel.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! { _ = explicit => {}, _ = invocation => {} }
        };
        tokio::pin!(cancellation);
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let stop_observation = tokio::time::sleep(super::super::CLOSE_OBSERVATION_TIMEOUT);
        tokio::pin!(stop_observation);
        let mut cancellation_requested = false;
        let mut timeout_observed = false;
        let mut stop_observation_started = false;
        loop {
            tokio::select! {
                result = &mut physical => {
                    let result: Result<ExecutionResult> = match result {
                        Ok(result) => result,
                        Err(error) => {
                            if let Some(resources) = original_resources.get()
                                && !resources.is_settled()
                            {
                                resources.mark_stop_unknown("original physical task observation was lost after SSH spawn");
                                let until = if stop_observation_started {
                                    stop_observation.deadline()
                                } else {
                                    tokio::time::Instant::now() + super::super::CLOSE_OBSERVATION_TIMEOUT
                                };
                                if !super::wait_for_unknown_settlement(
                                    resources,
                                    until.saturating_duration_since(tokio::time::Instant::now()),
                                ).await {
                                    tracing::warn!("SSH physical observation was lost; original unknown resources retained");
                                }
                            }
                            return Err(remote_failure(format!("SSH physical result observation failed: {error}"), true));
                        }
                    };
                    let result = result?;
                    // 本机预算与退出原因是两件事：payload也能自行返回124。
                    // timed_out只记录观察预算，不能从它推出看门狗或目标停止。
                    let budget_elapsed = timeout_observed || start.elapsed() >= timeout;
                    if budget_elapsed && !cancellation_requested && result.exit_code == 124 {
                        let mut timed_out = result;
                        timed_out.timed_out = true;
                        return Ok(timed_out);
                    }
                    // 取消仍只发起观察。没有独立原因凭据时保留未知，不能把
                    // 124说成已杀掉，也不能把其它退出码说成一定早于期限。
                    if cancellation_requested {
                        let budget = if budget_elapsed {
                            "本机观察期限已到；"
                        } else {
                            ""
                        };
                        return Err(remote_failure(format!(
                            "取消已请求；{budget}原命令已返回（exit {}），退出原因未确认。副作用需先核对再重试",
                            result.exit_code,
                        ), true));
                    }
                    if budget_elapsed {
                        return Err(remote_failure(format!("Timeout was requested; original remote command subsequently completed with exit {}. Verify effects before retry", result.exit_code), true));
                    }
                    return Ok(result);
                },
                _ = &mut cancellation, if !cancellation_requested => {
                    cancellation_requested = true;
                    if !stop_observation_started {
                        stop_observation.as_mut().reset(
                            tokio::time::Instant::now() + super::super::CLOSE_OBSERVATION_TIMEOUT,
                        );
                        stop_observation_started = true;
                    }
                    tracing::warn!(
                        deadline_secs = effective_deadline_secs,
                        "remote command cancellation requested; original stop observation is bounded and target stop is unconfirmed"
                    );
                }
                _ = &mut deadline, if !timeout_observed => {
                    timeout_observed = true;
                    if !stop_observation_started {
                        stop_observation.as_mut().reset(
                            tokio::time::Instant::now() + super::super::CLOSE_OBSERVATION_TIMEOUT,
                        );
                        stop_observation_started = true;
                    }
                },
                _ = &mut stop_observation, if stop_observation_started => {
                    let stop_known = original_resources.get().is_some_and(|resources| resources.is_settled());
                    if let Some(resources) = original_resources.get()
                        && !stop_known
                    {
                        resources.mark_stop_unknown("bounded SSH stop observation expired; original physical operation retained");
                    }
                    let fact = if stop_known {
                        "原命令退出已确认，但结果观察尚未结算"
                    } else {
                        "远端停止尚未确认"
                    };
                    // 丢弃JoinHandle只结束本观察者；原physical任务、child与guards
                    // 继续由原连接持有，不能abort、清unknown或自动重放控制动作。
                    return Err(remote_failure(format!(
                        "停止观察期限已到；{fact}。原连接仍持有原物理任务及未结算资源，副作用需先核对再重试",
                    ), true));
                },
            }
        }
    }
}

impl SandboxExecutor for ProjectSshSandbox {
    fn name(&self) -> &str {
        "ssh"
    }

    fn isolation_level(&self) -> IsolationLevel {
        IsolationLevel::None
    }

    fn supports_physical_resource_ownership(&self) -> bool {
        // 原 remote operation 保有 guard；255/丢回执保留 Unknown，真实 stop proof 才结算。
        true
    }

    fn is_available(&self) -> BoxFuture<'_, bool> {
        let snapshot = self.connection.capture();
        Box::pin(async move {
            match snapshot {
                Ok(snapshot) => snapshot.connection.check_authenticated().await.is_ok(),
                Err(_) => false,
            }
        })
    }

    fn execute(&self, command: SandboxCommand) -> BoxFuture<'_, Result<ExecutionResult>> {
        Box::pin(Self::execute_selected(
            self.connection.capture(),
            command,
            ResourceLimits::unrestricted(),
            None,
        ))
    }

    fn execute_stream<'a>(
        &'a self,
        command: SandboxCommand,
    ) -> BoxFuture<
        'a,
        Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<Item = echo_agent::sandbox::SandboxStreamEvent> + Send + 'a,
                >,
            >,
        >,
    > {
        // 调用即捕获；默认trait到首次poll才execute，会误选重连后的代际。
        let execution = self.execute(command);
        Box::pin(async move {
            let result = execution.await?;
            let events: std::pin::Pin<
                Box<
                    dyn futures::Stream<Item = echo_agent::sandbox::SandboxStreamEvent> + Send + 'a,
                >,
            > = Box::pin(futures::stream::once(async move {
                echo_agent::sandbox::SandboxStreamEvent::Complete(result)
            }));
            Ok(events)
        })
    }

    fn execute_with_limits(
        &self,
        command: SandboxCommand,
        limits: ResourceLimits,
    ) -> BoxFuture<'_, Result<ExecutionResult>> {
        Box::pin(Self::execute_selected(
            self.connection.capture(),
            command,
            limits,
            None,
        ))
    }

    fn execute_with_limits_and_cancel(
        &self,
        command: SandboxCommand,
        limits: ResourceLimits,
        cancel: Option<Arc<CancellationToken>>,
    ) -> BoxFuture<'_, Result<ExecutionResult>> {
        Box::pin(Self::execute_selected(
            self.connection.capture(),
            command,
            limits,
            cancel,
        ))
    }

    fn cleanup(&self) -> BoxFuture<'_, Result<()>> {
        let connections = self.connection.connections();
        Box::pin(async move {
            for connection in connections {
                connection
                    .retry_retained_children()
                    .await
                    .map_err(unavailable)?;
                if connection
                    .active_operations
                    .load(std::sync::atomic::Ordering::Acquire)
                    != 0
                {
                    return Err(unavailable(connection.unsettled_remote_operation().unwrap_or_else(|| {
                    "original SSH operations remain active; cleanup has no remote completion proof".to_string()
                })));
                }
            }
            Ok(())
        })
    }
}

fn validate_capabilities(command: &SandboxCommand, limits: &ResourceLimits) -> Result<()> {
    if command
        .minimum_isolation
        .is_some_and(|minimum| minimum > IsolationLevel::None)
    {
        return Err(SandboxError::Unavailable(
            "SSH transport does not prove OS/process/container isolation on the remote host".into(),
        )
        .into());
    }
    if limits.memory_bytes.is_some()
        || limits.max_processes.is_some()
        || !limits.network
        || !limits.read_only_paths.is_empty()
        || !limits.writable_paths.is_empty()
    {
        return Err(SandboxError::Unavailable("requested remote memory/process/network/mount restrictions cannot be enforced by plain SSH; no local sandbox receipt is substituted".into()).into());
    }
    Ok(())
}

/// 远端看门狗执行的有效期限（秒）。与本地观察用的 `timeout` 同源，
/// 本机保留精确Duration，目标按秒上取整，不承诺两者同一时刻触发。
fn timeout_secs(command: &SandboxCommand, limits: &ResourceLimits) -> u64 {
    let timeout = limits
        .cpu_time_secs
        .map(Duration::from_secs)
        .map(|limit| limit.min(command.timeout))
        .unwrap_or(command.timeout);
    // 目标timer粒度为秒，向上取整避免早于原Duration发送终止信号；
    // 本机观察预算仍使用原精确Duration，不以扩大预算掩盖等待。
    let seconds = if timeout.subsec_nanos() == 0 {
        timeout.as_secs()
    } else {
        timeout.as_secs().saturating_add(1)
    };
    seconds.max(1)
}

/// 用 POSIX `sh` 在目标机器监督期限，届时发送 TERM，再按宽限尝试 KILL。
///
/// 负载本身仍是原来的 `cd ... || exit 125; exec ...` 脚本，因此 shell 语义、
/// 环境变量和退出码都不变；只有"到点后由谁负责结束"从本机观察变成远端执行。
/// 看门狗分支与负载本身都可能返回124；退出码本身不证明终止原因，
/// 更不证明被操作的服务已经停止。客户端仍须保留未知及原物理观察。
fn deadline_script(script: &str, timeout_seconds: u64) -> Result<String> {
    let payload = shell_quote(script).map_err(unavailable)?;
    // POSIX异步列表会先把fd0设为空；父shell须先保存原输入，
    // 再从独立fd传给负载。<&0在dash里只会复制已置空的输入。
    // timer停止时必须kill+wait其原sleep子进程，再由parent wait timer。
    // 仅kill外层subshell会留下sleep持有输出管道，给每条快命令多加约1秒。
    // PID发布期间先记录停止请求，发布完成再回收，不能在spawn半途退出。
    Ok(format!(
        r#"eko_marker="${{TMPDIR:-/tmp}}/.eko-deadline-$$"
rm -f "$eko_marker"
exec 3<&0
sh -c {payload} <&3 3<&- &
eko_child=$!
exec 3<&-
(
  eko_elapsed=0
  eko_timer=
  eko_spawning=0
  eko_stopping=0
  eko_reap_timer() {{
    if [ -n "$eko_timer" ]; then
      kill "$eko_timer" 2>/dev/null
      wait "$eko_timer" 2>/dev/null
      eko_timer=
    fi
  }}
  eko_stop_timer() {{
    eko_stopping=1
    if [ "$eko_spawning" -eq 0 ]; then eko_reap_timer; exit 0; fi
  }}
  trap eko_stop_timer HUP INT TERM
  eko_tick() {{
    eko_spawning=1
    sleep "$1" &
    eko_timer=$!
    eko_spawning=0
    if [ "$eko_stopping" -ne 0 ]; then eko_reap_timer; exit 0; fi
    wait "$eko_timer"
    eko_tick_status=$?
    eko_timer=
    [ "$eko_stopping" -eq 0 ] && [ "$eko_tick_status" -eq 0 ]
  }}
  while [ "$eko_elapsed" -lt {timeout_seconds} ]; do
    eko_tick 1 || exit 0
    eko_elapsed=$((eko_elapsed + 1))
    kill -0 "$eko_child" 2>/dev/null || exit 0
  done
  : > "$eko_marker"
  kill -TERM "$eko_child" 2>/dev/null
  eko_tick 5 || exit 0
  kill -KILL "$eko_child" 2>/dev/null
) </dev/null &
eko_watchdog=$!
wait "$eko_child"
eko_status=$?
kill "$eko_watchdog" 2>/dev/null
wait "$eko_watchdog" 2>/dev/null
if [ -f "$eko_marker" ]; then rm -f "$eko_marker"; exit 124; fi
rm -f "$eko_marker"
exit $eko_status"#
    ))
}

/// 有效 Some 使用同一个远端 controller 角色；原子建组、errno 与 monotonic 不依赖 shell 文本猜测。
/// 它 exec 替换原 POSIX controller/watchdog，不叠第二 supervisor；无要求仍用旧路径。
fn owned_group_deadline_script(script: &str, timeout: Duration, nonce: &str) -> Result<String> {
    if nonce.chars().count() != 32
        || !nonce.chars().all(|value| value.is_ascii_hexdigit())
        || Instant::now().checked_add(timeout).is_none()
    {
        return Err(unavailable(
            "invalid owned remote execution control metadata",
        ));
    }
    let controller = r#"import sys
# Controller-only isolation: payload environment remains inherited and unchanged.
sys.path = [path for path in sys.path if path]
nonce = sys.argv[3]
receipt_attempted = [False]
def receipt(kind, group, status):
    receipt_attempted[0] = True
    sys.stderr.write("\x1eEKO_OWNED_GROUP:%s:%s:%s:%s\x1f" % (nonce, kind, group, status))
    sys.stderr.flush()
try:
    import time
    started = time.monotonic()
    import os, signal, subprocess, errno
    needed = (os.killpg, os.getpgid, signal.SIGHUP, signal.SIGINT, signal.SIGTERM, signal.SIGKILL, signal.signal, time.monotonic, time.sleep, subprocess.Popen, subprocess.PIPE, subprocess.DEVNULL)
    duration = float(sys.argv[2])
    if sys.version_info[0] != 3 or not (0 <= duration < float("inf")):
        raise ValueError("invalid owned controller runtime")
except BaseException:
    receipt("not_started", 0, 125)
    sys.exit(125)
deadline = started + duration
stop_requested = [False]
def request_stop(signum, frame):
    stop_requested[0] = True
try:
    for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, request_stop)
except BaseException:
    receipt("not_started", 0, 125)
    sys.exit(125)
def absent(group):
    try:
        os.killpg(group, 0)
        return False
    except OSError as error:
        # EPERM/other errors are not closure. The original controller remains the owner.
        return error.errno == errno.ESRCH
def send(group, signum):
    try:
        os.killpg(group, signum)
    except OSError:
        pass
def closed(process, group, force_stop=False):
    stop_at = None
    killed = False
    expired = False
    while True:
        now = time.monotonic()
        status = process.poll()
        if now >= deadline:
            expired = True
        if status is not None and absent(group):
            return status, expired
        if force_stop or stop_requested[0] or now >= deadline:
            if now >= deadline:
                expired = True
            if stop_at is None:
                stop_at = now
                send(group, signal.SIGTERM)
            # Cleanup never starts another execution budget or extends the original deadline.
            if not killed and now >= min(deadline, stop_at + 5.0):
                send(group, signal.SIGKILL)
                killed = True
        time.sleep(0.01)
probe = None
process = None
status = 125
group = 0
try:
    if time.monotonic() >= deadline or stop_requested[0]:
        receipt("not_started", 0, 124)
        sys.exit(124)
    # Actual self-owned primitive probe before any user payload; a name/version is not a proof.
    probe = subprocess.Popen([sys.executable, "-S", "-E", "-c", "import sys; sys.stdin.buffer.read(1)"],
        stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True)
    probe_group = os.getpgid(probe.pid)
    if probe_group != probe.pid:
        raise RuntimeError("session/group primitive was not established")
    os.killpg(probe_group, 0)
    closed(probe, probe_group, True)
    probe = None
    if time.monotonic() >= deadline or stop_requested[0]:
        receipt("not_started", 0, 124)
        sys.exit(124)
    # start_new_session uses the standard atomic child startup hook, not hand-written preexec code.
    process = subprocess.Popen(["/bin/sh", "-c", sys.argv[1]], start_new_session=True)
    group = process.pid
    status, expired = closed(process, group)
    if expired:
        status = 124
    elif status < 0:
        status = 128 - status
    receipt("closed", group, status)
except SystemExit:
    raise
except BaseException:
    if receipt_attempted[0]:
        raise
    if process is not None:
        status, expired = closed(process, group, True)
        status = 124 if expired else (128 - status if status < 0 else status)
        receipt("closed", group, status)
    else:
        # Probe has no descendants/user workload; still reap its exact original child before refusal.
        if probe is not None:
            try:
                probe.kill()
            except OSError:
                pass
            probe.wait()
        receipt("not_started", 0, 125)
        status = 125
sys.exit(status)
"#;
    let duration = format!("{}.{:09}", timeout.as_secs(), timeout.subsec_nanos());
    let invocation = format!(
        "exec python3 -S -E -c {} {} {} {}",
        shell_quote(controller).map_err(unavailable)?,
        shell_quote(script).map_err(unavailable)?,
        shell_quote(&duration).map_err(unavailable)?,
        shell_quote(nonce).map_err(unavailable)?
    );
    // No Python3 means an explicit no-payload receipt, not a transport EOF/ordinary exit assertion.
    Ok(format!(
        "if command -v python3 >/dev/null 2>&1; then {invocation}; fi\nprintf '\\036EKO_OWNED_GROUP:%s:not_started:0:125\\037' {} >&2\nexit 125",
        shell_quote(nonce).map_err(unavailable)?
    ))
}

fn command_script(command: &SandboxCommand) -> Result<String> {
    let cwd = command.working_dir.as_ref().ok_or_else(|| {
        unavailable("remote command requires its bound actual project/checkout directory")
    })?;
    let cwd = remote_path(cwd).map_err(unavailable)?;
    if !cwd.starts_with('/') {
        return Err(unavailable(
            "remote execution cwd must be an absolute path in the remote namespace",
        ));
    }
    let (program, args) = match &command.kind {
        CommandKind::Shell(script) => (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), script.clone()],
        ),
        CommandKind::Program { program, args } => (program.clone(), args.clone()),
        CommandKind::Code { language, code } => match language.as_str() {
            "python" | "python3" => ("python3".to_string(), vec!["-c".to_string(), code.clone()]),
            "javascript" | "js" => ("node".to_string(), vec!["-e".to_string(), code.clone()]),
            "bash" | "shell" | "sh" => {
                ("/bin/sh".to_string(), vec!["-c".to_string(), code.clone()])
            }
            _ => {
                return Err(unavailable(
                    "remote code language has no configured command adapter",
                ));
            }
        },
    };
    if program.is_empty() || program.starts_with('-') {
        return Err(unavailable(
            "remote program is empty or starts with an option",
        ));
    }
    let mut environment = command.env.iter().collect::<Vec<_>>();
    environment.sort_by_key(|(left, _)| *left);
    let mut invocation = Vec::new();
    for (key, value) in environment {
        if key.is_empty()
            || !key.chars().enumerate().all(|(index, value)| {
                value == '_' || value.is_ascii_alphabetic() || (index > 0 && value.is_ascii_digit())
            })
        {
            return Err(unavailable(
                "remote environment variable has an invalid name",
            ));
        }
        invocation.push(shell_quote(&format!("{key}={value}")).map_err(unavailable)?);
    }
    invocation.push(shell_quote(&program).map_err(unavailable)?);
    for argument in args {
        invocation.push(shell_quote(&argument).map_err(unavailable)?);
    }
    Ok(format!(
        "cd {} || exit 125; exec env {}",
        shell_quote(cwd).map_err(unavailable)?,
        invocation.join(" ")
    ))
}

/// 控制帧只在本次 nonce 匹配且为 EOF 的完整内部尾部时剥离；其它字节仍是 payload。
fn parse_remote_group_receipt(tail: &[u8], nonce: &str) -> Option<(usize, RemoteGroupReceipt)> {
    let prefix = format!("\u{1e}EKO_OWNED_GROUP:{nonce}:");
    let start = tail
        .windows(prefix.len())
        .rposition(|value| value == prefix.as_bytes())?;
    let frame = std::str::from_utf8(tail.get(start..)?).ok()?;
    let body = frame.strip_prefix(&prefix)?.strip_suffix('\u{1f}')?;
    let mut fields = body.split(':');
    let kind = fields.next()?;
    let group = fields.next()?.parse::<u32>().ok()?;
    let status = fields.next()?.parse::<u8>().ok()?;
    if fields.next().is_some() {
        return None;
    }
    let group_id = match kind {
        "closed" if group > 1 => Some(group),
        "not_started" if group == 0 && matches!(status, 124 | 125) => None,
        _ => return None,
    };
    Some((
        start,
        RemoteGroupReceipt {
            exit_code: i32::from(status),
            group_id,
        },
    ))
}

async fn read_stderr_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: u64,
    nonce: Option<String>,
) -> std::io::Result<RemoteStderrCapture> {
    let Some(nonce) = nonce else {
        let (text, bytes, truncated) = read_bounded(reader, limit).await?;
        return Ok(RemoteStderrCapture {
            text,
            bytes,
            truncated,
            receipt: None,
        });
    };
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut retained = Vec::new();
    let mut tail = std::collections::VecDeque::new();
    let mut payload_bytes = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        for byte in buffer.iter().take(count).copied() {
            tail.push_back(byte);
            if tail.len() > GROUP_RECEIPT_TAIL_BYTES
                && let Some(payload) = tail.pop_front()
            {
                payload_bytes = payload_bytes.saturating_add(1);
                if retained.len() < limit {
                    retained.push(payload);
                }
            }
        }
    }
    let tail: Vec<u8> = tail.into_iter().collect();
    let parsed = parse_remote_group_receipt(&tail, &nonce);
    let payload_tail = match parsed {
        Some((start, _)) => tail.get(..start).unwrap_or_default(),
        None => tail.as_slice(),
    };
    payload_bytes =
        payload_bytes.saturating_add(u64::try_from(payload_tail.len()).unwrap_or(u64::MAX));
    retained.extend(
        payload_tail
            .iter()
            .take(limit.saturating_sub(retained.len()))
            .copied(),
    );
    Ok(RemoteStderrCapture {
        text: String::from_utf8_lossy(&retained).into_owned(),
        bytes: payload_bytes,
        truncated: payload_bytes > u64::try_from(retained.len()).unwrap_or(u64::MAX),
        receipt: parsed.map(|(_, receipt)| receipt),
    })
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: u64,
) -> std::io::Result<(String, u64, bool)> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut retained = Vec::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        let remaining = limit.saturating_sub(retained.len());
        retained.extend(buffer.iter().take(read.min(remaining)).copied());
    }
    let truncated = total > u64::try_from(retained.len()).unwrap_or(u64::MAX);
    Ok((
        String::from_utf8_lossy(&retained).into_owned(),
        total,
        truncated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fixture_process_exists(target: i32, failures: &mut Vec<std::io::Error>) -> Option<bool> {
        let probe = || -> std::io::Result<bool> {
            if target.checked_abs().is_none_or(|target| target <= 1) {
                return Err(std::io::Error::other("invalid self-owned fixture target"));
            }
            // 复用控制器测试已有的 Python 条件；只有 ESRCH 认作不存在，EPERM 等错误须失败。
            let output = std::process::Command::new("python3")
                .args([
                    "-S",
                    "-E",
                    "-c",
                    r#"import os, sys
try:
    os.kill(int(sys.argv[1]), 0)
except ProcessLookupError:
    sys.exit(3)
except OSError as error:
    print(error, file=sys.stderr)
    sys.exit(4)
"#,
                ])
                .arg(target.to_string())
                .output()?;
            match output.status.code() {
                Some(0) => Ok(true),
                Some(3) => Ok(false),
                _ => Err(std::io::Error::other(format!(
                    "self-owned fixture probe failed: {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                ))),
            }
        };
        match probe() {
            Ok(exists) => Some(exists),
            Err(error) => {
                // 延迟返回错误，先让原测试释放门闩、取消并收集自己启动的物理任务。
                failures.push(error);
                None
            }
        }
    }

    #[cfg(unix)]
    fn fixture_kill_group(group: i32) {
        if group > 1 {
            // 仅作自有本机夹具紧急回收；复用旧测试的 /bin/sh，不依赖可能失败的 Python 探针。
            // 这个状态不提供远端停止证明，测试仍须观察原组 ESRCH 才能通过。
            let _ = std::process::Command::new("/bin/sh")
                .args(["-c", "kill -KILL \"$1\" 2>/dev/null", "owned-fixture"])
                .arg(format!("-{group}"))
                .status();
        }
    }

    #[tokio::test]
    async fn owned_group_receipt_is_split_at_read_boundaries_without_retaining_unbounded_stderr()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let payload = format!("{}{}", "x".repeat(4096), "中🙂".repeat(40_000));
        let frame = format!("\u{1e}EKO_OWNED_GROUP:{nonce}:closed:23456:101\u{1f}");
        let bytes = format!("{payload}{frame}").into_bytes();
        let (mut writer, reader) = tokio::io::duplex(17);
        let send = tokio::spawn(async move {
            for chunk in bytes.chunks(7) {
                writer.write_all(chunk).await?;
            }
            Ok::<_, std::io::Error>(())
        });
        let capture = read_stderr_bounded(reader, 4096, Some(nonce)).await?;
        send.await??;
        assert_eq!(capture.text, "x".repeat(4096));
        assert_eq!(capture.bytes, u64::try_from(payload.len())?);
        assert!(capture.truncated);
        let receipt = capture
            .receipt
            .ok_or("matching original group receipt lost behind output limit")?;
        assert_eq!(receipt.group_id, Some(23456));
        assert_eq!(receipt.exit_code, 101);
        Ok(())
    }

    #[tokio::test]
    async fn mismatched_missing_or_incomplete_group_receipts_do_not_strip_payload()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let other = uuid::Uuid::new_v4().simple().to_string();
        for data in [
            "stderr中文🙂".to_string(),
            format!("stderr\u{1e}EKO_OWNED_GROUP:{other}:closed:23456:0\u{1f}"),
            format!("stderr\u{1e}EKO_OWNED_GROUP:{nonce}:closed:23456:0"),
            format!("stderr\u{1e}EKO_OWNED_GROUP:{nonce}:closed:23456:0\u{1f}later-payload"),
            format!("stderr\u{1e}EKO_OWNED_GROUP:{nonce}:closed:0:0\u{1f}"),
        ] {
            let capture = read_stderr_bounded(data.as_bytes(), 4096, Some(nonce.clone())).await?;
            assert!(capture.receipt.is_none());
            assert_eq!(capture.text, data);
            assert_eq!(capture.bytes, u64::try_from(data.len())?);
        }
        let legacy = format!("payload\u{1e}EKO_OWNED_GROUP:{nonce}:closed:23456:0\u{1f}");
        let capture = read_stderr_bounded(legacy.as_bytes(), 4096, None).await?;
        assert_eq!(capture.text, legacy);
        assert!(capture.receipt.is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn program_arguments_and_actual_checkout_are_not_shell_fragments()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let directory = temporary.path().join("远端 checkout '二'");
        std::fs::create_dir(&directory)?;
        let argument = "引号' $(touch wrong) 🦀";
        let command = SandboxCommand::program("printf", vec!["%s".into(), argument.into()])
            .with_working_dir(&directory);
        let script = command_script(&command)?;
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .output()?;
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout)?, argument);
        assert!(!directory.join("wrong").exists());
        Ok(())
    }

    #[test]
    fn remote_isolation_is_not_the_local_machine_sandbox() {
        let mut command = SandboxCommand::shell("true");
        command.minimum_isolation = Some(IsolationLevel::OsSandbox);
        assert!(validate_capabilities(&command, &ResourceLimits::unrestricted()).is_err());
        assert!(
            validate_capabilities(&SandboxCommand::shell("true"), &ResourceLimits::default())
                .is_err()
        );
    }

    #[cfg(unix)]
    struct GuardDrop(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    #[cfg(unix)]
    impl Drop for GuardDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
    }

    #[cfg(unix)]
    fn fixture_connection() -> std::result::Result<
        (
            Arc<crate::terminal::TerminalService>,
            Arc<ProjectSshConnection>,
        ),
        String,
    > {
        let service = crate::terminal::TerminalService::new();
        let target = crate::terminal::TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let connection = ProjectSshConnection::prepare(&service, "fixture", target.clone())?;
        connection
            .effective_target
            .set(target)
            .map_err(|_| "duplicate fixture target".to_string())?;
        service.project_connections.insert(
            "fixture".into(),
            super::super::ProjectSshOwner::new(Arc::clone(&connection)),
        );
        Ok((service, connection))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn owned_controller_proves_the_original_group_after_root_exit_for_normal_cancel_and_deadline()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for mode in ["natural", "cancel", "deadline"] {
            let directory = tempfile::tempdir()?;
            let (service, connection) = fixture_connection()?;
            let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let cancel = Arc::new(CancellationToken::new());
            let timeout = if mode == "natural" {
                Duration::from_secs(10)
            } else {
                Duration::from_secs(2)
            };
            let tail = if mode == "deadline" { "; sleep 1" } else { "" };
            let payload = format!(
                "echo $$ > leader.pid; sh -c 'while [ ! -e release ]; do sleep 0.02; done' >/dev/null 2>&1 & echo $! > descendant.pid{tail}"
            );
            let mut command = SandboxCommand::shell(payload)
                .with_working_dir(directory.path())
                .with_timeout(timeout)
                .with_resource_guards(vec![echo_agent::tools::InvocationResourceGuard::new(
                    GuardDrop(dropped.clone()),
                )]);
            command.require_physical_resource_ownership = true;
            let control_group_file = directory.path().join("controller.pgid");
            let diagnostic_hex: String = control_group_file
                .to_str()
                .ok_or("invalid controller fixture path")?
                .as_bytes()
                .iter()
                .map(|value| format!("{value:02x}"))
                .collect();
            let owner = connection.clone();
            let owner_cancel = cancel.clone();
            let mut physical = tokio::spawn(async move {
                ProjectSshSandbox::execute_with_launcher(owner, command, ResourceLimits::unrestricted(), Some(owner_cancel), move |_, script| {
                    // 仅在原启动接缝记录实际 PGID，供夹具失败时回收；不修改终结／预算／凭据逻辑。
                    let hook = format!("group = process.pid\n    with open(bytes.fromhex(\"{diagnostic_hex}\").decode(\"utf-8\"), \"w\") as group_record: group_record.write(str(group))");
                    let observed = script.replace("group = process.pid", &hook);
                    let mut command = tokio::process::Command::new("/bin/sh");
                    command.args(["-c", &observed]);
                    Ok(command)
                }).await
            });
            let mut pids = None;
            let mut payload_started = tokio::time::Instant::now();
            for _ in 0..500 {
                let leader = std::fs::read_to_string(directory.path().join("leader.pid"))
                    .ok()
                    .and_then(|value| value.trim().parse::<i32>().ok());
                let descendant = std::fs::read_to_string(directory.path().join("descendant.pid"))
                    .ok()
                    .and_then(|value| value.trim().parse::<i32>().ok());
                if let (Some(leader), Some(descendant)) = (leader, descendant) {
                    pids = Some((leader, descendant));
                    payload_started = tokio::time::Instant::now();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let Some((leader, descendant)) = pids else {
                cancel.cancel();
                let diagnostic_group = std::fs::read_to_string(&control_group_file)
                    .ok()
                    .and_then(|value| value.trim().parse::<i32>().ok());
                if let Some(group) = diagnostic_group.filter(|group| *group > 1) {
                    // 仅回收这个启动接缝记录的自有组。
                    fixture_kill_group(group);
                }
                let closing = tokio::time::timeout(Duration::from_secs(8), &mut physical).await;
                if closing.is_ok() {
                    service.close_project_ssh("fixture").await?;
                }
                return Err("owned controller did not reach a real payload; prerequisite/fixture failed, no target GREEN inferred".into());
            };
            let mut probe_failures = Vec::new();
            for _ in 0..300 {
                if fixture_process_exists(leader, &mut probe_failures) != Some(true) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let root_gone = fixture_process_exists(leader, &mut probe_failures) == Some(false);
            let descendant_live =
                fixture_process_exists(descendant, &mut probe_failures) == Some(true);
            if !probe_failures.is_empty() {
                cancel.cancel();
            }
            let guard_while_live = dropped.load(std::sync::atomic::Ordering::Acquire);
            let early = tokio::time::timeout(Duration::from_millis(20), &mut physical)
                .await
                .ok();
            let completed_while_live = early.is_some();
            if !probe_failures.is_empty() || early.is_some() || mode == "natural" {
                std::fs::write(directory.path().join("release"), "release")?;
            } else if mode == "cancel" {
                cancel.cancel();
            } else {
                tokio::time::sleep_until(
                    payload_started
                        .checked_add(Duration::from_millis(2200))
                        .ok_or("fixture deadline overflow")?,
                )
                .await;
                std::fs::write(directory.path().join("release"), "release")?;
            }
            let result = match early {
                Some(result) => result,
                None => match tokio::time::timeout(Duration::from_secs(8), &mut physical).await {
                    Ok(result) => result,
                    Err(_) => {
                        // 唯一自有本机 controller fixture 的紧急回收，不是假远端 stop API。
                        fixture_kill_group(leader);
                        let _ = std::fs::write(directory.path().join("release"), "release");
                        physical.await
                    }
                },
            };
            let mut group_closed = false;
            for _ in 0..200 {
                match fixture_process_exists(-leader, &mut probe_failures) {
                    Some(false) => {
                        group_closed = true;
                        break;
                    }
                    Some(true) => tokio::time::sleep(Duration::from_millis(10)).await,
                    None => break,
                }
            }
            if !group_closed {
                fixture_kill_group(leader);
                let _ = std::fs::write(directory.path().join("release"), "release");
            }
            eprintln!(
                "owned SSH controller fixture: mode={mode}; root_gone={root_gone}; descendant_live={descendant_live}; guard_while_live={guard_while_live}; early={completed_while_live}; result={result:?}"
            );
            service.close_project_ssh("fixture").await?;
            if let Some(error) = probe_failures.into_iter().next() {
                return Err(error.into());
            }
            assert!(root_gone && descendant_live && group_closed);
            assert_eq!(guard_while_live, 0);
            assert!(
                !completed_while_live,
                "non255/root EOF falsely settled the owned group"
            );
            match mode {
                "natural" => {
                    let result = result??;
                    assert!(result.success());
                }
                "cancel" => assert!(result?.is_err()),
                _ => {
                    let result = result??;
                    assert!(
                        result.timed_out,
                        "controller reset its original budget after root exit"
                    );
                }
            }
            assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non255_without_original_group_receipt_keeps_the_original_unknown_owner()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let (service, connection) = fixture_connection()?;
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut command = fixture_command(&dropped).with_timeout(Duration::from_secs(10));
        command.require_physical_resource_ownership = true;
        let cwd = directory.path().to_path_buf();
        let owner = connection.clone();
        let mut physical = tokio::spawn(async move {
            ProjectSshSandbox::execute_with_launcher(owner, command, ResourceLimits::unrestricted(), None, move |_, _script| {
                // 故意模拟丢失控制凭据的transport：实际本机root退出0/EOF，原组后代仍活；不是正常controller输入。
                let mut command = tokio::process::Command::new("/bin/sh");
                // 消费者按文件存在判断就绪；同目录 rename 只发布已完整写入的 PID。
                command.args(["-c", "echo $$ > leader.pid.tmp; mv leader.pid.tmp leader.pid; sh -c 'while [ ! -e release ]; do sleep 0.02; done' >/dev/null 2>&1 & echo $! > descendant.pid.tmp; mv descendant.pid.tmp descendant.pid; exit 0"])
                    .current_dir(cwd).process_group(0);
                Ok(command)
            }).await
        });
        for _ in 0..200 {
            if directory.path().join("leader.pid").is_file()
                && directory.path().join("descendant.pid").is_file()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let leader: i32 = std::fs::read_to_string(directory.path().join("leader.pid"))?
            .trim()
            .parse()?;
        let descendant: i32 = std::fs::read_to_string(directory.path().join("descendant.pid"))?
            .trim()
            .parse()?;
        let mut probe_failures = Vec::new();
        let live = fixture_process_exists(descendant, &mut probe_failures) == Some(true);
        let mut resource = None;
        for _ in 0..200 {
            resource = connection
                .retained_children
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .find_map(|entry| match entry {
                    super::super::RetainedSshResource::RemoteOperation(resource) => {
                        Some(resource.clone())
                    }
                    _ => None,
                });
            if resource
                .as_ref()
                .is_some_and(|resource| resource.stop_is_unknown())
                || physical.is_finished()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let held = resource
            .as_ref()
            .is_some_and(|resource| resource.stop_is_unknown() && !resource.is_settled())
            && dropped.load(std::sync::atomic::Ordering::Acquire) == 0;
        let early = tokio::time::timeout(Duration::from_millis(20), &mut physical)
            .await
            .ok();
        let returned_while_live = early.is_some();
        std::fs::write(directory.path().join("release"), "release")?;
        let mut closed = false;
        for _ in 0..200 {
            match fixture_process_exists(-leader, &mut probe_failures) {
                Some(false) => {
                    closed = true;
                    break;
                }
                Some(true) => tokio::time::sleep(Duration::from_millis(10)).await,
                None => break,
            }
        }
        if !closed {
            // 紧急回收仅作用于本夹具原始自有组。
            fixture_kill_group(leader);
            return Err("self-owned missing-receipt fixture did not close its original group; no stop proof inferred".into());
        }
        // 唯一自有本机fixture已真正组闭合；不把此能力暴露为远端unknown恢复API。
        if let Some(resource) = resource {
            resource.complete_with_proof();
        }
        let result = match early {
            Some(result) => result?,
            None => tokio::time::timeout(Duration::from_secs(3), physical).await??,
        };
        service.close_project_ssh("fixture").await?;
        assert!(live && held);
        assert!(
            !returned_while_live,
            "non255/EOF was used as a substitute for the missing original group receipt"
        );
        assert!(result.is_err());
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_runtime_or_actual_group_primitive_refuses_before_payload()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for missing in ["runtime", "primitive"] {
            let directory = tempfile::tempdir()?;
            let (service, connection) = fixture_connection()?;
            let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let marker = directory.path().join("must-not-run");
            let mut command = SandboxCommand::shell("printf effect > must-not-run")
                .with_working_dir(directory.path())
                .with_timeout(Duration::from_secs(5))
                .with_resource_guards(vec![echo_agent::tools::InvocationResourceGuard::new(
                    GuardDrop(dropped.clone()),
                )]);
            command.require_physical_resource_ownership = true;
            let result = ProjectSshSandbox::execute_with_launcher(
                connection,
                command,
                ResourceLimits::unrestricted(),
                None,
                move |_, script| {
                    let mut child = tokio::process::Command::new("/bin/sh");
                    let script = if missing == "primitive" {
                        // 实际Python仍在；只注入缺少POSIX primitive，不能靠名字/版本通过。
                        script.replace(
                            "needed = (os.killpg",
                            "del os.killpg\n    needed = (os.killpg",
                        )
                    } else {
                        script
                    };
                    child.args(["-c", &script]);
                    if missing == "runtime" {
                        child.env("PATH", "/nonexistent-controlled-path");
                    }
                    Ok(child)
                },
            )
            .await;
            let ran = marker.exists();
            service.close_project_ssh("fixture").await?;
            assert!(
                matches!(result, Err(echo_agent::error::ReactError::Tool(error))
                if matches!(error.as_ref(), echo_agent::error::ToolError::RemoteFailure { message, failure, .. }
                    if message.contains("payload was not started")
                        && failure.category == echo_agent::tools::ToolFailureCategory::Unavailable
                        && failure.side_effect == echo_agent::tools::ToolSideEffect::None))
            );
            assert!(!ran);
            assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        }
        Ok(())
    }

    #[cfg(unix)]
    async fn wait_for_unknown(
        connection: &ProjectSshConnection,
    ) -> std::result::Result<Arc<RemoteOperationResources>, String> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let resource = connection
                    .retained_children
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .iter()
                    .find_map(|resource| match resource {
                        super::super::RetainedSshResource::RemoteOperation(resource)
                            if resource.stop_is_unknown() =>
                        {
                            Some(Arc::clone(resource))
                        }
                        _ => None,
                    });
                if let Some(resource) = resource {
                    return resource;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "fixture never reached original unknown state".to_string())
    }

    #[cfg(unix)]
    fn fixture_command(dropped: &Arc<std::sync::atomic::AtomicUsize>) -> SandboxCommand {
        SandboxCommand::program("/bin/sh", Vec::new())
            .with_working_dir("/")
            .with_resource_guards(vec![echo_agent::tools::InvocationResourceGuard::new(
                GuardDrop(Arc::clone(dropped)),
            )])
    }

    #[cfg(unix)]
    struct ReleaseLocalFixture(std::path::PathBuf);
    #[cfg(unix)]
    impl Drop for ReleaseLocalFixture {
        fn drop(&mut self) {
            let _release = std::fs::write(&self.0, b"release");
        }
    }

    #[cfg(unix)]
    struct ShellReceiptFixture {
        connection: Arc<ProjectSshConnection>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[cfg(unix)]
    impl SandboxExecutor for ShellReceiptFixture {
        fn name(&self) -> &str {
            "ssh"
        }
        fn isolation_level(&self) -> IsolationLevel {
            IsolationLevel::None
        }
        fn is_available(&self) -> BoxFuture<'_, bool> {
            Box::pin(async { true })
        }
        fn execute(&self, command: SandboxCommand) -> BoxFuture<'_, Result<ExecutionResult>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let connection = self.connection.clone();
            Box::pin(ProjectSshSandbox::execute_with_launcher(
                connection,
                command,
                ResourceLimits::unrestricted(),
                None,
                |_, _| {
                    let mut command = tokio::process::Command::new("/bin/sh");
                    command.args(["-c", "exit 255"]);
                    Ok(command)
                },
            ))
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_consumer_keeps_original_ssh_unknown_failure_and_guards()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use echo_agent::tools::{
            Tool, ToolContext, ToolFailureCategory, ToolRecoveryAction, ToolSideEffect,
        };
        let (service, connection) = fixture_connection()?;
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool = echo_agent::tools::shell::ShellTool::new().with_sandbox(Arc::new(
            ShellReceiptFixture {
                connection: connection.clone(),
                calls: calls.clone(),
            },
        ));
        let context = ToolContext {
            working_dir: Some(std::path::PathBuf::from("/")),
            resource_guards: vec![echo_agent::tools::InvocationResourceGuard::new(GuardDrop(
                dropped.clone(),
            ))],
            ..Default::default()
        };
        let parameters = std::collections::HashMap::from([
            ("command".to_string(), serde_json::json!("printf fixture")),
            ("timeout".to_string(), serde_json::json!(1)),
        ]);
        let result = tokio::time::timeout(
            Duration::from_secs(8),
            tool.execute_with_context(parameters, &context),
        )
        .await??;
        drop(context);
        tokio::task::yield_now().await;
        let resource = wait_for_unknown(&connection).await?;
        let retained = !resource.is_settled()
            && resource.stop_is_unknown()
            && dropped.load(std::sync::atomic::Ordering::Acquire) == 0;
        // 原本机fixture的child退出已被同一wait观察；在断言前收回夹具，
        // 不把该本机清理能力暴露为远端unknown恢复API。
        resource.complete_with_proof();
        tokio::task::yield_now().await;
        service.close_project_ssh("fixture").await?;
        assert!(retained);
        assert!(!result.success);
        let failure = result.failure.ok_or("Shell lost structured failure")?;
        assert_eq!(failure.category, ToolFailureCategory::PartialSideEffect);
        assert_eq!(failure.side_effect, ToolSideEffect::Possible);
        assert_eq!(failure.recovery, ToolRecoveryAction::VerifyThenRetry);
        assert!(!failure.allows_automatic_retry());
        assert!(
            result
                .metadata
                .get("duration_ms")
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|millis| millis >= 1000),
            "buffered execution already exceeded its one-second budget"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_consumer_keeps_known_not_started_ssh_failure_without_effects()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use echo_agent::tools::{
            Tool, ToolContext, ToolFailureCategory, ToolRecoveryAction, ToolSideEffect,
        };
        let (service, connection) = fixture_connection()?;
        connection.begin_shutdown();
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool = echo_agent::tools::shell::ShellTool::new().with_sandbox(Arc::new(
            ShellReceiptFixture {
                connection: connection.clone(),
                calls: calls.clone(),
            },
        ));
        let context = ToolContext {
            working_dir: Some(std::path::PathBuf::from("/")),
            resource_guards: vec![echo_agent::tools::InvocationResourceGuard::new(GuardDrop(
                dropped.clone(),
            ))],
            ..Default::default()
        };
        let parameters = std::collections::HashMap::from([(
            "command".to_string(),
            serde_json::json!("printf fixture"),
        )]);
        let result = tool.execute_with_context(parameters, &context).await?;
        drop(context);
        tokio::task::yield_now().await;
        service.close_project_ssh("fixture").await?;
        assert!(!result.success);
        let failure = result.failure.ok_or("Shell lost structured failure")?;
        assert_eq!(failure.category, ToolFailureCategory::Unavailable);
        assert_eq!(failure.side_effect, ToolSideEffect::None);
        assert_eq!(failure.recovery, ToolRecoveryAction::RestoreThenRetry);
        assert!(!failure.allows_automatic_retry());
        assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn wrapper_regression_stdin_and_payload_diagnostics_are_preserved()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_service, connection) = fixture_connection()?;
        let directory = tempfile::tempdir()?;
        let launch_directory = directory.path().to_path_buf();
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let input = "输入不能丢失 🧪\n";
        let mut command = fixture_command(&dropped);
        command.kind = CommandKind::Shell("cat; printf payload_diagnostic >&2; exit 7".into());
        command.stdin = Some(input.to_string());
        let result = ProjectSshSandbox::execute_with_launcher(
            connection.clone(),
            command,
            ResourceLimits::unrestricted(),
            None,
            move |_, script| {
                let mut command = tokio::process::Command::new("/bin/sh");
                command
                    .args(["-c", &script])
                    .env("TMPDIR", launch_directory);
                Ok(command)
            },
        )
        .await?;
        assert_eq!(result.exit_code, 7);
        assert_eq!(result.stdout, input);
        assert_eq!(result.stderr, "payload_diagnostic");
        assert!(!result.timed_out);
        assert!(!result.cancelled);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(
            connection
                .active_operations
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn wrapper_regression_cancel_natural_124_is_not_claimed_killed()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_service, connection) = fixture_connection()?;
        let directory = tempfile::tempdir()?;
        let started = directory.path().join("started");
        let release = directory.path().join("release");
        let _release = ReleaseLocalFixture(release.clone());
        let launch_directory = directory.path().to_path_buf();
        let script = format!(
            "printf started > {}; while [ ! -f {} ]; do /bin/sleep 0.01; done; exit 124",
            shell_quote(started.to_str().ok_or("invalid fixture path")?)?,
            shell_quote(release.to_str().ok_or("invalid fixture path")?)?,
        );
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancel = Arc::new(CancellationToken::new());
        let mut command = fixture_command(&dropped).with_cancel(Some(cancel.clone()));
        command.kind = CommandKind::Shell(script);
        command.timeout = Duration::from_secs(30);
        let operation = ProjectSshSandbox::execute_with_launcher(
            connection.clone(),
            command,
            ResourceLimits::unrestricted(),
            None,
            move |_, script| {
                let mut command = tokio::process::Command::new("/bin/sh");
                command
                    .args(["-c", &script])
                    .env("TMPDIR", launch_directory);
                Ok(command)
            },
        );
        tokio::pin!(operation);
        assert!(matches!(
            futures::poll!(operation.as_mut()),
            std::task::Poll::Pending
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        cancel.cancel();
        // 负载仍被原release门阻塞，此次poll只能先消费取消请求；无需猜延迟。
        assert!(matches!(
            futures::poll!(operation.as_mut()),
            std::task::Poll::Pending
        ));
        std::fs::write(&release, b"release")?;
        let result = tokio::time::timeout(Duration::from_secs(5), operation.as_mut()).await?;
        let error = result
            .err()
            .ok_or("cancelled observation unexpectedly succeeded")?;
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        let message = error.to_string();
        assert!(message.contains("124"));
        assert!(
            !message.contains("实际终止"),
            "natural 124 is not a watchdog kill receipt: {message}"
        );
        assert!(message.contains("退出原因未确认"));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn wrapper_regression_none_descendant_pipes_keep_unknown_until_original_settlement()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (service, connection) = fixture_connection()?;
        let directory = tempfile::tempdir()?;
        let ready = directory.path().join("descendant-ready");
        let release = directory.path().join("descendant-release");
        let done = directory.path().join("descendant-done");
        let _release = ReleaseLocalFixture(release.clone());
        let descendant = format!(
            "trap '' HUP TERM; printf started > {}; while [ ! -f {} ]; do /bin/sleep 0.01; done; printf descendant_stdout; printf descendant_stderr >&2; printf done > {}",
            shell_quote(ready.to_str().ok_or("invalid fixture path")?)?,
            shell_quote(release.to_str().ok_or("invalid fixture path")?)?,
            shell_quote(done.to_str().ok_or("invalid fixture path")?)?,
        );
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut command = fixture_command(&dropped);
        command.kind =
            CommandKind::Shell(format!("sh -c {} & wait \"$!\"", shell_quote(&descendant)?));
        command.timeout = Duration::from_secs(1);
        assert!(!command.require_physical_resource_ownership);
        // 复用现有观察测试的2秒外层调度容差，生产预算仍为1秒+CLOSE的5秒。
        let bound =
            command.timeout + crate::terminal::CLOSE_OBSERVATION_TIMEOUT + Duration::from_secs(2);
        let launch_directory = directory.path().to_path_buf();
        let started = tokio::time::Instant::now();
        let mut observer = tokio::spawn(ProjectSshSandbox::execute_with_launcher(
            connection.clone(),
            command,
            ResourceLimits::unrestricted(),
            None,
            move |_, script| {
                let mut command = tokio::process::Command::new("/bin/sh");
                command
                    .args(["-c", &script])
                    .env("TMPDIR", launch_directory);
                Ok(command)
            },
        ));
        // 只等真实后代已进入自有门闩；不靠预先sleep猜测到期与管道状态。
        let descendant_ready = tokio::time::timeout(Duration::from_secs(2), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let original = connection
            .retained_children
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find_map(|resource| match resource {
                super::super::RetainedSshResource::RemoteOperation(resource) => {
                    Some(Arc::clone(resource))
                }
                _ => None,
            });
        let observed = tokio::time::timeout_at(started + bound, &mut observer).await;
        let returned_bounded = observed.is_ok();
        let resource = wait_for_unknown(&connection).await;
        let capture_retained = || {
            resource.as_ref().is_ok_and(|resource| {
                !resource.is_settled()
                    && resource.stop_is_unknown()
                    && !resource
                        .test_local_exit_observed
                        .load(std::sync::atomic::Ordering::Acquire)
                    && original
                        .as_ref()
                        .is_some_and(|original| Arc::ptr_eq(original, resource))
            }) && dropped.load(std::sync::atomic::Ordering::Acquire) == 0
                && connection
                    .active_operations
                    .load(std::sync::atomic::Ordering::Acquire)
                    == 1
                && !done.exists()
        };
        let retained_before_root_exit = capture_retained();
        // 原root退出与调用方返回Unknown不是同一个时点；timer回收也须在
        // 同一个started+bound绝对截止前观察，不能另给一轮等待预算。
        // 只探测原spawn发布的正PID；不枚举、猜测或终止任何进程组。
        let local_root = tokio::time::timeout_at(started + bound, async {
            let pid = original
                .as_ref()
                .and_then(|resource| resource.child_id)
                .ok_or_else(|| std::io::Error::other("fixture original PID was not published"))?;
            loop {
                let status = tokio::process::Command::new("/bin/sh")
                    .args(["-c", "kill -0 \"$1\" 2>/dev/null", "owned-wrapper"])
                    .arg(pid.to_string())
                    .status()
                    .await?;
                if !status.success() {
                    return Ok::<(), std::io::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let retained_after_root_exit =
            local_root.as_ref().is_ok_and(|result| result.is_ok()) && capture_retained();
        // 所有断言前先释放唯一自有门闩。不得complete_with_proof/abort或补发kill；
        // 原wait和原stdout/stderr join须自行完成，并由原physical路径释放guard。
        let released = std::fs::write(&release, b"release");
        let settled = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if connection
                    .active_operations
                    .load(std::sync::atomic::Ordering::Acquire)
                    == 0
                    && dropped.load(std::sync::atomic::Ordering::Acquire) == 1
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let result = match observed {
            Ok(result) => result,
            Err(_) => tokio::time::timeout(Duration::from_secs(5), &mut observer).await?,
        };
        let closed = service.close_project_ssh("fixture").await;
        descendant_ready?;
        released?;
        settled?;
        closed?;
        let resource = resource?;
        let error = result?
            .err()
            .ok_or("bounded unknown unexpectedly succeeded")?;
        assert!(
            returned_bounded,
            "None observer exceeded its existing budget"
        );
        assert!(
            retained_before_root_exit,
            "Unknown must retain the original operation and guard"
        );
        local_root??;
        assert!(
            retained_after_root_exit,
            "root exit must not settle the original pipes, operation or guard"
        );
        assert!(error.to_string().contains("停止观察期限已到"));
        assert_eq!(
            echo_agent::tools::ToolFailure::from_error(&error, false).side_effect,
            echo_agent::tools::ToolSideEffect::Possible
        );
        assert!(done.exists());
        assert!(resource.is_settled());
        assert!(!resource.stop_is_unknown());
        assert!(
            resource
                .test_local_exit_observed
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn wrapper_regression_transport_loss_observation_is_bounded_and_keeps_unknown()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (service, connection) = fixture_connection()?;
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut command = fixture_command(&dropped);
        command.timeout = Duration::from_millis(20);
        let mut observer = tokio::spawn(ProjectSshSandbox::execute_with_launcher(
            connection.clone(),
            command,
            ResourceLimits::unrestricted(),
            None,
            |_, _| {
                let mut command = tokio::process::Command::new("/bin/sh");
                command.args(["-c", "exit 255"]);
                Ok(command)
            },
        ));
        let resource = wait_for_unknown(&connection).await?;
        let observed = tokio::time::timeout(
            crate::terminal::CLOSE_OBSERVATION_TIMEOUT + Duration::from_secs(2),
            &mut observer,
        )
        .await;
        let returned = observed.is_ok();
        let retained = !resource.is_settled()
            && resource.stop_is_unknown()
            && dropped.load(std::sync::atomic::Ordering::Acquire) == 0;
        // 仅此自有本机夹具的退出已被原wait观察；先完成cleanup，不能把
        // SSH255推广为远端停止证明，也不能为让断言通过而提前清债。
        resource.complete_with_proof();
        let result = match observed {
            Ok(result) => result?,
            Err(_) => observer.await?,
        };
        tokio::task::yield_now().await;
        service.close_project_ssh("fixture").await?;
        assert!(
            returned,
            "the observer must return bounded unknown instead of hanging"
        );
        assert!(
            retained,
            "bounded observation cannot settle the original unknown operation"
        );
        assert!(result.is_err());
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transport_255_keeps_original_guard_and_close_unknown_after_observer_drop()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (service, connection) = fixture_connection()?;
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let operation = ProjectSshSandbox::execute_with_launcher(
            Arc::clone(&connection),
            fixture_command(&dropped),
            ResourceLimits::unrestricted(),
            None,
            |_, _| {
                let mut command = tokio::process::Command::new("/bin/sh");
                command.args(["-c", "exit 255"]);
                Ok(command)
            },
        );
        let observer = tokio::spawn(operation);
        let resource = wait_for_unknown(&connection).await?;
        observer.abort();
        let _original_observer = observer.await;
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 0);
        assert_eq!(
            connection
                .active_operations
                .load(std::sync::atomic::Ordering::Acquire),
            1
        );
        assert!(service.close_project_ssh("fixture").await.is_err());
        assert!(service.project_ssh("fixture").is_ok());
        connection.retry_retained_children().await?;
        assert!(!resource.is_settled());
        // Test cleanup only: this is a self-owned local fixture process, whose
        // actual local exit has been observed by the production path. It is
        // not a remote recovery API or a claim that SSH255 proves remote stop.
        resource.complete_with_proof();
        service.close_project_ssh("fixture").await?;
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_exit_receipt_is_nonterminal_but_known_125_releases_possible_effects()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (service, connection) = fixture_connection()?;
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut observer = tokio::spawn(ProjectSshSandbox::execute_with_launcher(
            Arc::clone(&connection),
            fixture_command(&dropped),
            ResourceLimits::unrestricted(),
            None,
            |_, _| {
                let mut command = tokio::process::Command::new("/bin/sh");
                command.args(["-c", "kill -TERM $$"]);
                Ok(command)
            },
        ));
        let resource = wait_for_unknown(&connection).await?;
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut observer)
                .await
                .is_err()
        );
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 0);
        resource.complete_with_proof(); // Actual self-owned local fixture cleanup, not remote proof.
        assert!(observer.await?.is_err());
        service.close_project_ssh("fixture").await?;

        let (service, connection) = fixture_connection()?;
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let result = ProjectSshSandbox::execute_with_launcher(
            Arc::clone(&connection),
            fixture_command(&dropped),
            ResourceLimits::unrestricted(),
            None,
            |_, _| {
                let mut command = tokio::process::Command::new("/bin/sh");
                command.args(["-c", "exit 125"]);
                Ok(command)
            },
        )
        .await;
        let error = result
            .err()
            .ok_or("fixture125 was incorrectly successful")?;
        assert_eq!(
            echo_agent::tools::ToolFailure::from_error(&error, false).side_effect,
            echo_agent::tools::ToolSideEffect::Possible
        );
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(
            connection
                .active_operations
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        service.close_project_ssh("fixture").await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_and_cancel_only_observe_the_same_physical_invocation_until_exit()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_service, connection) = fixture_connection()?;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("release");
        let _release = ReleaseLocalFixture(path.clone());
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancel = Arc::new(CancellationToken::new());
        let mut command = fixture_command(&dropped).with_cancel(Some(Arc::clone(&cancel)));
        command.timeout = Duration::from_millis(10);
        let mut observer = tokio::spawn(ProjectSshSandbox::execute_with_launcher(
            Arc::clone(&connection),
            command,
            ResourceLimits::unrestricted(),
            None,
            move |_, _| {
                let mut command = tokio::process::Command::new("/bin/sh");
                command
                    .args([
                        "-c",
                        "while [ ! -f \"$1\" ]; do sleep 0.01; done; exit 0",
                        "fixture",
                    ])
                    .arg(path);
                Ok(command)
            },
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let started = connection
                    .retained_children
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .iter()
                    .any(|resource| {
                        matches!(
                            resource,
                            super::super::RetainedSshResource::RemoteOperation(_)
                        )
                    });
                if started {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        cancel.cancel();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut observer)
                .await
                .is_err()
        );
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 0);
        std::fs::write(directory.path().join("release"), b"release")?;
        assert!(
            tokio::time::timeout(Duration::from_secs(2), observer)
                .await??
                .is_err()
        );
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(
            connection
                .active_operations
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod deadline_tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn wrapper_regression_fractional_timeout_is_not_rounded_down() {
        let mut command = SandboxCommand::shell("true");
        command.timeout = Duration::from_millis(1500);
        assert_eq!(timeout_secs(&command, &ResourceLimits::unrestricted()), 2);
        let mut limits = ResourceLimits::unrestricted();
        limits.cpu_time_secs = Some(1);
        assert_eq!(timeout_secs(&command, &limits), 1);
    }

    fn timer_is_running(pid: u32) -> std::io::Result<bool> {
        Command::new("/bin/sh")
            .args(["-c", "kill -0 \"$1\" 2>/dev/null", "owned-timer"])
            .arg(pid.to_string())
            .status()
            .map(|status| status.success())
    }

    #[tokio::test]
    async fn wrapper_regression_reaps_timer_without_waiting_for_its_wake()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir()?;
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin)?;
        let ready = directory.path().join("timer-ready");
        let gate_path = directory.path().join("timer-gate");
        let sleep = bin.join("sleep");
        // 在原外部sleep边界控制唤醒，避免把CPU负载或1秒时钟精度当判据。
        // parent先完成payload；timer若没被真正回收，会持续占有原输出管道。
        std::fs::write(
            &sleep,
            r#"#!/bin/sh
printf '%s\n' "$$" > "$EKO_TEST_TIMER_READY"
IFS= read -r ignored < "$EKO_TEST_TIMER_GATE"
"#,
        )?;
        std::fs::set_permissions(&sleep, std::fs::Permissions::from_mode(0o755))?;
        assert!(Command::new("mkfifo").arg(&gate_path).status()?.success());
        let mut gate = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&gate_path)?;
        let ready_path = shell_quote(ready.to_str().ok_or("invalid fixture path")?)?;
        let payload = format!(
            "eko_probe_round=0; while [ ! -s {ready_path} ]; do eko_probe_round=$((eko_probe_round + 1)); [ \"$eko_probe_round\" -lt 1000 ] || exit 91; /bin/sleep 0.01; done; printf payload_stdout; printf payload_stderr >&2"
        );
        let script = deadline_script(&payload, 30)?;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &script])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("TMPDIR", directory.path())
            .env("EKO_TEST_TIMER_READY", &ready)
            .env("EKO_TEST_TIMER_GATE", &gate_path);
        let mut physical = tokio::task::spawn_blocking(move || command.output());
        let observed = tokio::time::timeout(Duration::from_secs(5), &mut physical).await;
        let returned_before_wake = observed.is_ok();
        let timer_pid = std::fs::read_to_string(&ready)
            .and_then(|pid| pid.trim().parse::<u32>().map_err(std::io::Error::other));
        let timer_still_running = match &timer_pid {
            Ok(pid) => timer_is_running(*pid),
            Err(error) => Err(std::io::Error::other(error.to_string())),
        };
        // 不管观察是否超时，先释放原夹具并收集输出；不能丢弃物理线程。
        gate.write_all(b"release\n")?;
        let output = match observed {
            Ok(result) => result??,
            Err(_) => physical.await??,
        };
        let timer_pid = timer_pid?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while timer_is_running(timer_pid)? {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<(), std::io::Error>(())
        })
        .await??;
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout)?, "payload_stdout");
        assert_eq!(String::from_utf8(output.stderr)?, "payload_stderr");
        assert!(
            returned_before_wake,
            "completed payload must not wait for the timer's scheduled wake"
        );
        assert!(
            !timer_still_running?,
            "redirecting timer pipes does not replace stopping and reaping it"
        );
        Ok(())
    }

    fn run_wrapped(script: &str, seconds: u64) -> std::io::Result<std::process::Output> {
        let wrapped = deadline_script(script, seconds).map_err(std::io::Error::other)?;
        Command::new("/bin/sh").arg("-c").arg(wrapped).output()
    }

    /// 远端看门狗必须在到点时真正结束负载，而不是等它自然跑完。
    #[test]
    fn watchdog_terminates_payload_at_the_deadline()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let started = std::time::Instant::now();
        let output = run_wrapped("exec sleep 30", 1)?;
        assert_eq!(output.status.code(), Some(124), "deadline must map to 124");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "watchdog must not wait for the payload to finish naturally"
        );
        Ok(())
    }

    /// 远端看门狗的期限必须来自这条命令自己的 timeout，而不是任何一个
    /// 硬编码默认值：后台 cell 会把 cell 剩余期限写进 `command.timeout`，
    /// 若这里退回 30 秒默认值，长构建会被提前杀掉。
    #[test]
    fn deadline_follows_the_command_timeout_not_a_default() {
        let mut command = SandboxCommand::shell("true");
        command.timeout = Duration::from_secs(300);
        assert_eq!(timeout_secs(&command, &ResourceLimits::unrestricted()), 300);

        let mut limited = ResourceLimits::unrestricted();
        limited.cpu_time_secs = Some(5);
        assert_eq!(timeout_secs(&command, &limited), 5, "更紧的上限生效");

        // 零期限也必须至少给 1 秒，避免看门狗立即杀进程把命令当失败。
        command.timeout = Duration::from_secs(0);
        assert_eq!(timeout_secs(&command, &ResourceLimits::unrestricted()), 1);
    }

    /// 正常结束的命令必须原样保留退出码与输出，包装不改变语义。
    #[test]
    fn watchdog_preserves_exit_code_and_output_for_fast_payloads()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let output = run_wrapped("printf 'hello'; exit 7", 30)?;
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "hello");
        Ok(())
    }

    /// 负载比期限短时看门狗要自行退出，不能把孤儿 sleep 留到超时。
    #[test]
    fn watchdog_exits_early_when_payload_finishes()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let started = std::time::Instant::now();
        let _output = run_wrapped("true", 30)?;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "wrapper must return as soon as the payload exits"
        );
        Ok(())
    }
}
