//! The existing SandboxExecutor contract over the owned OpenSSH connection.

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

pub struct ProjectSshSandbox {
    connection: Arc<ProjectSshConnection>,
}

impl ProjectSshSandbox {
    pub(super) fn new(connection: Arc<ProjectSshConnection>) -> Self {
        Self { connection }
    }

    async fn execute_owned(
        connection: Arc<ProjectSshConnection>,
        command: SandboxCommand,
        limits: ResourceLimits,
        cancel: Option<Arc<CancellationToken>>,
    ) -> Result<ExecutionResult> {
        Self::execute_with_launcher(connection, command, limits, cancel, |connection, script| {
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
        // 远端截止时间：由目标机器自己看门狗终止负载，而不是本机只观察。
        // 这样 `timeout` 到点时命令真的会停，而不是等它自然结束（可能是几十分钟）。
        let script = deadline_script(&command_script(&command)?, timeout_secs(&command, &limits))?;
        let target = connection.effective_target().map_err(unavailable)?.clone();
        let lease = connection.operation().map_err(unavailable)?;
        let timeout = limits
            .cpu_time_secs
            .map(Duration::from_secs)
            .map(|limit| limit.min(command.timeout))
            .unwrap_or(command.timeout);
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
            let stderr = tokio::spawn(async move {
                match stderr {
                    Some(stderr) => read_bounded(stderr, output_limit).await,
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
            let exit_code = match status.code() {
                Some(code) if code != 255 => code,
                _ => {
                    // Native OpenSSH cannot recover the lost remote exit
                    // receipt. Local EOF, master check or reconnect is not a
                    // stop proof for this original command. Keep this same
                    // invocation on original-owner progress, not a fake task.
                    resources.mark_stop_unknown(
                        "remote exit receipt was lost (SSH255 or no exit status)",
                    );
                    resources.wait_for_settlement().await;
                    return Err(remote_failure(
                        "original remote execution stop was unknown; verify its effects before retry",
                        true,
                    ));
                }
            };
            // A real non255 exit receipt proves command completion even when
            // its contents/effects or a subsequent pipe observation failed.
            // Do not make every Possible side effect a permanent stop debt.
            resources.complete_with_proof();
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
            let (stderr, stderr_bytes, stderr_truncated) = stderr
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
        let mut cancellation_requested = false;
        let mut timeout_observed = false;
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
                                resources.wait_for_settlement().await;
                            }
                            return Err(remote_failure(format!("SSH physical result observation failed: {error}"), true));
                        }
                    };
                    let result = result?;
                    // 远端看门狗用 124 表示"到点并已实际终止负载"：这是有证据的
                    // 超时，不是"本机不看了"。取消仍然只观察（真实终止在后续阶段）。
                    if timeout_observed && !cancellation_requested && result.exit_code == 124 {
                        let mut timed_out = result;
                        timed_out.timed_out = true;
                        return Ok(timed_out);
                    }
                    if cancellation_requested || timeout_observed {
                        return Err(remote_failure(format!("{} was requested; original remote command subsequently completed with exit {}. Verify effects before retry", if cancellation_requested { "Cancellation" } else { "Timeout" }, result.exit_code), true));
                    }
                    return Ok(result);
                },
                _ = &mut cancellation, if !cancellation_requested => cancellation_requested = true,
                _ = &mut deadline, if !timeout_observed => timeout_observed = true,
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

    fn is_available(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move { self.connection.check_authenticated().await.is_ok() })
    }

    fn execute(&self, command: SandboxCommand) -> BoxFuture<'_, Result<ExecutionResult>> {
        Box::pin(Self::execute_owned(
            Arc::clone(&self.connection),
            command,
            ResourceLimits::unrestricted(),
            None,
        ))
    }

    fn execute_with_limits(
        &self,
        command: SandboxCommand,
        limits: ResourceLimits,
    ) -> BoxFuture<'_, Result<ExecutionResult>> {
        Box::pin(Self::execute_owned(
            Arc::clone(&self.connection),
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
        Box::pin(Self::execute_owned(
            Arc::clone(&self.connection),
            command,
            limits,
            cancel,
        ))
    }

    fn cleanup(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.connection
                .retry_retained_children()
                .await
                .map_err(unavailable)?;
            if self
                .connection
                .active_operations
                .load(std::sync::atomic::Ordering::Acquire)
                != 0
            {
                return Err(unavailable(self.connection.unsettled_remote_operation().unwrap_or_else(|| {
                    "original SSH operations remain active; cleanup has no remote completion proof".to_string()
                })));
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
/// 保证"本机判定超时"和"远端真的开始终止"是同一时刻，而不是两套口径。
fn timeout_secs(command: &SandboxCommand, limits: &ResourceLimits) -> u64 {
    let timeout = limits
        .cpu_time_secs
        .map(Duration::from_secs)
        .map(|limit| limit.min(command.timeout))
        .unwrap_or(command.timeout);
    u64::try_from(timeout.as_secs()).unwrap_or(u64::MAX).max(1)
}

/// 用 POSIX `sh` 包一层看门狗，让目标机器在到点时真正终止负载。
///
/// 负载本身仍是原来的 `cd ... || exit 125; exec ...` 脚本，因此 shell 语义、
/// 环境变量和退出码都不变；只有"到点后由谁负责结束"从本机观察变成远端执行。
/// 终止顺序是先 `TERM` 再宽限 `KILL`；是否真的走到终止用标记文件回报，
/// 命中时统一以 124 退出（与 `timeout(1)` 的约定一致），由本地映射成超时结果。
fn deadline_script(script: &str, timeout_seconds: u64) -> Result<String> {
    let payload = shell_quote(script).map_err(unavailable)?;
    // 看门狗自己轮询，负载提前结束就立刻退出，避免每次命令都在远端留一个
    // 睡到超时的孤儿 sleep。标记文件按会话派生，先删再跑，防止上次异常退出
    // 留下的残留被误读成"本次已到点"。
    Ok(format!(
        "eko_marker=\"${{TMPDIR:-/tmp}}/.eko-deadline-$$\"\n\
         rm -f \"$eko_marker\"\n\
         sh -c {payload} &\n\
         eko_child=$!\n\
         ( eko_elapsed=0\n\
           while [ \"$eko_elapsed\" -lt {timeout_seconds} ]; do\n\
             sleep 1\n\
             eko_elapsed=$((eko_elapsed + 1))\n\
             kill -0 \"$eko_child\" 2>/dev/null || exit 0\n\
           done\n\
           : > \"$eko_marker\"\n\
           kill -TERM \"$eko_child\" 2>/dev/null\n\
           sleep 5\n\
           kill -KILL \"$eko_child\" 2>/dev/null ) &\n\
         eko_watchdog=$!\n\
         wait \"$eko_child\"\n\
         eko_status=$?\n\
         kill \"$eko_watchdog\" 2>/dev/null\n\
         if [ -f \"$eko_marker\" ]; then rm -f \"$eko_marker\"; exit 124; fi\n\
         rm -f \"$eko_marker\"\n\
         exit $eko_status"
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
        service
            .project_connections
            .insert("fixture".into(), Arc::clone(&connection));
        Ok((service, connection))
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
        struct ReleaseLocalFixture(std::path::PathBuf);
        impl Drop for ReleaseLocalFixture {
            fn drop(&mut self) {
                let _release = std::fs::write(&self.0, b"release");
            }
        }
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

    fn run_wrapped(script: &str, seconds: u64) -> std::io::Result<std::process::Output> {
        let wrapped = deadline_script(script, seconds).map_err(std::io::Error::other)?;
        Command::new("/bin/sh").arg("-c").arg(wrapped).output()
    }

    /// 远端看门狗必须在到点时真正结束负载，而不是等它自然跑完。
    #[test]
    fn watchdog_terminates_payload_at_the_deadline()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let started = std::time::Instant::now();
        let output = run_wrapped("sleep 30", 1)?;
        assert_eq!(output.status.code(), Some(124), "deadline must map to 124");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "watchdog must not wait for the payload to finish naturally"
        );
        Ok(())
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
