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

mod cas;
mod channel;
mod effective;
mod reader;
mod sandbox;
mod sftp;
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
        }))
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

    async fn observe_idle(&self) -> Result<(), String> {
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
                    return Err(format!("original SSH check child wait failed; owner retained: {error}"));
                }
            };
            drop(operation);
            if status.success() {
                Ok(())
            } else {
                Err("project SSH authentication is not ready; complete the GUI authentication terminal or reconnect".to_string())
            }
        })
        .await
        .map_err(|error| format!("SSH connection check observation failed: {error}"))?
    }

    pub fn sandbox(self: &Arc<Self>) -> Arc<ProjectSshSandbox> {
        Arc::new(ProjectSshSandbox::new(Arc::clone(self)))
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
        sftp::verify_directory(Arc::clone(self), path).await
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

    /// Transfer the original prepared connection into the existing Workspace
    /// identity after owned project creation succeeds; no connection is rebuilt.
    pub async fn reassign_project_ssh(
        &self,
        previous_owner: &str,
        workspace_id: &str,
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
        if previous_owner == workspace_id {
            return self.project_ssh(previous_owner).map(|_| ());
        }
        if self.project_connections.contains_key(workspace_id) {
            return Err("workspace already owns a project SSH connection".to_string());
        }
        let connection = self.project_ssh(previous_owner)?;
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
    use super::*;

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
