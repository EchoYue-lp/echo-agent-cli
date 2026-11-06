//! Project SSH resources attached to the original TerminalService owner.
//!
//! The authentication PTY is a foreground OpenSSH master. Normal terminals,
//! structured commands and SFTP are slaves of its private control socket. A
//! missing master must fail, including the check/use race: ProxyCommand=false
//! prevents OpenSSH's usual fallback to a fresh connection.

use super::{TerminalService, TerminalSessionInfo, TerminalTarget};
use echo_agent::sandbox::{SandboxCommand, SandboxExecutor};
use echo_agent::tools::InvocationResourceGuard;
use portable_pty::CommandBuilder;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as ResourceMutex, OnceLock, Weak};
use tempfile::TempDir;
use tokio::sync::Notify;

struct RetainedSshChild {
    child: tokio::process::Child,
    _operation: ProjectSshOperation,
    _resource_guards: Vec<InvocationResourceGuard>,
    readers: Vec<tokio::task::JoinHandle<Result<(), String>>>,
    reader_errors: Vec<String>,
}

#[derive(Clone)]
enum RetainedSshResource {
    Child(Arc<tokio::sync::Mutex<RetainedSshChild>>),
    RemoteOperation(Arc<RemoteOperationResources>),
}

struct RemoteOperationState {
    resources: Option<(ProjectSshOperation, Vec<InvocationResourceGuard>)>,
    unknown_reason: Option<&'static str>,
}

// This is a resource projection in the original connection's retained_children
// container. It is registered synchronously after the actual child is spawned,
// before an await can lose the observer. Only an actual completion proof may
// release it; dropping a physical closure/JoinHandle does not release guards.
pub(super) struct RemoteOperationResources {
    connection: Weak<ProjectSshConnection>,
    target: TerminalTarget,
    child_id: Option<u32>,
    state: ResourceMutex<RemoteOperationState>,
    progress: Notify,
}

impl RemoteOperationResources {
    fn notify_progress(&self) {
        self.progress.notify_waiters();
        if let Some(connection) = self.connection.upgrade() {
            connection.operations_changed.notify_waiters();
        }
    }

    pub(super) fn mark_stop_unknown(&self, reason: &'static str) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.resources.is_some() {
            state.unknown_reason = Some(reason);
        }
        drop(state);
        self.notify_progress();
    }

    pub(super) fn stop_is_unknown(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .unknown_reason
            .is_some()
    }

    fn is_settled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .resources
            .is_none()
    }

    // Call only after the original command exit receipt, or the standard
    // SFTP acknowledgement/close proof, and required local resource joins.
    pub(super) fn complete_with_proof(&self) {
        let resources = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.unknown_reason = None;
            state.resources.take()
        };
        drop(resources);
        if let Some(connection) = self.connection.upgrade() {
            connection
                .retained_children
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retain(|resource| match resource {
                    RetainedSshResource::RemoteOperation(candidate) => {
                        !std::ptr::eq(candidate.as_ref(), self)
                    }
                    RetainedSshResource::Child(_) => true,
                });
        }
        self.notify_progress();
    }

    pub(super) async fn wait_for_settlement(&self) {
        loop {
            let progress = self.progress.notified();
            if self.is_settled() {
                return;
            }
            progress.await;
        }
    }
}

/// 有界等待一次原物理操作的结算。
///
/// `retry_retained_children` 明确跳过 `RemoteOperation`，所以除了该操作
/// 自己的 `complete_with_proof` 没有任何路径会发布 settle 通知。对这条
/// 负债无界等待会让调用工具线程永久挂起；超时只能返回“尚未结算”，
/// 由调用者保留 unknown 负债并返回失败，不能假装已经结算或清债。
pub(super) async fn wait_for_unknown_settlement(
    resources: &RemoteOperationResources,
    timeout: std::time::Duration,
) -> bool {
    matches!(
        tokio::time::timeout(timeout, resources.wait_for_settlement()).await,
        Ok(())
    )
}

mod cas;
mod channel;
mod effective;
mod lanes;
mod reader;
mod remote_search;
mod sandbox;
mod sftp;
pub use remote_search::RemoteContentSearch;
pub use sandbox::ProjectSshSandbox;
pub use sftp::ProjectSshFileSystem;

pub struct ProjectSshConnection {
    target: TerminalTarget,
    // A resolved system-SSH fact for this exact connection, not another
    // Workspace/configuration authority. It is immutable after preparation.
    effective_target: OnceLock<TerminalTarget>,
    master_terminal_id: String,
    control_directory: TempDir,
    service: Weak<TerminalService>,
    closing: AtomicBool,
    active_operations: AtomicUsize,
    operations_changed: Notify,
    // Original process handles whose wait failed stay with this same owner;
    // retrying project close observes them rather than inventing settled state.
    retained_children: ResourceMutex<Vec<RetainedSshResource>>,
    // 连接级 SFTP 车道：按需创建，随本连接一起关闭。弱引用关系使
    // 车道不反过来持有连接。
    sftp_lanes: OnceLock<Arc<lanes::SftpLanes>>,
}

pub(super) struct ProjectTerminalLaunch {
    pub command: CommandBuilder,
    pub connection: Arc<ProjectSshConnection>,
    pub operation: Option<ProjectSshOperation>,
}

pub(super) struct ProjectSshOperation {
    connection: Arc<ProjectSshConnection>,
}

impl Drop for ProjectSshOperation {
    fn drop(&mut self) {
        self.connection
            .active_operations
            .fetch_sub(1, Ordering::AcqRel);
        self.connection.operations_changed.notify_waiters();
    }
}

impl ProjectSshConnection {
    fn prepare(
        service: &Arc<TerminalService>,
        owner_id: &str,
        target: TerminalTarget,
    ) -> Result<Arc<Self>, String> {
        let target = target.normalized()?;
        if !matches!(target, TerminalTarget::Ssh { .. }) {
            return Err("a project SSH connection requires an SSH target".to_string());
        }
        if !cfg!(unix) {
            return Err(
                "project SSH multiplexing is not qualified on this platform; no local fallback"
                    .to_string(),
            );
        }
        // A short, private directory avoids AF_UNIX path limits under macOS's
        // long per-user TMPDIR. tempfile creates it with owner-only permissions.
        let control_directory = tempfile::Builder::new()
            .prefix("eko-ssh-")
            .tempdir_in("/tmp")
            .map_err(|error| format!("cannot prepare private SSH control directory: {error}"))?;
        Ok(Arc::new(Self {
            target,
            effective_target: OnceLock::new(),
            master_terminal_id: TerminalService::project_ssh_auth_terminal_id(owner_id),
            control_directory,
            service: Arc::downgrade(service),
            closing: AtomicBool::new(false),
            active_operations: AtomicUsize::new(0),
            operations_changed: Notify::new(),
            retained_children: ResourceMutex::new(Vec::new()),
            sftp_lanes: OnceLock::new(),
        }))
    }

    /// Opaque identity of this physical OpenSSH owner. It cannot be rebuilt
    /// from the workspace key and changes when the connection is replaced.
    pub fn generation(&self) -> String {
        use sha2::{Digest as _, Sha256};
        hex::encode(Sha256::digest(
            self.control_directory.path().as_os_str().as_encoded_bytes(),
        ))
    }

    pub fn target(&self) -> &TerminalTarget {
        &self.target
    }

    pub fn effective_target(&self) -> Result<&TerminalTarget, String> {
        self.effective_target
            .get()
            .ok_or_else(|| "project SSH effective target has not been resolved".to_string())
    }

    pub fn verify_effective_target(&self, expected: &TerminalTarget) -> Result<(), String> {
        if self.effective_target()? != expected {
            return Err("resolved SSH host/account/port changed; reconnect requires an explicit project location decision".to_string());
        }
        Ok(())
    }

    pub(super) fn master_terminal_id(&self) -> &str {
        &self.master_terminal_id
    }

    fn control_path(&self) -> PathBuf {
        self.control_directory.path().join("control")
    }

    fn arguments(&self, master: bool) -> Result<Vec<String>, String> {
        let mut args = vec![
            "-S".to_string(),
            self.control_path().to_string_lossy().into_owned(),
            "-o".to_string(),
            format!("ControlMaster={}", if master { "yes" } else { "no" }),
            "-o".to_string(),
            "ControlPersist=no".to_string(),
        ];
        if !master {
            args.extend([
                "-o".to_string(),
                "ProxyCommand=false".to_string(),
                "-o".to_string(),
                "BatchMode=yes".to_string(),
            ]);
        }
        let TerminalTarget::Ssh {
            host,
            user: Some(user),
            port: Some(port),
        } = self.effective_target()?
        else {
            return Err("resolved SSH target is incomplete".to_string());
        };
        // Keep the user's raw alias as destination, so its IdentityFile,
        // ProxyJump, agent and host-key configuration remain in effect. Pin
        // resolved endpoint values on argv (which wins over later config), and
        // do not canonicalize them again after the system resolution snapshot.
        args.extend([
            "-o".to_string(),
            // System -G already expanded tokens. Escape literal '%' (for
            // example an IPv6 zone) before HostName expands it a second time.
            format!("HostName={}", host.replace('%', "%%")),
            "-l".to_string(),
            user.clone(),
            "-p".to_string(),
            port.to_string(),
            "-o".to_string(),
            "CanonicalizeHostname=no".to_string(),
        ]);
        Ok(args)
    }

    fn destination(&self) -> Result<&str, String> {
        match &self.target {
            TerminalTarget::Ssh { host, .. } => Ok(host),
            TerminalTarget::Local => Err("project SSH target is not remote".to_string()),
        }
    }

    fn master_command(&self) -> Result<CommandBuilder, String> {
        let mut command = CommandBuilder::new("ssh");
        command.args(self.arguments(true)?);
        command.args(["-N", "--", self.destination()?]);
        Ok(command)
    }

    pub(super) fn slave_command(&self) -> Result<tokio::process::Command, String> {
        let mut command = tokio::process::Command::new("ssh");
        command.args(self.arguments(false)?);
        // Never rely on kill-on-drop to claim a remote effect was stopped.
        command.kill_on_drop(false);
        Ok(command)
    }

    pub(super) fn add_destination(
        &self,
        command: &mut tokio::process::Command,
    ) -> Result<(), String> {
        command.arg("--").arg(self.destination()?);
        Ok(())
    }

    pub(super) fn operation(self: &Arc<Self>) -> Result<ProjectSshOperation, String> {
        if self.closing.load(Ordering::Acquire)
            || self
                .service
                .upgrade()
                .is_none_or(|service| service.shutting_down.load(Ordering::Acquire))
        {
            return Err("project SSH connection is closing; operation not started".to_string());
        }
        self.active_operations
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| "too many active project SSH operations".to_string())?;
        let operation = ProjectSshOperation {
            connection: Arc::clone(self),
        };
        if self.closing.load(Ordering::Acquire) {
            return Err("project SSH connection is closing; operation not started".to_string());
        }
        Ok(operation)
    }

    pub(super) fn begin_shutdown(&self) {
        self.closing.store(true, Ordering::Release);
    }

    pub(super) async fn retain_child(
        &self,
        child: tokio::process::Child,
        operation: ProjectSshOperation,
    ) {
        self.retain_child_with_resources(child, operation, Vec::new(), Vec::new())
            .await;
    }

    pub(super) async fn retain_child_with_resources(
        &self,
        child: tokio::process::Child,
        operation: ProjectSshOperation,
        resource_guards: Vec<InvocationResourceGuard>,
        readers: Vec<tokio::task::JoinHandle<Result<(), String>>>,
    ) {
        self.retained_children
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(RetainedSshResource::Child(Arc::new(
                tokio::sync::Mutex::new(RetainedSshChild {
                    child,
                    _operation: operation,
                    _resource_guards: resource_guards,
                    readers,
                    reader_errors: Vec::new(),
                }),
            )));
        self.operations_changed.notify_waiters();
    }

    pub(super) fn retain_remote_operation(
        self: &Arc<Self>,
        child: &tokio::process::Child,
        target: TerminalTarget,
        operation: ProjectSshOperation,
        resource_guards: Vec<InvocationResourceGuard>,
    ) -> Arc<RemoteOperationResources> {
        let resource = Arc::new(RemoteOperationResources {
            connection: Arc::downgrade(self),
            target,
            child_id: child.id(),
            state: ResourceMutex::new(RemoteOperationState {
                resources: Some((operation, resource_guards)),
                unknown_reason: None,
            }),
            progress: Notify::new(),
        });
        self.retained_children
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(RetainedSshResource::RemoteOperation(Arc::clone(&resource)));
        self.operations_changed.notify_waiters();
        resource
    }

    async fn retry_retained_children(&self) -> Result<(), String> {
        let retained = self
            .retained_children
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        for retained_resource in retained {
            let RetainedSshResource::Child(child) = &retained_resource else {
                // In particular, local EOF/check/reconnect cannot clear a
                // remote-stop obligation whose only exit receipt was lost.
                continue;
            };
            let mut resource = child.lock().await;
            match resource.child.try_wait() {
                Ok(Some(_)) => {
                    resource.child.wait().await.map_err(|error| {
                        format!("original SSH child wait still failed; owner retained: {error}")
                    })?;
                    let mut reader_index = 0;
                    while reader_index < resource.readers.len() {
                        let reader = resource
                            .readers
                            .get_mut(reader_index)
                            .ok_or_else(|| "retained SSH reader index changed".to_string())?;
                        if !reader.is_finished() {
                            reader_index = reader_index.saturating_add(1);
                            continue;
                        }
                        let result = (&mut *reader).await;
                        drop(resource.readers.swap_remove(reader_index));
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => resource.reader_errors.push(error),
                            Err(error) => resource
                                .reader_errors
                                .push(format!("original SSH pipe join failed: {error}")),
                        }
                    }
                    if !resource.reader_errors.is_empty() {
                        return Err(
                            "original SSH pipe failed; resources retained, not a confirmed close"
                                .to_string(),
                        );
                    }
                    if !resource.readers.is_empty() {
                        continue;
                    }
                    self.retained_children
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .retain(|candidate| match candidate {
                            RetainedSshResource::Child(candidate) => !Arc::ptr_eq(candidate, child),
                            RetainedSshResource::RemoteOperation(_) => true,
                        });
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(format!("original SSH child remains unobserved: {error}"));
                }
            }
        }
        self.retained_children
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|resource| match resource {
                RetainedSshResource::RemoteOperation(resource) => !resource.is_settled(),
                RetainedSshResource::Child(_) => true,
            });
        Ok(())
    }

    fn unsettled_remote_operation(&self) -> Option<String> {
        self.retained_children
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find_map(|resource| {
                let RetainedSshResource::RemoteOperation(resource) = resource else {
                    return None;
                };
                let state = resource.state.lock().unwrap_or_else(|error| error.into_inner());
                state.unknown_reason.map(|reason| {
                    format!("original SSH operation on {} (local child {:?}) remains unsettled: {reason}", resource.target.label(), resource.child_id)
                })
            })
    }

    async fn observe_idle(self: &Arc<Self>) -> Result<(), String> {
        // 车道是长寿命的常驻通道：不先关闭它们，`active_operations`
        // 永远不会归零，关闭观察必然超时。车道关闭本身等待在途操作归还。
        if let Some(lanes) = self.sftp_lanes.get() {
            lanes.shutdown().await.map_err(|error| error.to_string())?;
        }
        self.begin_shutdown();
        let idle = async {
            loop {
                let notified = self.operations_changed.notified();
                self.retry_retained_children().await?;
                if self.active_operations.load(Ordering::Acquire) == 0 {
                    return Ok::<(), String>(());
                }
                tokio::select! { _ = notified => {}, _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {} }
            }
        };
        tokio::time::timeout(super::CLOSE_OBSERVATION_TIMEOUT, idle)
            .await
            .map_err(|_| {
                self.unsettled_remote_operation().unwrap_or_else(|| {
                    "project SSH operations remain active or uncertain; original master retained"
                        .to_string()
                })
            })?
    }

    /// Natural `ssh -O check` success is authentication/transport evidence;
    /// neither a prompt nor the earlier local PTY spawn is such evidence.
    pub async fn check_authenticated(self: &Arc<Self>) -> Result<(), String> {
        if self.authentication_ready().await? {
            Ok(())
        } else {
            Err("SSH 登录尚未完成，请检查用户名和密码后重新测试连接".to_string())
        }
    }

    /// 登录测试只读取原 ControlMaster，不访问项目目录，也不会另建认证连接。
    pub async fn test_target_authenticated(
        self: &Arc<Self>,
        expected_target: TerminalTarget,
    ) -> Result<bool, String> {
        if self.target != expected_target.normalized()? {
            return Err("SSH 认证属于另一连接目标，请重新测试连接".to_string());
        }
        self.authentication_ready().await
    }

    async fn authentication_ready(self: &Arc<Self>) -> Result<bool, String> {
        let operation = self.operation()?;
        let connection = Arc::clone(self);
        tokio::spawn(async move {
            let mut command = connection.slave_command()?;
            command.args(["-O", "check"]);
            connection.add_destination(&mut command)?;
            let mut child = command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|error| format!("cannot check owned OpenSSH connection: {error}"))?;
            let status = match child.wait().await {
                Ok(status) => status,
                Err(error) => {
                    connection.retain_child(child, operation).await;
                    return Err(format!(
                        "original SSH check child wait failed; owner retained: {error}"
                    ));
                }
            };
            drop(operation);
            Ok(status.success())
        })
        .await
        .map_err(|error| format!("SSH connection check observation failed: {error}"))?
    }

    pub fn sandbox(self: &Arc<Self>) -> Arc<ProjectSshSandbox> {
        Arc::new(ProjectSshSandbox::new(Arc::clone(self)))
    }

    /// 连接级 SFTP 车道。惰性创建：从未用过 SFTP 的连接不持有任何常驻通道。
    pub(super) fn sftp_lanes(self: &Arc<Self>) -> Arc<lanes::SftpLanes> {
        Arc::clone(
            self.sftp_lanes
                .get_or_init(|| Arc::new(lanes::SftpLanes::new(self))),
        )
    }

    /// The caller must be the existing offloaded project/tool preparation
    /// boundary, because FileSystem is a synchronous framework contract.
    pub fn file_system(
        self: &Arc<Self>,
        remote_directory: String,
    ) -> Result<Arc<ProjectSshFileSystem>, String> {
        ProjectSshFileSystem::new(Arc::clone(self), remote_directory).map(Arc::new)
    }

    pub async fn verify_directory(self: &Arc<Self>, path: String) -> Result<String, String> {
        validate_remote_text(&path)?;
        self.check_authenticated().await?;
        // 远程相对路径属于远程用户 HOME，不能继承本机 cwd 或把 ~/ 当作 SFTP 字面目录。
        let resolved = self
            .sandbox()
            .execute(
                SandboxCommand::program(
                    "/bin/sh",
                    vec!["-c".to_string(), remote_directory_command(&path)?],
                )
                .with_working_dir("/"),
            )
            .await
            .map_err(|error| format!("远程目录解析失败：{error}"))?;
        if !resolved.success() || resolved.output_truncated {
            return Err(
                "远程文件夹不存在或无法访问，请检查目录；相对路径按远程用户 home 解析".to_string(),
            );
        }
        let canonical = resolved
            .stdout
            .strip_suffix('\n')
            .unwrap_or(&resolved.stdout);
        if !canonical.starts_with('/') {
            return Err("远程目录未返回完整绝对路径，项目尚未创建".to_string());
        }
        sftp::verify_directory(Arc::clone(self), canonical.to_string()).await
    }

    pub async fn verify_target_directory(
        self: &Arc<Self>,
        expected_target: TerminalTarget,
        path: String,
    ) -> Result<String, String> {
        if self.target != expected_target.normalized()? {
            return Err("authentication belongs to a different project SSH target".to_string());
        }
        let path = self.verify_directory(path).await?;
        let result = self
            .sandbox()
            .execute(
                SandboxCommand::program("/bin/sh", vec!["-c".to_string(), "pwd -P".to_string()])
                    .with_working_dir(&path),
            )
            .await
            .map_err(|error| error.to_string())?;
        let actual = result.stdout.strip_suffix('\n').unwrap_or(&result.stdout);
        if !result.success() || result.output_truncated || actual != path {
            return Err("remote SFTP directory and actual command working directory did not agree; project was not published".to_string());
        }
        Ok(path)
    }
}

fn remote_directory_command(path: &str) -> Result<String, String> {
    validate_remote_text(path)?;
    let directory = if path.starts_with('/') {
        shell_quote(path)?
    } else if path == "~" {
        "\"$HOME\"".to_string()
    } else if let Some(relative) = path.strip_prefix("~/") {
        format!("\"$HOME\"/{}", shell_quote(relative)?)
    } else if path.starts_with('~') {
        return Err("仅支持绝对路径、远程 home 下的相对路径或 ~/ 路径".to_string());
    } else {
        format!("\"$HOME\"/{}", shell_quote(path)?)
    };
    Ok(format!("cd -- {directory} && pwd -P"))
}

impl TerminalService {
    pub fn project_ssh_auth_terminal_id(owner_id: &str) -> String {
        // The GUI's original caller-generated session id is also the draft
        // connection id, so it can subscribe before the process is spawned.
        owner_id.to_string()
    }

    /// The returned terminal is used by the creation dialog for password and
    /// host-key interaction. It is not an ordinary closeable project tab.
    /// No target credential is stored, logged, or exposed to a model.
    pub async fn create_project_ssh_auth(
        self: &Arc<Self>,
        owner_id: String,
        target: TerminalTarget,
        rows: u16,
        cols: u16,
    ) -> Result<TerminalSessionInfo, String> {
        let _creation = self.project_creation_lock.lock().await;
        if owner_id.trim().is_empty() {
            return Err("project SSH owner id cannot be empty".to_string());
        }
        if let Some(connection) = self
            .project_connections
            .get(&owner_id)
            .map(|connection| Arc::clone(connection.value()))
        {
            if connection.target != target.clone().normalized()? {
                return Err("existing project connection has a different fixed target".to_string());
            }
            return self
                .session(connection.master_terminal_id())
                .map(|session| session.info.clone());
        }
        let connection = ProjectSshConnection::prepare(self, &owner_id, target)?;
        {
            // Reserve the caller's known event id under the same creation
            // boundary as ordinary PTYs. A draft can never claim an existing
            // session, and a normal create cannot steal its pending master id.
            let _sessions = self.creation_lock.lock().await;
            if self.sessions.contains_key(connection.master_terminal_id()) {
                return Err(
                    "project authentication id already belongs to an existing terminal".to_string(),
                );
            }
            self.project_connections
                .insert(owner_id.clone(), Arc::clone(&connection));
        }
        connection.resolve_effective_target().await?;
        let command = connection.master_command()?;
        let result = self
            .create_inner_with_project(
                connection.master_terminal_id.clone(),
                None,
                rows,
                cols,
                connection.target.clone(),
                None,
                Some(ProjectTerminalLaunch {
                    command,
                    connection,
                    operation: None,
                }),
                Vec::new(),
            )
            .await;
        // Failure may follow actual spawn; preserve its original session owner.
        if result.is_err()
            && self
                .project_connections
                .get(&owner_id)
                .is_some_and(|connection| !self.contains(connection.master_terminal_id()))
        {
            self.project_connections.remove(&owner_id);
        }
        result
    }

    pub fn project_ssh(&self, owner_id: &str) -> Result<Arc<ProjectSshConnection>, String> {
        self.project_connections
            .get(owner_id)
            .map(|connection| Arc::clone(connection.value()))
            .ok_or_else(|| "project SSH connection has not been prepared".to_string())
    }

    /// 读取单一物理owner快照；后继mutation仍须在原creation lock内核对Arc。
    pub fn project_ssh_exclusive(
        &self,
        owner_id: &str,
    ) -> Result<Arc<ProjectSshConnection>, String> {
        let connection = self.project_ssh(owner_id)?;
        if self
            .project_connections
            .iter()
            .any(|entry| entry.key() != owner_id && Arc::ptr_eq(entry.value(), &connection))
        {
            return Err("SSH physical connection has another owner".into());
        }
        Ok(connection)
    }

    /// Transfer the original prepared connection into the existing Workspace
    /// identity after owned project creation succeeds; no connection is rebuilt.
    pub async fn reassign_project_ssh(
        &self,
        previous_owner: &str,
        workspace_id: &str,
    ) -> Result<(), String> {
        let expected = self.project_ssh(previous_owner)?;
        self.reassign_project_ssh_checked(previous_owner, workspace_id, &expected)
            .await
    }

    /// 只转移刚才实际验证的物理 owner。防止认证后等待期间旧 draft
    /// 关闭/重建或同 Arc 双登记，让来源连接被误迁走或误关闭。
    pub async fn reassign_project_ssh_checked(
        &self,
        previous_owner: &str,
        workspace_id: &str,
        expected: &Arc<ProjectSshConnection>,
    ) -> Result<(), String> {
        let _creation = self.project_creation_lock.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(
                "terminal service is shutting down; connection transfer was not admitted"
                    .to_string(),
            );
        }
        if workspace_id.trim().is_empty() {
            return Err("workspace id cannot be empty".to_string());
        }
        let connection = self.project_ssh(previous_owner)?;
        if !Arc::ptr_eq(&connection, expected) {
            return Err(
                "SSH draft generation changed after verification; connection not transferred"
                    .into(),
            );
        }
        if self
            .project_connections
            .iter()
            .any(|entry| entry.key() != previous_owner && Arc::ptr_eq(entry.value(), &connection))
        {
            return Err(
                "SSH physical connection already has another owner; connection not transferred"
                    .into(),
            );
        }
        if previous_owner == workspace_id {
            return Ok(());
        }
        if self.project_connections.contains_key(workspace_id) {
            return Err("workspace already owns a project SSH connection".to_string());
        }
        // 本地资源保护：转交与原会话登记/退出发布串行；保留已失效或 closing 的
        // draft 原 owner，不让迟到认证/取消的关闭观察把它发布为新工作空间。
        let _sessions = self.creation_lock.lock().await;
        if self.shutting_down.load(Ordering::Acquire) || connection.closing.load(Ordering::Acquire)
        {
            return Err("SSH 认证连接正在关闭，请重新连接后重试".into());
        }
        let master = self
            .session(connection.master_terminal_id())
            .map_err(|_| "原 SSH 认证连接已退出或不存在，请重新连接后重试".to_string())?;
        let same_master = master
            .project_master_owner
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|owner| Arc::ptr_eq(&owner, &connection));
        if !same_master
            || master.close_requested.load(Ordering::Acquire)
            || master.child_exit_observed.load(Ordering::Acquire)
        {
            return Err("原 SSH 认证连接已退出、正在关闭或身份已变更，请重新连接后重试".into());
        }
        connection.effective_target()?;
        self.project_connections
            .insert(workspace_id.to_string(), connection);
        self.project_connections.remove(previous_owner);
        Ok(())
    }

    pub async fn create_project_ssh_terminal(
        self: &Arc<Self>,
        id: String,
        owner_id: &str,
        remote_directory: &str,
        rows: u16,
        cols: u16,
        resource_guards: Vec<InvocationResourceGuard>,
    ) -> Result<TerminalSessionInfo, String> {
        let connection = self.project_ssh(owner_id)?;
        let operation = connection.operation()?;
        let script = format!(
            "cd {} && exec \"${{SHELL:-/bin/sh}}\" -l",
            shell_quote(remote_directory)?
        );
        let mut command = CommandBuilder::new("ssh");
        command.args(connection.arguments(false)?);
        command.args(["-tt", "--", connection.destination()?, &script]);
        self.create_inner_with_project(
            id,
            None,
            rows,
            cols,
            connection.target.clone(),
            None,
            Some(ProjectTerminalLaunch {
                command,
                connection,
                operation: Some(operation),
            }),
            resource_guards,
        )
        .await
    }

    pub async fn close_project_ssh(&self, owner_id: &str) -> Result<(), String> {
        self.close_project_ssh_inner(owner_id, None).await
    }

    /// 原操作结算未移交draft时只关闭已验证的同一物理owner。
    /// 已移交的旧key不关闭新workspace；替换/双登记均保留并报告。
    pub async fn close_project_ssh_checked(
        &self,
        owner_id: &str,
        expected: &Arc<ProjectSshConnection>,
    ) -> Result<(), String> {
        self.close_project_ssh_inner(owner_id, Some(expected)).await
    }

    async fn close_project_ssh_inner(
        &self,
        owner_id: &str,
        expected: Option<&Arc<ProjectSshConnection>>,
    ) -> Result<(), String> {
        // Serializes physical close with the original draft -> Workspace map
        // transfer. A cancelled stale draft cannot close a published project.
        let _creation = self.project_creation_lock.lock().await;
        let Some(connection) = self
            .project_connections
            .get(owner_id)
            .map(|connection| Arc::clone(connection.value()))
        else {
            return Ok(());
        };
        if let Some(expected) = expected {
            if !Arc::ptr_eq(expected, &connection) {
                return Err(
                    "SSH draft generation changed before cleanup; current owner retained".into(),
                );
            }
            if self
                .project_connections
                .iter()
                .any(|entry| entry.key() != owner_id && Arc::ptr_eq(entry.value(), &connection))
            {
                return Err("SSH draft has another owner; source connection retained".into());
            }
        }
        if let Err(error) = connection.observe_idle().await {
            if !self.shutting_down.load(Ordering::Acquire) {
                // A bounded failed close observation is not a terminal state.
                // Allow the same owner to inspect/verify and retry later.
                connection.closing.store(false, Ordering::Release);
            }
            return Err(error);
        }
        // Only our original foreground master is closed. No `ssh -O exit`
        // sweep or user-configured/foreign master is ever selected.
        self.close_session(connection.master_terminal_id()).await?;
        self.project_connections
            .remove_if(owner_id, |_, current| Arc::ptr_eq(current, &connection));
        Ok(())
    }
}

pub(super) fn validate_remote_text(value: &str) -> Result<(), String> {
    if value.is_empty() || value.contains('\0') {
        Err("remote path or argument is empty or contains NUL".to_string())
    } else {
        Ok(())
    }
}

pub(super) fn shell_quote(value: &str) -> Result<String, String> {
    if value.contains('\0') {
        return Err("remote shell argument contains NUL".to_string());
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

pub(super) fn remote_path(path: &Path) -> Result<&str, String> {
    let path = path
        .to_str()
        .ok_or_else(|| "remote project path is not UTF-8".to_string())?;
    validate_remote_text(path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use echo_agent::tools::filesystem::FileSystem;

    use super::*;

    #[cfg(unix)]
    async fn launch_original_master_fixture(
        service: &Arc<TerminalService>,
        connection: &Arc<ProjectSshConnection>,
    ) -> Result<(), String> {
        // 仅替换现有 launch 的物理程序；原 PTY、reader、连接 owner 和结算路径都是真实的。
        // 本机 shell 不提供 SSH 认证证据，也不启动网络连接。
        service
            .create_inner_with_project(
                connection.master_terminal_id().to_string(),
                None,
                24,
                80,
                connection.target.clone(),
                None,
                Some(ProjectTerminalLaunch {
                    command: CommandBuilder::new("/bin/sh"),
                    connection: Arc::clone(connection),
                    operation: None,
                }),
                Vec::new(),
            )
            .await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reply_fork_exited_authentication_pty_cannot_transfer_registered_draft()
    -> Result<(), String> {
        let service = TerminalService::new();
        let mut receiver = service.subscribe();
        let target = TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let source = ProjectSshConnection::prepare(&service, "source-live", target.clone())?;
        let draft = ProjectSshConnection::prepare(&service, "draft-exited", target.clone())?;
        draft
            .effective_target
            .set(target)
            .map_err(|_| "fixture target already set")?;
        service
            .project_connections
            .insert("source-live".into(), Arc::clone(&source));
        service
            .project_connections
            .insert("draft-exited".into(), Arc::clone(&draft));
        let observed = async {
            launch_original_master_fixture(&service, &source).await?;
            launch_original_master_fixture(&service, &draft).await?;
            service.write("draft-exited", b"exit 23\r").await?;
            let original_exit = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match receiver.recv().await.map_err(|error| error.to_string())? {
                        crate::terminal::TerminalEvent::Exited { id, exit_code, .. }
                            if id == "draft-exited" =>
                        {
                            return Ok::<_, String>(exit_code);
                        }
                        crate::terminal::TerminalEvent::Issue { id, message }
                            if id == "draft-exited" =>
                        {
                            return Err(message);
                        }
                        _ => {}
                    }
                }
            })
            .await
            .map_err(|_| "original authentication fixture did not settle".to_string())??;
            let transferred = service
                .reassign_project_ssh_checked("draft-exited", "target", &draft)
                .await;
            let original_owner_kept = service
                .project_ssh("draft-exited")
                .is_ok_and(|current| Arc::ptr_eq(&current, &draft));
            let source_kept = service
                .project_ssh("source-live")
                .is_ok_and(|current| Arc::ptr_eq(&current, &source));
            let source_pty_kept = service.contains("source-live");
            let target_absent = service.project_ssh("target").is_err();
            Ok::<_, String>((
                original_exit,
                transferred,
                original_owner_kept,
                source_kept,
                source_pty_kept,
                target_absent,
            ))
        }
        .await;
        // 在任何断言前结算本测试的原资源；失败不能遗留本机 shell。
        let cleanup = service.close_all().await;
        let (
            original_exit,
            transferred,
            original_owner_kept,
            source_kept,
            source_pty_kept,
            target_absent,
        ) = observed?;
        cleanup?;
        assert_eq!(original_exit, 23);
        assert!(
            transferred.is_err(),
            "exited authentication owner was transferred"
        );
        assert!(original_owner_kept && target_absent);
        assert!(source_kept && source_pty_kept);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reply_fork_cancelled_close_observer_cannot_transfer_closing_draft()
    -> Result<(), String> {
        let service = TerminalService::new();
        let target = TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let draft = ProjectSshConnection::prepare(&service, "draft-closing", target.clone())?;
        draft
            .effective_target
            .set(target)
            .map_err(|_| "fixture target already set")?;
        service
            .project_connections
            .insert("draft-closing".into(), Arc::clone(&draft));
        launch_original_master_fixture(&service, &draft).await?;
        let operation = draft.operation()?;
        let closing_service = Arc::clone(&service);
        let closing_draft = Arc::clone(&draft);
        let observer = tokio::spawn(async move {
            closing_service
                .close_project_ssh_checked("draft-closing", &closing_draft)
                .await
        });
        let closing_started = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !draft.closing.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        observer.abort();
        let observer_cancelled = observer.await.is_err_and(|error| error.is_cancelled());
        let transferred = service
            .reassign_project_ssh_checked("draft-closing", "target", &draft)
            .await;
        let original_owner_kept = service
            .project_ssh("draft-closing")
            .is_ok_and(|current| Arc::ptr_eq(&current, &draft));
        let target_absent = service.project_ssh("target").is_err();
        let original_pty_kept = service.contains("draft-closing");
        let active_before_release = draft.active_operations.load(Ordering::Acquire);
        drop(operation);
        service.close_all().await?;
        closing_started.map_err(|_| "original close observer did not start".to_string())?;
        assert!(observer_cancelled);
        assert!(
            transferred.is_err(),
            "closing draft was transferred after observer cancellation"
        );
        assert!(original_owner_kept && target_absent && original_pty_kept);
        assert_eq!(active_before_release, 1);
        assert_eq!(draft.active_operations.load(Ordering::Acquire), 0);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reply_fork_observed_child_exit_before_reader_join_cannot_transfer_draft()
    -> Result<(), String> {
        let service = TerminalService::new();
        let target = TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let draft = ProjectSshConnection::prepare(&service, "reader-draft", target.clone())?;
        draft
            .effective_target
            .set(target)
            .map_err(|_| "fixture target already set")?;
        service
            .project_connections
            .insert("reader-draft".into(), Arc::clone(&draft));
        launch_original_master_fixture(&service, &draft).await?;
        let master = service.session("reader-draft")?;
        master.test.hold_reader.store(true, Ordering::Release);
        let observed = async {
            service.write("reader-draft", b"exit 29\r").await?;
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !master.test.reader_ended.load(Ordering::Acquire)
                    || master.test.wait_attempts.load(Ordering::Acquire) == 0
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| "original child exit was not observed before reader join".to_string())?;
            let original_reader_pending = !master.is_completed()?
                && service.contains("reader-draft")
                && !master.close_requested.load(Ordering::Acquire)
                && !draft.closing.load(Ordering::Acquire);
            let transferred = service
                .reassign_project_ssh_checked("reader-draft", "target", &draft)
                .await;
            let original_owner_kept = service
                .project_ssh("reader-draft")
                .is_ok_and(|current| Arc::ptr_eq(&current, &draft));
            let target_absent = service.project_ssh("target").is_err();
            Ok::<_, String>((
                original_reader_pending,
                transferred,
                original_owner_kept,
                target_absent,
            ))
        }
        .await;
        // 正常或失败路径都先让原reader实际join，不用状态位冒充结算。
        master.test.hold_reader.store(false, Ordering::Release);
        let cleanup = service.close_all().await;
        let (original_reader_pending, transferred, original_owner_kept, target_absent) = observed?;
        cleanup?;
        assert!(original_reader_pending);
        assert!(
            transferred.is_err(),
            "observed child exit was transferred before original reader joined"
        );
        assert!(original_owner_kept && target_absent);
        assert!(master.is_completed()?);
        let completed_code = master
            .completed
            .lock()
            .map_err(|_| "fixture completion lock poisoned".to_string())?
            .as_ref()
            .map(|status| status.exit_code());
        assert_eq!(completed_code, Some(29));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reply_fork_mux_session_cannot_impersonate_original_authentication_master()
    -> Result<(), String> {
        let service = TerminalService::new();
        let target = TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let draft = ProjectSshConnection::prepare(&service, "mux-draft", target.clone())?;
        draft
            .effective_target
            .set(target.clone())
            .map_err(|_| "fixture target already set")?;
        service
            .project_connections
            .insert("mux-draft".into(), Arc::clone(&draft));
        // 复用原 mux launch 形状，只有物理程序替为本机 shell。
        // 相同 id/target/connection 也不能把有 operation 的 slave 当作认证 master。
        service
            .create_inner_with_project(
                "mux-draft".into(),
                None,
                24,
                80,
                target,
                None,
                Some(ProjectTerminalLaunch {
                    command: CommandBuilder::new("/bin/sh"),
                    connection: Arc::clone(&draft),
                    operation: Some(draft.operation()?),
                }),
                Vec::new(),
            )
            .await?;
        let transferred = service
            .reassign_project_ssh_checked("mux-draft", "target", &draft)
            .await;
        let original_owner_kept = service
            .project_ssh("mux-draft")
            .is_ok_and(|current| Arc::ptr_eq(&current, &draft));
        let target_absent = service.project_ssh("target").is_err();
        let operation_retained = draft.active_operations.load(Ordering::Acquire);
        // 这是本机 mux 夹具，先按原 session 实际结算，随后关闭其连接 owner。
        service.close_session("mux-draft").await?;
        service.close_all().await?;
        assert!(transferred.is_err());
        assert!(original_owner_kept && target_absent);
        assert_eq!(operation_retained, 1);
        assert_eq!(draft.active_operations.load(Ordering::Acquire), 0);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_launch_pty_keeps_target_owner_and_reports_xterm_capabilities()
    -> Result<(), String> {
        use crate::terminal::TerminalEvent;
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = TerminalService::new();
        let mut receiver = service.subscribe();
        let target = TerminalTarget::Ssh {
            host: "original.invalid".to_string(),
            user: Some("echo".to_string()),
            port: Some(2222),
        };
        let connection = ProjectSshConnection::prepare(&service, "term-launch", target.clone())?;
        service
            .project_connections
            .insert("term-launch".into(), Arc::clone(&connection));
        let operation = connection.operation()?;
        let mut command = CommandBuilder::new("/bin/sh");
        command.cwd(directory.path());
        // 复用既有SSH launch注入缝，只替换物理子进程；不启动SSH、不声称已认证。
        service
            .create_inner_with_project(
                "term-launch".into(),
                None,
                29,
                87,
                target.clone(),
                None,
                Some(ProjectTerminalLaunch {
                    command,
                    connection: Arc::clone(&connection),
                    operation: Some(operation),
                }),
                Vec::new(),
            )
            .await?;
        assert_eq!(service.session("term-launch")?.info.target, target);
        assert_eq!(connection.active_operations.load(Ordering::Acquire), 1);
        service
            .write(
                "term-launch",
                b"printf 'TERM_CAP=<%s>\\nCWD_CAP=<%s>\\n' \"$TERM\" \"$PWD\"; stty size; exit 7\r",
            )
            .await?;
        let (output, code) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut output = Vec::new();
            loop {
                match receiver.recv().await {
                    Ok(TerminalEvent::Output { id, bytes }) if id == "term-launch" => {
                        output.extend(bytes)
                    }
                    Ok(TerminalEvent::Exited { id, exit_code, .. }) if id == "term-launch" => {
                        break Ok::<_, String>((output, exit_code));
                    }
                    Ok(TerminalEvent::Issue { message, .. }) => break Err(message),
                    Ok(_) => {}
                    Err(error) => break Err(error.to_string()),
                }
            }
        })
        .await
        .map_err(|_| "original project PTY owner did not settle".to_string())??;
        assert_eq!(code, 7);
        assert_eq!(connection.active_operations.load(Ordering::Acquire), 0);
        assert!(!service.contains("term-launch"));
        let retained = service
            .project_connections
            .get("term-launch")
            .ok_or_else(|| "original project connection was replaced".to_string())?;
        assert!(Arc::ptr_eq(retained.value(), &connection));
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("TERM_CAP=<xterm-256color>"), "{output}");
        assert!(output.contains("29 87"), "{output}");
        let cwd = directory
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        assert!(
            output.contains(&format!("CWD_CAP=<{}>", cwd.display())),
            "{output}"
        );
        Ok(())
    }

    #[test]
    fn relative_project_directory_resolves_remote_home_not_current_directory() -> Result<(), String>
    {
        let home = std::env::var("HOME").map_err(|error| error.to_string())?;
        let expected = std::fs::canonicalize(home).map_err(|error| error.to_string())?;
        for selected in [".", "~", "~/."] {
            let result = std::process::Command::new("/bin/sh")
                .args(["-c", &remote_directory_command(selected)?])
                .current_dir("/")
                .output()
                .map_err(|error| error.to_string())?;
            assert!(result.status.success());
            assert_eq!(
                String::from_utf8_lossy(&result.stdout).trim_end(),
                expected.to_string_lossy()
            );
        }
        assert!(remote_directory_command("~other/project").is_err());
        assert!(remote_directory_command("bad\0path").is_err());
        Ok(())
    }

    #[test]
    fn remote_directory_keeps_shell_text_literal() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let directory = temporary.path().join("中文 ' $(touch must-not-exist)");
        std::fs::create_dir(&directory).map_err(|error| error.to_string())?;
        let selected = directory.to_str().ok_or("test path is not UTF-8")?;
        let result = std::process::Command::new("/bin/sh")
            .args(["-c", &remote_directory_command(selected)?])
            .current_dir(temporary.path())
            .output()
            .map_err(|error| error.to_string())?;
        assert!(result.status.success());
        assert!(!temporary.path().join("must-not-exist").exists());
        let expected = directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        assert_eq!(
            String::from_utf8_lossy(&result.stdout).trim_end(),
            expected.to_string_lossy()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn masked_form_bytes_reach_original_pty_without_password_echo() -> Result<(), String> {
        use super::super::TerminalEvent;
        let service = TerminalService::new();
        let mut receiver = service.subscribe();
        service
            .create_with_shell_for_test("masked-probe".into(), None, 24, 80, "/bin/sh".into())
            .await?;
        let probe = async {
            service.write("masked-probe", b"stty -echo; printf '%s%s\\n' SSH_ PASSWORD_READY; IFS= read -r answer; if [ \"$answer\" = public-probe-42 ]; then printf '%s%s\\n' INPUT_ REACHED_PTY; fi; exit\r").await?;
            let mut output = Vec::new();
            loop {
                match receiver.recv().await.map_err(|error| error.to_string())? {
                    TerminalEvent::Output { bytes, .. } => {
                        output.extend(bytes);
                        if String::from_utf8_lossy(&output).contains("SSH_PASSWORD_READY") {
                            break;
                        }
                    }
                    TerminalEvent::Issue { message, .. } => return Err(message),
                    TerminalEvent::Exited { .. } => return Err("PTY 在输入挑战前退出".to_string()),
                }
            }
            service.write("masked-probe", b"public-probe-42\r").await?;
            let mut after_prompt = Vec::new();
            loop {
                match receiver.recv().await.map_err(|error| error.to_string())? {
                    TerminalEvent::Output { bytes, .. } => after_prompt.extend(bytes),
                    TerminalEvent::Issue { message, .. } => return Err(message),
                    TerminalEvent::Exited { .. } => break,
                }
            }
            let received = String::from_utf8_lossy(&after_prompt);
            assert!(received.contains("INPUT_REACHED_PTY"));
            assert!(!received.contains("public-probe-42"));
            Ok(())
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), probe)
            .await
            .map_err(|_| "PTY 输入挑战超时".to_string());
        service.close_all().await?;
        result?
    }

    #[test]
    fn quoting_keeps_unicode_and_shell_metacharacters_literal() -> Result<(), String> {
        assert_eq!(
            shell_quote("/目录/it's 🦀 $(touch nope)")?,
            "'/目录/it'\\''s 🦀 $(touch nope)'"
        );
        assert_eq!(shell_quote("")?, "''");
        assert!(shell_quote("a\0b").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_master_cannot_fall_back_to_new_authentication() -> Result<(), String> {
        let service = TerminalService::new();
        let connection = ProjectSshConnection::prepare(
            &service,
            "test-missing-master",
            TerminalTarget::Ssh {
                host: "example.invalid".into(),
                user: None,
                port: None,
            },
        )?;
        connection
            .effective_target
            .set(TerminalTarget::Ssh {
                host: "example.invalid".into(),
                user: Some("fixture".into()),
                port: Some(22),
            })
            .map_err(|_| "test effective target already exists".to_string())?;
        let args = connection.arguments(false)?;
        assert!(args.iter().any(|argument| argument == "ProxyCommand=false"));
        assert!(args.iter().any(|argument| argument == "BatchMode=yes"));
        assert!(!connection.control_path().exists());
        // This is argument/source preparation; no actual SSH process is run.
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_cannot_drop_an_active_project_operation() -> Result<(), String> {
        let service = TerminalService::new();
        let connection = ProjectSshConnection::prepare(
            &service,
            "test-shutdown",
            TerminalTarget::Ssh {
                host: "example.invalid".into(),
                user: None,
                port: None,
            },
        )?;
        let operation = connection.operation()?;
        connection.begin_shutdown();
        assert!(connection.operation().is_err());
        assert_eq!(connection.active_operations.load(Ordering::Acquire), 1);
        drop(operation);
        connection.observe_idle().await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reply_fork_reassign_rejects_replaced_or_double_registered_owner() -> Result<(), String>
    {
        let service = TerminalService::new();
        let target = TerminalTarget::Ssh {
            host: "example.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let original = ProjectSshConnection::prepare(&service, "draft", target.clone())?;
        let replacement = ProjectSshConnection::prepare(&service, "draft", target.clone())?;
        assert_ne!(original.generation(), replacement.generation());
        replacement
            .effective_target
            .set(target)
            .map_err(|_| "fixture target already set")?;
        service
            .project_connections
            .insert("draft".into(), replacement.clone());
        launch_original_master_fixture(&service, &replacement).await?;
        assert!(
            service
                .reassign_project_ssh_checked("draft", "destination", &original)
                .await
                .is_err()
        );
        assert!(Arc::ptr_eq(&service.project_ssh("draft")?, &replacement));
        assert!(service.project_ssh("destination").is_err());
        service
            .project_connections
            .insert("source".into(), replacement.clone());
        assert!(
            service
                .reassign_project_ssh_checked("draft", "destination", &replacement)
                .await
                .is_err()
        );
        assert!(Arc::ptr_eq(&service.project_ssh("source")?, &replacement));
        service.project_connections.remove("source");
        service
            .reassign_project_ssh_checked("draft", "destination", &replacement)
            .await?;
        assert!(service.project_ssh("draft").is_err());
        service.close_project_ssh("destination").await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reply_fork_pretransfer_failure_closes_only_the_proven_independent_draft()
    -> Result<(), String> {
        let service = TerminalService::new();
        let target = TerminalTarget::Ssh {
            host: "example.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let source = ProjectSshConnection::prepare(&service, "source", target.clone())?;
        let draft =
            ProjectSshConnection::prepare(&service, "reply-fork-auth-fixture", target.clone())?;
        let replaced = ProjectSshConnection::prepare(&service, "reply-fork-auth-fixture", target)?;
        service
            .project_connections
            .insert("source".into(), source.clone());
        service
            .project_connections
            .insert("reply-fork-auth-fixture".into(), draft.clone());
        assert!(
            service
                .close_project_ssh_checked("reply-fork-auth-fixture", &replaced)
                .await
                .is_err()
        );
        assert!(service.project_ssh("reply-fork-auth-fixture").is_ok());
        service
            .close_project_ssh_checked("reply-fork-auth-fixture", &draft)
            .await?;
        assert!(service.project_ssh("reply-fork-auth-fixture").is_err());
        let active = source.operation()?;
        drop(active);
        assert!(!source.closing.load(Ordering::Acquire));
        service
            .project_connections
            .insert("bad-double-owner".into(), source.clone());
        assert!(
            service
                .close_project_ssh_checked("bad-double-owner", &source)
                .await
                .is_err()
        );
        assert!(!source.closing.load(Ordering::Acquire));
        service.project_connections.remove("bad-double-owner");
        service.close_project_ssh("source").await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reply_fork_distinct_owners_survive_closing_either_side() -> Result<(), String> {
        let service = TerminalService::new();
        let target = TerminalTarget::Ssh {
            host: "example.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        for close_source_first in [true, false] {
            let source = ProjectSshConnection::prepare(&service, "source", target.clone())?;
            let draft = ProjectSshConnection::prepare(&service, "draft", target.clone())?;
            source
                .effective_target
                .set(target.clone())
                .map_err(|_| "fixture source already set")?;
            draft
                .effective_target
                .set(target.clone())
                .map_err(|_| "fixture draft already set")?;
            assert!(!Arc::ptr_eq(&source, &draft));
            assert_ne!(source.generation(), draft.generation());
            assert_ne!(source.master_terminal_id(), draft.master_terminal_id());
            service
                .project_connections
                .insert("source".into(), source.clone());
            service
                .project_connections
                .insert("draft".into(), draft.clone());
            launch_original_master_fixture(&service, &source).await?;
            launch_original_master_fixture(&service, &draft).await?;
            service
                .reassign_project_ssh_checked("draft", "destination", &draft)
                .await?;
            let (first, survivor) = if close_source_first {
                ("source", "destination")
            } else {
                ("destination", "source")
            };
            service.close_project_ssh(first).await?;
            let remaining = service.project_ssh(survivor)?;
            let operation = remaining.operation()?;
            assert!(!remaining.closing.load(Ordering::Acquire));
            drop(operation);
            service.close_project_ssh(survivor).await?;
        }
        // 仅原物理 owner/准入控制；没有启动 SSH 或真实远端 PTY。
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn old_draft_close_cannot_close_transferred_project_resources() -> Result<(), String> {
        let service = TerminalService::new();
        let connection = ProjectSshConnection::prepare(
            &service,
            "draft",
            TerminalTarget::Ssh {
                host: "example.invalid".into(),
                user: None,
                port: None,
            },
        )?;
        service
            .project_connections
            .insert("draft".into(), Arc::clone(&connection));
        connection
            .effective_target
            .set(TerminalTarget::Ssh {
                host: "example.invalid".into(),
                user: Some("fixture".into()),
                port: Some(22),
            })
            .map_err(|_| "test effective target already exists".to_string())?;
        launch_original_master_fixture(&service, &connection).await?;
        service.reassign_project_ssh("draft", "workspace").await?;
        service.close_project_ssh("draft").await?;
        assert!(Arc::ptr_eq(&service.project_ssh("workspace")?, &connection));
        let operation = connection.operation()?;
        assert_eq!(connection.master_terminal_id(), "draft");
        drop(operation);
        service.close_project_ssh("workspace").await?;
        assert!(service.project_ssh("workspace").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edited_endpoint_is_rejected_before_any_ssh_command() -> Result<(), String> {
        let service = TerminalService::new();
        let connection = ProjectSshConnection::prepare(
            &service,
            "draft",
            TerminalTarget::Ssh {
                host: "first.invalid".into(),
                user: Some("first".into()),
                port: Some(22),
            },
        )?;
        let error = connection
            .verify_target_directory(
                TerminalTarget::Ssh {
                    host: "second.invalid".into(),
                    user: Some("first".into()),
                    port: Some(22),
                },
                "/项目".into(),
            )
            .await
            .err();
        assert!(error.is_some_and(|error| error.contains("different project SSH target")));
        assert_eq!(connection.active_operations.load(Ordering::Acquire), 0);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_adapters_keep_effective_identity_without_network_access() -> Result<(), String>
    {
        let service = TerminalService::new();
        let connection = ProjectSshConnection::prepare(
            &service,
            "project-adapter-fixture",
            TerminalTarget::Ssh {
                host: "eko-alias".into(),
                user: None,
                port: None,
            },
        )?;
        let effective = TerminalTarget::Ssh {
            host: "198.51.100.7".into(),
            user: Some("echo".into()),
            port: Some(2222),
        };
        connection
            .effective_target
            .set(effective.clone())
            .map_err(|_| "test effective target already exists".to_string())?;

        assert!(
            connection
                .verify_effective_target(&connection.target)
                .is_err()
        );
        let files = connection.file_system("/srv/项目".to_string())?;
        assert_eq!(
            files.current_dir().map_err(|error| error.to_string())?,
            PathBuf::from("/srv/项目")
        );
        let executor = connection.sandbox();
        assert_eq!(executor.name(), "ssh");
        assert_eq!(
            executor.isolation_level(),
            echo_agent::sandbox::IsolationLevel::None
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_closes_human_sessions_without_closing_a_busy_project_master()
    -> Result<(), String> {
        let service = TerminalService::new();
        let connection = ProjectSshConnection::prepare(
            &service,
            "master-fixture",
            TerminalTarget::Ssh {
                host: "example.invalid".into(),
                user: None,
                port: None,
            },
        )?;
        service
            .project_connections
            .insert("project".into(), Arc::clone(&connection));
        // A local shell exercises the original PTY owner/close boundary; this
        // fixture never starts SSH or claims that authentication succeeded.
        service
            .create_inner_with_project(
                "master-fixture".into(),
                None,
                24,
                80,
                connection.target.clone(),
                None,
                Some(ProjectTerminalLaunch {
                    command: CommandBuilder::new("/bin/sh"),
                    connection: Arc::clone(&connection),
                    operation: None,
                }),
                Vec::new(),
            )
            .await?;
        service
            .create_with_shell_for_test("human-fixture".into(), None, 24, 80, "/bin/sh".into())
            .await?;
        let collision = service
            .create_project_ssh_auth("human-fixture".into(), connection.target.clone(), 24, 80)
            .await;
        assert!(collision.is_err());
        assert!(service.project_ssh("human-fixture").is_err());
        let operation = connection.operation()?;
        service.begin_shutdown();
        service.close_interactive_sessions().await?;
        assert!(!service.contains("human-fixture"));
        assert!(service.contains("master-fixture"));
        assert_eq!(connection.active_operations.load(Ordering::Acquire), 1);
        drop(operation);
        service.close_all().await?;
        assert!(!service.contains("master-fixture"));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn same_connection_resource_survives_lost_physical_observer_until_actual_fixture_proof()
    -> Result<(), Box<dyn std::error::Error>> {
        struct GuardDrop(Arc<AtomicUsize>);
        impl Drop for GuardDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }
        let service = TerminalService::new();
        let target = TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let connection =
            ProjectSshConnection::prepare(&service, "fixture-resource", target.clone())?;
        connection
            .effective_target
            .set(target.clone())
            .map_err(|_| "duplicate fixture target")?;
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()?;
        let resources = connection.retain_remote_operation(
            &child,
            target.clone(),
            connection.operation()?,
            vec![InvocationResourceGuard::new(GuardDrop(Arc::clone(
                &dropped,
            )))],
        );
        resources.mark_stop_unknown("fixture physical observer was lost");
        assert_eq!(resources.target, target);
        drop(resources);
        assert_eq!(dropped.load(Ordering::Acquire), 0);
        connection.retry_retained_children().await?;
        let resources = connection
            .retained_children
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find_map(|resource| match resource {
                RetainedSshResource::RemoteOperation(resource) => Some(Arc::clone(resource)),
                _ => None,
            })
            .ok_or("connection discarded its actual resource")?;
        assert!(resources.stop_is_unknown());
        assert_eq!(connection.active_operations.load(Ordering::Acquire), 1);
        // This native child belongs to the self-owned fixture, not SSH. Its
        // actual status supplies test cleanup proof unavailable after remote255.
        assert!(child.wait().await?.success());
        resources.complete_with_proof();
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        assert!(
            connection
                .retained_children
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
        connection.observe_idle().await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retained_local_child_clears_only_after_its_original_wait_and_reader_join()
    -> Result<(), Box<dyn std::error::Error>> {
        struct GuardDrop(Arc<AtomicUsize>);
        impl Drop for GuardDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }
        let service = TerminalService::new();
        let connection = ProjectSshConnection::prepare(
            &service,
            "local-retained-fixture",
            TerminalTarget::Ssh {
                host: "fixture.invalid".into(),
                user: Some("fixture".into()),
                port: Some(22),
            },
        )?;
        let child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()?;
        let dropped = Arc::new(AtomicUsize::new(0));
        let (release, released) = tokio::sync::oneshot::channel();
        let reader = tokio::spawn(async move { released.await.map_err(|error| error.to_string()) });
        connection
            .retain_child_with_resources(
                child,
                connection.operation()?,
                vec![InvocationResourceGuard::new(GuardDrop(Arc::clone(
                    &dropped,
                )))],
                vec![reader],
            )
            .await;
        connection.retry_retained_children().await?;
        assert_eq!(connection.active_operations.load(Ordering::Acquire), 1);
        assert_eq!(dropped.load(Ordering::Acquire), 0);
        release
            .send(())
            .map_err(|_| "original fixture reader was lost")?;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while connection.active_operations.load(Ordering::Acquire) != 0 {
                connection.retry_retained_children().await?;
                tokio::task::yield_now().await;
            }
            Ok::<(), String>(())
        })
        .await??;
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        connection.observe_idle().await?;
        Ok(())
    }
}
