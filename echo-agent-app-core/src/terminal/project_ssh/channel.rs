//! The original SFTP child/client/pipe resources, shared by file operations
//! and streaming readers. There is one connection owner and launch path.

use super::sftp::{remote_failure, sftp_error, unavailable};
use super::{ProjectSshConnection, RemoteOperationResources};
use echo_agent::error::Result;
use openssh_sftp_client::{Sftp, SftpAuxiliaryData, SftpOptions};
use std::ops::Deref;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};

pub(super) struct SftpChannel {
    client: Arc<Sftp>,
    child: tokio::process::Child,
    stderr_reader: tokio::task::JoinHandle<std::io::Result<()>>,
    resources: Arc<RemoteOperationResources>,
}

impl SftpChannel {
    pub(super) async fn open(
        connection: Arc<ProjectSshConnection>,
        registration: Arc<OnceLock<Arc<RemoteOperationResources>>>,
    ) -> Result<Self> {
        let operation = connection.operation().map_err(unavailable)?;
        let target = connection.effective_target().map_err(unavailable)?.clone();
        let mut command = connection.slave_command().map_err(unavailable)?;
        command.arg("-s");
        connection
            .add_destination(&mut command)
            .map_err(unavailable)?;
        command.arg("sftp");
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                unavailable(format!("cannot start system SSH SFTP channel: {error}"))
            })?;
        Self::from_spawned(connection, registration, child, operation, target).await
    }

    pub(super) async fn from_spawned(
        connection: Arc<ProjectSshConnection>,
        registration: Arc<OnceLock<Arc<RemoteOperationResources>>>,
        mut child: tokio::process::Child,
        operation: super::ProjectSshOperation,
        target: crate::terminal::TerminalTarget,
    ) -> Result<Self> {
        let resources = registration
            .get_or_init(|| {
                connection.retain_remote_operation(&child, target, operation, Vec::new())
            })
            .clone();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stderr_reader = tokio::spawn(async move {
            if let Some(mut stderr) = stderr {
                tokio::io::copy(&mut stderr, &mut tokio::io::sink())
                    .await
                    .map(|_| ())
            } else {
                Ok(())
            }
        });
        let (stdin, stdout) = match (stdin, stdout) {
            (Some(stdin), Some(stdout)) => (stdin, stdout),
            (stdin, stdout) => {
                drop(stdin);
                if let Some(mut stdout) = stdout {
                    let _drained = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
                }
                let _settled = settle_child(child, stderr_reader, false).await;
                // No standard file request was sent before pipe preparation.
                resources.complete_with_proof();
                return Err(unavailable(
                    "SSH SFTP pipe preparation failed before any file request",
                ));
            }
        };
        match Sftp::new_with_auxiliary(
            stdin,
            stdout,
            SftpOptions::default(),
            SftpAuxiliaryData::Arced(connection),
        )
        .await
        {
            Ok(client) => Ok(Self {
                client: Arc::new(client),
                child,
                stderr_reader,
                resources,
            }),
            Err(error) => {
                let original = sftp_error(error, false);
                let settlement = settle_child(child, stderr_reader, false).await;
                // Failed initialization cannot have sent a file mutation.
                resources.complete_with_proof();
                match settlement {
                    Ok(()) => Err(original),
                    Err(error) => Err(error),
                }
            }
        }
    }

    pub(super) fn client(&self) -> Arc<SftpClient> {
        Arc::new(SftpClient {
            client: Arc::clone(&self.client),
            resources: Arc::clone(&self.resources),
        })
    }

    /// 车道可以安全复用：没有其它持有者仍握着客户端克隆，因此
    /// `finish()` 的独占结算证据不会被池化破坏。
    pub(super) fn can_reuse(&self) -> bool {
        Arc::strong_count(&self.client) == 1
    }

    /// 这条通道是否带有未确认的远端副作用负债。带负债的车道必须丢弃，
    /// 不能继续服务后续请求。
    pub(super) fn has_unknown_debt(&self) -> bool {
        self.resources.stop_is_unknown()
    }

    pub(super) async fn finish(self, mutating: bool) -> Result<()> {
        let Self {
            client,
            child,
            stderr_reader,
            resources,
        } = self;
        let closed = match Arc::try_unwrap(client) {
            Ok(client) => client
                .close()
                .await
                .map_err(|error| sftp_error(error, mutating)),
            Err(_) => {
                if mutating {
                    resources.mark_stop_unknown(
                        "a mutating SFTP client still owns the original channel",
                    );
                }
                Err(remote_failure(
                    "SFTP operation retained an unsettled channel",
                    mutating,
                ))
            }
        };
        let settled = settle_child(child, stderr_reader, mutating).await;
        // close() actually awaits responses for buffered/sent requests. A
        // lost mutation response without that proof cannot become a terminal
        // ordinary error just because the local subsystem child reached EOF.
        if mutating && resources.stop_is_unknown() && closed.is_err() {
            resources.mark_stop_unknown("standard SFTP mutation response/close proof was lost");
            // 该负债没有任何其它 settle 来源，必须是有界观察：超时保留
            // unknown 并继续返回失败，不能挂起调用工具线程。
            if !super::wait_for_unknown_settlement(
                &resources,
                super::super::CLOSE_OBSERVATION_TIMEOUT,
            )
            .await
            {
                tracing::warn!(
                    "project SSH SFTP mutation settlement observation timed out; unknown debt retained"
                );
            }
        } else {
            // Successful request/close acknowledgement, or an operation with
            // no outstanding mutation, is different from unknown file contents.
            resources.complete_with_proof();
        }
        match (closed, settled) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }
}

async fn settle_child(
    mut child: tokio::process::Child,
    stderr_reader: tokio::task::JoinHandle<std::io::Result<()>>,
    mutating: bool,
) -> Result<()> {
    // The existing synchronous caller retains invocation guards until this
    // physical future returns. Retry only the same local wait, never a request.
    let mut wait_error = None;
    let status = loop {
        match child.wait().await {
            Ok(status) => break status,
            Err(error) => {
                if wait_error.is_none() {
                    wait_error = Some(error.to_string());
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
    };
    let drained = stderr_reader.await;
    if let Some(error) = wait_error {
        return Err(remote_failure(
            format!(
                "original SSH SFTP child settled only after a failed wait observation: {error}; verify any writes before retry"
            ),
            mutating,
        ));
    }
    if !matches!(drained, Ok(Ok(()))) {
        return Err(remote_failure(
            "SSH SFTP diagnostic reader did not settle",
            mutating,
        ));
    }
    if !status.success() {
        return Err(remote_failure(
            format!(
                "SSH SFTP channel ended with {status}; verify any already-sent writes before retry"
            ),
            mutating,
        ));
    }
    Ok(())
}

// Thin standard-client adapter. This preserves native response knowledge before
// mapping errors into the existing ToolFailure contract; it implements no wire
// protocol and has no resource store or execution identity of its own.
pub(super) struct SftpClient {
    client: Arc<Sftp>,
    resources: Arc<RemoteOperationResources>,
}

impl Deref for SftpClient {
    type Target = Sftp;
    fn deref(&self) -> &Self::Target {
        self.client.as_ref()
    }
}

impl SftpClient {
    pub(super) fn note_error(&self, error: &openssh_sftp_client::Error, mutating: bool) {
        note_request_error(&self.resources, error, mutating);
    }

    pub(super) fn error(
        &self,
        error: openssh_sftp_client::Error,
        mutating: bool,
    ) -> echo_agent::error::ReactError {
        self.error_with_effects(error, mutating, mutating)
    }

    // A readonly request can fail after earlier mkdir/delete acknowledgements.
    // Its own missing response is not a write-stop debt, while the containing
    // operation must still preserve its already-applied partial effects.
    pub(super) fn error_with_effects(
        &self,
        error: openssh_sftp_client::Error,
        request_mutating: bool,
        effects_possible: bool,
    ) -> echo_agent::error::ReactError {
        request_error(&self.resources, error, request_mutating, effects_possible)
    }
}

fn request_error(
    resources: &RemoteOperationResources,
    error: openssh_sftp_client::Error,
    request_mutating: bool,
    effects_possible: bool,
) -> echo_agent::error::ReactError {
    note_request_error(resources, &error, request_mutating);
    sftp_error(error, request_mutating || effects_possible)
}

fn note_request_error(
    resources: &RemoteOperationResources,
    error: &openssh_sftp_client::Error,
    request_mutating: bool,
) {
    if request_mutating && response_was_lost(error) {
        resources.mark_stop_unknown("standard SFTP mutation did not return its response");
    }
}

fn response_was_lost(error: &openssh_sftp_client::Error) -> bool {
    use openssh_sftp_client::Error;
    match error {
        // These are actual server replies or errors before a request could
        // be sent. Possible effects do not mean the request is still running.
        Error::SftpError(_, _) | Error::BufferTooLong(_) | Error::UnsupportedExtension(_) => false,
        Error::RecursiveErrors(errors) => {
            response_was_lost(&errors.original_error) || response_was_lost(&errors.occuring_error)
        }
        Error::RecursiveErrors3(errors) => {
            response_was_lost(&errors.err1)
                || response_was_lost(&errors.err2)
                || response_was_lost(&errors.err3)
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_standard_response_is_different_from_a_presend_capability_error() {
        let lost = openssh_sftp_client::Error::IOError(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "fixture response was lost",
        ));
        let capability = openssh_sftp_client::Error::UnsupportedExtension(&"fixture-extension");
        assert!(response_was_lost(&lost));
        assert!(!response_was_lost(&capability));
    }

    #[cfg(unix)]
    async fn completed_local_fixture()
    -> Result<(Arc<RemoteOperationResources>, Arc<ProjectSshConnection>)> {
        let service = crate::terminal::TerminalService::new();
        let target = crate::terminal::TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let connection = ProjectSshConnection::prepare(&service, "effects-fixture", target.clone())
            .map_err(unavailable)?;
        connection
            .effective_target
            .set(target.clone())
            .map_err(|_| unavailable("duplicate fixture target"))?;
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()?;
        let resources = connection.retain_remote_operation(
            &child,
            target,
            connection.operation().map_err(unavailable)?,
            Vec::new(),
        );
        if !child.wait().await?.success() {
            return Err(unavailable("local fixture did not complete"));
        }
        Ok((resources, connection))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn readonly_failure_after_completed_mutation_keeps_possible_effects_without_stop_debt()
    -> Result<()> {
        use echo_agent::tools::{
            ToolFailure, ToolFailureCategory, ToolRecoveryAction, ToolSideEffect,
        };
        let (resources, connection) = completed_local_fixture().await?;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("completed-delete");
        std::fs::write(&path, b"self-owned fixture")?;
        let mutation_completed = std::fs::remove_file(&path).map(|_| true)?;
        let read_error = std::fs::read(&path)
            .err()
            .ok_or_else(|| unavailable("fixture read unexpectedly succeeded"))?;
        // The production adapter preserves two facts: this request is readonly,
        // while the containing operation already completed an earlier mutation.
        // This is a controlled local error/owner fixture, not SFTP acceptance.
        let error = request_error(
            &resources,
            openssh_sftp_client::Error::IOError(read_error),
            false,
            mutation_completed,
        );
        let failure = ToolFailure::from_error(&error, false);
        assert_eq!(failure.category, ToolFailureCategory::PartialSideEffect);
        assert_eq!(failure.side_effect, ToolSideEffect::Possible);
        assert_eq!(failure.recovery, ToolRecoveryAction::VerifyThenRetry);
        assert!(!failure.allows_automatic_retry());
        assert!(!resources.stop_is_unknown());
        // Only the actual self-owned local child's completion is test cleanup
        // proof; no remote debt-clearing capability is exposed by this fixture.
        resources.complete_with_proof();
        connection.observe_idle().await.map_err(unavailable)?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn readonly_failure_without_any_mutation_has_no_effect_or_stop_debt() -> Result<()> {
        use echo_agent::tools::{ToolFailure, ToolRecoveryAction, ToolSideEffect};
        let (resources, connection) = completed_local_fixture().await?;
        let directory = tempfile::tempdir()?;
        let read_error = std::fs::read(directory.path().join("never-created"))
            .err()
            .ok_or_else(|| unavailable("fixture read unexpectedly succeeded"))?;
        let error = request_error(
            &resources,
            openssh_sftp_client::Error::IOError(read_error),
            false,
            false,
        );
        let failure = ToolFailure::from_error(&error, false);
        assert_eq!(failure.side_effect, ToolSideEffect::None);
        assert_eq!(failure.recovery, ToolRecoveryAction::RestoreThenRetry);
        assert!(!resources.stop_is_unknown());
        resources.complete_with_proof();
        connection.observe_idle().await.map_err(unavailable)?;
        Ok(())
    }

    /// 该负债没有其它 settle 来源：无界等待会让调用工具线程永久挂起。
    /// 这里用自有本地夹具进程证明等待是有界的、并且超时后负债仍然保留。
    #[cfg(unix)]
    #[tokio::test]
    async fn unknown_settlement_wait_is_bounded_and_keeps_the_debt() -> Result<()> {
        let service = crate::terminal::TerminalService::new();
        let target = crate::terminal::TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let connection = ProjectSshConnection::prepare(&service, "bounded-fixture", target.clone())
            .map_err(unavailable)?;
        connection
            .effective_target
            .set(target.clone())
            .map_err(|_| unavailable("duplicate fixture target"))?;
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .spawn()?;
        let operation = connection.operation().map_err(unavailable)?;
        let resources = connection.retain_remote_operation(&child, target, operation, Vec::new());
        resources.mark_stop_unknown("bounded-settlement fixture");

        let started = std::time::Instant::now();
        let settled = super::super::wait_for_unknown_settlement(
            &resources,
            std::time::Duration::from_millis(50),
        )
        .await;
        assert!(
            !settled,
            "an unsettled remote operation must not report settlement"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "unknown settlement observation must be bounded"
        );
        // 超时不等于清债：原负债仍必须保留，供关闭/重试继续拒绝。
        assert!(!resources.is_settled());
        assert!(resources.stop_is_unknown());

        // 仅清理本测试自有的本地夹具进程，不是远端停止证据。
        resources.complete_with_proof();
        child.start_kill()?;
        let _reaped = child.wait().await?;
        Ok(())
    }
}
