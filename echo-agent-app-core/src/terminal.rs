//! Application-owned interactive PTY sessions. OpenSSH is a process target, not
//! another protocol client, execution owner, or source of authentication claims.

use dashmap::DashMap;
use echo_agent::tools::InvocationResourceGuard;
use portable_pty::{CommandBuilder, ExitStatus, PtyPair, PtySize};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as BlockingMutex, Weak};
use std::time::Duration;
use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinHandle;

mod project_ssh;
pub use project_ssh::{
    ProjectSshConnection, ProjectSshFileSystem, ProjectSshSandbox, RemoteContentSearch,
};

pub const MAX_TERMINAL_WRITE_BYTES: usize = 64 * 1024;
const DEFAULT_EVENT_CAPACITY: usize = 256;
// This bounds an observer, never the lifetime of the original cleanup task.
const CLOSE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalTarget {
    #[default]
    Local,
    Ssh {
        host: String,
        user: Option<String>,
        port: Option<u16>,
    },
}

impl TerminalTarget {
    fn normalized(self) -> Result<Self, String> {
        match self {
            Self::Local => Ok(Self::Local),
            Self::Ssh { host, user, port } => {
                let host = host.trim().to_string();
                if host.is_empty()
                    || host.starts_with('-')
                    || host.contains('@')
                    || host.contains("://")
                    || host.chars().any(|c| c.is_whitespace() || c.is_control())
                {
                    return Err(
                        "SSH host must be a host or existing alias; enter the account separately"
                            .to_string(),
                    );
                }
                let user = user
                    .map(|user| user.trim().to_string())
                    .filter(|user| !user.is_empty());
                if user
                    .as_ref()
                    .is_some_and(|user| user.chars().any(char::is_control))
                {
                    return Err("SSH account cannot contain control characters".to_string());
                }
                if port == Some(0) {
                    return Err("SSH port must be between 1 and 65535".to_string());
                }
                Ok(Self::Ssh { host, user, port })
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Local => "local".to_string(),
            Self::Ssh { host, user, port } => {
                let account = user
                    .as_ref()
                    .map(|user| format!("{user}@"))
                    .unwrap_or_default();
                let port = port
                    .map(|port| format!(" (port {port})"))
                    .unwrap_or_default();
                format!("SSH {account}{host}{port}")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    Output {
        id: String,
        bytes: Vec<u8>,
    },
    Exited {
        id: String,
        reason: TerminalExitReason,
        exit_code: u32,
        signal: Option<String>,
    },
    // An error is not an exit receipt: the original resource owner remains.
    Issue {
        id: String,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalExitReason {
    ProcessExited,
    Closed,
    ReadFailed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalSessionInfo {
    pub id: String,
    pub pid: u32,
    pub target: TerminalTarget,
}

struct TerminalResources {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Option<std::thread::JoinHandle<Result<(), String>>>,
    reader_result: Option<Result<(), String>>,
    waited: Option<ExitStatus>,
    creation_error: Option<String>,
    // A project authentication master and each mux terminal retain the same
    // application-owned connection until original child wait AND reader join.
    project_connection: Option<Arc<ProjectSshConnection>>,
    project_operation: Option<project_ssh::ProjectSshOperation>,
    resource_guards: Vec<InvocationResourceGuard>,
}

#[cfg(test)]
#[derive(Default)]
struct TerminalTestControl {
    fail_kill: AtomicUsize,
    fail_wait: AtomicUsize,
    kill_attempts: AtomicUsize,
    wait_attempts: AtomicUsize,
    hold_reader: AtomicBool,
    fail_reader: AtomicBool,
    reader_ended: AtomicBool,
}

type TerminalSettlementSlot = Arc<Mutex<Option<JoinHandle<Result<(), String>>>>>;

struct TerminalSession {
    info: TerminalSessionInfo,
    // 只读来源身份，不持有资源、不证明远端状态。原资源锁贯穿 child/reader 结算，
    // owner 转交不得等待那把锁，也不能把同 id 的普通/mux PTY 当作认证 master。
    project_master_owner: Option<Weak<ProjectSshConnection>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Arc<BlockingMutex<Box<dyn Write + Send>>>,
    // These are the original handles, retained across failed/cancelled observers.
    resources: BlockingMutex<TerminalResources>,
    settlement: TerminalSettlementSlot,
    completed: BlockingMutex<Option<ExitStatus>>,
    // 原 waiter 的只读退出观察；reader/资源仍须完整结算，不能把此位当作 Closed。
    child_exit_observed: AtomicBool,
    close_requested: AtomicBool,
    exit_published: AtomicBool,
    service: Weak<TerminalService>,
    observation_timeout: Duration,
    #[cfg(test)]
    test: TerminalTestControl,
}

fn command_for_target(
    target: &TerminalTarget,
    cwd: Option<PathBuf>,
    shell: Option<String>,
) -> CommandBuilder {
    let mut command = match target {
        TerminalTarget::Local => CommandBuilder::new(shell.unwrap_or_else(default_shell)),
        TerminalTarget::Ssh { host, user, port } => {
            let mut command = CommandBuilder::new("ssh");
            command.arg("-tt");
            if let Some(user) = user {
                command.arg("-l");
                command.arg(user);
            }
            if let Some(port) = port {
                command.arg("-p");
                command.arg(port.to_string());
            }
            command.arg("--");
            command.arg(host);
            command
        }
    };
    if let Some(cwd) = cwd {
        command.cwd(cwd);
    }
    command
}

impl TerminalSession {
    #[allow(clippy::too_many_arguments)]
    fn prepare(
        service: &Arc<TerminalService>,
        id: String,
        cwd: Option<PathBuf>,
        size: PtySize,
        target: TerminalTarget,
        shell: Option<String>,
        settlement: TerminalSettlementSlot,
        project_launch: Option<project_ssh::ProjectTerminalLaunch>,
        resource_guards: Vec<InvocationResourceGuard>,
    ) -> Result<(Arc<Self>, Box<dyn Read + Send>), String> {
        validate_dimensions(size.rows, size.cols)?;
        let target = target.normalized()?;
        let (mut command, project_connection, project_operation) = match project_launch {
            Some(launch) => (launch.command, Some(launch.connection), launch.operation),
            None => (command_for_target(&target, cwd, shell), None, None),
        };
        let project_master_owner = project_connection
            .as_ref()
            .filter(|connection| {
                project_operation.is_none() && connection.master_terminal_id() == id.as_str()
            })
            .map(Arc::downgrade);
        // 前端是 xterm 仿真器；所有交互 PTY（包括原 SSH launch）都应广告其能力，
        // 不能把启动宿主继承的 TERM=dumb 当作终端类型。这里只设置子进程环境，
        // 不改全局环境或 shell 配置；当前没有独立的产品 terminal-type 配置。
        command.env("TERM", "xterm-256color");
        let pair: PtyPair = portable_pty::native_pty_system()
            .openpty(size)
            .map_err(|error| format!("failed to open PTY: {error}"))?;
        // Complete fallible FD preparation before spawning a child.
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("failed to clone PTY reader: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("failed to take PTY writer: {error}"))?;
        let child = pair.slave.spawn_command(command).map_err(|error| match &target {
            TerminalTarget::Ssh { .. } => format!("failed to start system ssh; ensure OpenSSH is available in PATH (no local fallback): {error}"),
            TerminalTarget::Local => format!("failed to spawn terminal shell: {error}"),
        })?;
        drop(pair.slave);
        let pid = child.process_id().unwrap_or(0);
        Ok((
            Arc::new(Self {
                info: TerminalSessionInfo { id, pid, target },
                project_master_owner,
                master: Mutex::new(pair.master),
                writer: Arc::new(BlockingMutex::new(writer)),
                resources: BlockingMutex::new(TerminalResources {
                    child,
                    reader: None,
                    reader_result: None,
                    waited: None,
                    creation_error: None,
                    project_connection,
                    project_operation,
                    resource_guards,
                }),
                settlement,
                completed: BlockingMutex::new(None),
                child_exit_observed: AtomicBool::new(false),
                close_requested: AtomicBool::new(false),
                exit_published: AtomicBool::new(false),
                service: Arc::downgrade(service),
                observation_timeout: service.observation_timeout,
                #[cfg(test)]
                test: TerminalTestControl::default(),
            }),
            reader,
        ))
    }

    async fn write(&self, bytes: &[u8]) -> Result<(), String> {
        let writer = Arc::clone(&self.writer);
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut writer = writer
                .lock()
                .map_err(|_| "terminal writer lock poisoned".to_string())?;
            writer
                .write_all(&bytes)
                .map_err(|error| format!("terminal write failed: {error}"))?;
            writer
                .flush()
                .map_err(|error| format!("terminal flush failed: {error}"))
        })
        .await
        .map_err(|error| format!("terminal write observation failed: {error}"))?
    }

    async fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        validate_dimensions(rows, cols)?;
        self.master
            .lock()
            .await
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("terminal resize failed: {error}"))
    }

    fn is_completed(&self) -> Result<bool, String> {
        self.completed
            .lock()
            .map(|status| status.is_some())
            .map_err(|_| "terminal completion lock poisoned".to_string())
    }

    async fn close(self: &Arc<Self>) -> Result<(), String> {
        self.close_requested.store(true, Ordering::Release);
        let observation = async {
            let mut task = self.settlement.lock().await;
            if self.is_completed()? {
                return Ok(());
            }
            if task.is_none() {
                *task = Some(start_settlement(Arc::clone(self)));
            }
            let handle = task
                .as_mut()
                .ok_or_else(|| "terminal settlement handle missing".to_string())?;
            let result = (&mut *handle)
                .await
                .map_err(|error| format!("terminal settlement observation failed: {error}"));
            // Only consume a handle after its actual terminal result was observed.
            *task = None;
            result?
        };
        tokio::time::timeout(self.observation_timeout, observation)
            .await
            .map_err(|_| {
                "terminal close is not confirmed; original child/reader owner retained, retry close"
                    .to_string()
            })?
    }

    fn settle_original_resources(&self) -> Result<ExitStatus, String> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| "terminal resource lock poisoned".to_string())?;
        let mut kill_sent = false;
        loop {
            if self.service.upgrade().is_none() {
                self.close_requested.store(true, Ordering::Release);
            }
            if resources.reader_result.is_none()
                && resources
                    .reader
                    .as_ref()
                    .is_some_and(|reader| reader.is_finished())
            {
                let reader = resources
                    .reader
                    .take()
                    .ok_or_else(|| "terminal reader handle missing".to_string())?;
                resources.reader_result = Some(
                    reader
                        .join()
                        .map_err(|_| "terminal reader thread failed".to_string())
                        .and_then(|result| result),
                );
            }
            let read_failed = resources
                .reader_result
                .as_ref()
                .is_some_and(|result| result.is_err());
            if resources.waited.is_none() {
                match resources.child.try_wait().map_err(|error| {
                    format!("terminal process observation failed; owner retained: {error}")
                })? {
                    Some(_) => {
                        // 已确认原 child 退出时立即阻止后继转交；原 wait/join 与资源保留照旧。
                        self.child_exit_observed.store(true, Ordering::Release);
                        #[cfg(test)]
                        {
                            self.test.wait_attempts.fetch_add(1, Ordering::Relaxed);
                            if injected_failure(&self.test.fail_wait) {
                                return Err("injected original child wait failure".to_string());
                            }
                        }
                        resources.waited = Some(resources.child.wait().map_err(|error| {
                            format!("terminal child wait failed; owner retained: {error}")
                        })?);
                    }
                    None if (self.close_requested.load(Ordering::Acquire) || read_failed)
                        && !kill_sent =>
                    {
                        #[cfg(test)]
                        {
                            self.test.kill_attempts.fetch_add(1, Ordering::Relaxed);
                            if injected_failure(&self.test.fail_kill) {
                                return Err("injected original child kill failure".to_string());
                            }
                        }
                        // portable-pty's original Child::kill escalates after SIGHUP;
                        // clone_killer does not. Still require original wait and join.
                        resources.child.kill().map_err(|error| {
                            format!("terminal child kill failed; owner retained: {error}")
                        })?;
                        kill_sent = true;
                    }
                    None => {}
                }
            }
            if let Some(status) = resources.waited.as_ref() {
                match resources.reader_result.as_ref() {
                    Some(Ok(())) => return Ok(status.clone()),
                    Some(Err(error)) => {
                        return Err(format!(
                            "terminal reader failed; original session retained: {error}"
                        ));
                    }
                    None => {}
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

pub struct TerminalService {
    sessions: DashMap<String, Arc<TerminalSession>>,
    events: broadcast::Sender<TerminalEvent>,
    creation_lock: Mutex<()>,
    project_connections: DashMap<String, Arc<ProjectSshConnection>>,
    project_creation_lock: Mutex<()>,
    shutting_down: AtomicBool,
    observation_timeout: Duration,
    #[cfg(test)]
    spawn_attempts: AtomicUsize,
}

impl TerminalService {
    pub fn new() -> Arc<Self> {
        Self::with_event_capacity(DEFAULT_EVENT_CAPACITY)
    }
    fn with_event_capacity(capacity: usize) -> Arc<Self> {
        let (events, _) = broadcast::channel(capacity.max(1));
        Arc::new(Self {
            sessions: DashMap::new(),
            events,
            creation_lock: Mutex::new(()),
            project_connections: DashMap::new(),
            project_creation_lock: Mutex::new(()),
            shutting_down: AtomicBool::new(false),
            observation_timeout: CLOSE_OBSERVATION_TIMEOUT,
            #[cfg(test)]
            spawn_attempts: AtomicUsize::new(0),
        })
    }
    pub fn subscribe(&self) -> broadcast::Receiver<TerminalEvent> {
        self.events.subscribe()
    }
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        for connection in &self.project_connections {
            connection.begin_shutdown();
        }
    }
    pub async fn create(
        self: &Arc<Self>,
        id: String,
        cwd: Option<PathBuf>,
        rows: u16,
        cols: u16,
    ) -> Result<TerminalSessionInfo, String> {
        self.create_with_target(id, cwd, rows, cols, TerminalTarget::Local)
            .await
    }
    pub async fn create_with_target(
        self: &Arc<Self>,
        id: String,
        cwd: Option<PathBuf>,
        rows: u16,
        cols: u16,
        target: TerminalTarget,
    ) -> Result<TerminalSessionInfo, String> {
        self.create_with_target_and_resources(id, cwd, rows, cols, target, Vec::new())
            .await
    }
    pub async fn create_with_target_and_resources(
        self: &Arc<Self>,
        id: String,
        cwd: Option<PathBuf>,
        rows: u16,
        cols: u16,
        target: TerminalTarget,
        resource_guards: Vec<InvocationResourceGuard>,
    ) -> Result<TerminalSessionInfo, String> {
        self.create_inner_with_project(id, cwd, rows, cols, target, None, None, resource_guards)
            .await
    }
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn create_with_shell_for_test(
        self: &Arc<Self>,
        id: String,
        cwd: Option<PathBuf>,
        rows: u16,
        cols: u16,
        shell: String,
    ) -> Result<TerminalSessionInfo, String> {
        self.create_inner_with_project(
            id,
            cwd,
            rows,
            cols,
            TerminalTarget::Local,
            Some(shell),
            None,
            Vec::new(),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn create_with_shell_and_resources_for_test(
        self: &Arc<Self>,
        id: String,
        cwd: Option<PathBuf>,
        shell: String,
        resource_guards: Vec<InvocationResourceGuard>,
    ) -> Result<TerminalSessionInfo, String> {
        self.create_inner_with_project(
            id,
            cwd,
            24,
            80,
            TerminalTarget::Local,
            Some(shell),
            None,
            resource_guards,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_inner_with_project(
        self: &Arc<Self>,
        id: String,
        cwd: Option<PathBuf>,
        rows: u16,
        cols: u16,
        target: TerminalTarget,
        shell: Option<String>,
        project_launch: Option<project_ssh::ProjectTerminalLaunch>,
        resource_guards: Vec<InvocationResourceGuard>,
    ) -> Result<TerminalSessionInfo, String> {
        let id = id.trim().to_string();
        if id.is_empty() {
            return Err("terminal id cannot be empty".to_string());
        }
        let _creation = self.creation_lock.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(
                "terminal service is shutting down; new sessions are not accepted".to_string(),
            );
        }
        if self.sessions.contains_key(&id) {
            return Err(format!("terminal '{id}' already exists"));
        }
        if project_launch.is_none()
            && self
                .project_connections
                .iter()
                .any(|connection| connection.master_terminal_id() == id)
        {
            return Err(
                "terminal id is reserved by an original project authentication preparation"
                    .to_string(),
            );
        }
        // Tokio can yield even for an uncontended lock under cooperative budget.
        // Acquire the original slot BEFORE spawn; no await may separate obtaining
        // the original Child from registering its owner and starting cleanup.
        let settlement = Arc::new(Mutex::new(None));
        let mut slot = Arc::clone(&settlement).lock_owned().await;
        #[cfg(test)]
        self.spawn_attempts.fetch_add(1, Ordering::Relaxed);
        let (session, reader) = TerminalSession::prepare(
            self,
            id.clone(),
            cwd,
            PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            },
            target,
            shell,
            settlement,
            project_launch,
            resource_guards,
        )?;
        self.sessions.insert(id.clone(), Arc::clone(&session));
        let setup_error = match spawn_reader(Arc::clone(&session), reader) {
            Ok(reader) => {
                session
                    .resources
                    .lock()
                    .map_err(|_| "new terminal resource lock poisoned".to_string())?
                    .reader = Some(reader);
                None
            }
            Err(error) => {
                let mut resources = session
                    .resources
                    .lock()
                    .map_err(|_| "new terminal resource lock poisoned".to_string())?;
                // No reader thread was created; its unstarted closure dropped the
                // reader FD. Cleanup still owns the original spawned child.
                resources.reader_result = Some(Ok(()));
                resources.creation_error = Some(error.clone());
                session.close_requested.store(true, Ordering::Release);
                Some(error)
            }
        };
        *slot = Some(start_settlement(Arc::clone(&session)));
        drop(slot);
        if let Some(error) = setup_error {
            return Err(error);
        }
        tracing::info!(terminal_id = %id, pid = session.info.pid, "terminal process started");
        Ok(session.info.clone())
    }
    pub fn list(&self) -> Vec<TerminalSessionInfo> {
        let authentication_ids = self
            .project_connections
            .iter()
            .map(|connection| connection.master_terminal_id().to_string())
            .collect::<std::collections::HashSet<_>>();
        let mut sessions = self
            .sessions
            .iter()
            .map(|entry| entry.value().info.clone())
            .filter(|info| !authentication_ids.contains(&info.id))
            .collect::<Vec<_>>();
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        sessions
    }
    pub fn contains(&self, id: &str) -> bool {
        self.sessions.contains_key(id)
    }
    pub async fn write(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_TERMINAL_WRITE_BYTES {
            return Err(format!(
                "terminal write payload is {} bytes; maximum is {MAX_TERMINAL_WRITE_BYTES}",
                bytes.len()
            ));
        }
        let session = self.session(id)?;
        if session.close_requested.load(Ordering::Acquire) {
            return Err("terminal is closing; input was not accepted".to_string());
        }
        session.write(bytes).await
    }
    pub async fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<(), String> {
        self.session(id)?.resize(rows, cols).await
    }
    pub async fn close(&self, id: &str) -> Result<bool, String> {
        if self
            .project_connections
            .iter()
            .any(|connection| connection.master_terminal_id() == id)
        {
            return Err("project authentication master belongs to its project; close the draft/project connection through its original owner".to_string());
        }
        self.close_session(id).await
    }
    async fn close_session(&self, id: &str) -> Result<bool, String> {
        let Some(session) = self.sessions.get(id).map(|entry| Arc::clone(entry.value())) else {
            return Ok(false);
        };
        session.close().await?;
        Ok(true)
    }
    /// Close human-operated local/mux sessions while retaining project masters
    /// used by outstanding Agent/file work. The original lifecycle can call
    /// this before waiting for host I/O, then call close_all after that work.
    pub async fn close_interactive_sessions(&self) -> Result<(), String> {
        // Local data protection: sealing creation first prevents a delayed
        // snapshot close from killing a replacement with a reused caller id.
        if !self.shutting_down.load(Ordering::Acquire) {
            return Err(
                "terminal shutdown admission must be closed before observing interactive sessions"
                    .to_string(),
            );
        }
        // First stop and observe ordinary terminals; they own mux-operation
        // guards. A master must outlive its actual file/command/slave users.
        let ids = {
            let _creation = self.creation_lock.lock().await;
            let authentication_ids = self
                .project_connections
                .iter()
                .map(|connection| connection.master_terminal_id().to_string())
                .collect::<std::collections::HashSet<_>>();
            self.sessions
                .iter()
                .filter(|entry| !authentication_ids.contains(entry.key()))
                .map(|entry| entry.key().clone())
                .collect::<Vec<_>>()
        };
        let mut errors = Vec::new();
        for id in ids {
            if let Err(error) = self.close(&id).await {
                errors.push(format!("{id}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "interactive terminal sessions remain owned and unsettled: {}",
                errors.join("; ")
            ))
        }
    }
    pub async fn close_all(&self) -> Result<(), String> {
        self.begin_shutdown();
        let mut errors = Vec::new();
        if let Err(error) = self.close_interactive_sessions().await {
            errors.push(error);
        }
        let owners = self
            .project_connections
            .iter()
            .map(|connection| connection.key().clone())
            .collect::<Vec<_>>();
        for owner in owners {
            if let Err(error) = self.close_project_ssh(&owner).await {
                errors.push(format!("project {owner}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "terminal sessions remain owned and unsettled: {}",
                errors.join("; ")
            ))
        }
    }
    fn session(&self, id: &str) -> Result<Arc<TerminalSession>, String> {
        self.sessions
            .get(id)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| format!("terminal '{id}' not found"))
    }
    fn publish(&self, event: TerminalEvent) {
        let _sent = self.events.send(event);
    }
    async fn finish_session(
        &self,
        session: &Arc<TerminalSession>,
        status: ExitStatus,
    ) -> Result<(), String> {
        // Serialize removal AND publication with create: old-instance cleanup must
        // never remove a replacement or publish a second terminal transition.
        let _creation = self.creation_lock.lock().await;
        let creation_error = session
            .resources
            .lock()
            .map_err(|_| "terminal resource lock poisoned".to_string())?
            .creation_error
            .clone();
        let reason = if let Some(error) = creation_error {
            TerminalExitReason::ReadFailed(error)
        } else if session.close_requested.load(Ordering::Acquire) {
            TerminalExitReason::Closed
        } else {
            TerminalExitReason::ProcessExited
        };
        let mut completed = session
            .completed
            .lock()
            .map_err(|_| "terminal completion lock poisoned".to_string())?;
        *completed = Some(status.clone());
        let mut resources = session
            .resources
            .lock()
            .map_err(|_| "terminal resource lock poisoned".to_string())?;
        resources.project_operation.take();
        resources.project_connection.take();
        resources.resource_guards.clear();
        drop(resources);
        if self
            .sessions
            .remove_if(&session.info.id, |_, current| Arc::ptr_eq(current, session))
            .is_some()
            && !session.exit_published.swap(true, Ordering::AcqRel)
        {
            self.publish(TerminalEvent::Exited {
                id: session.info.id.clone(),
                reason,
                exit_code: status.exit_code(),
                signal: status.signal().map(str::to_string),
            });
        }
        Ok(())
    }
}

fn validate_dimensions(rows: u16, cols: u16) -> Result<(), String> {
    if rows == 0 || cols == 0 {
        Err("terminal rows and columns must be greater than zero".to_string())
    } else {
        Ok(())
    }
}

fn start_settlement(session: Arc<TerminalSession>) -> JoinHandle<Result<(), String>> {
    tokio::spawn(async move {
        let original = Arc::clone(&session);
        let result = tokio::task::spawn_blocking(move || original.settle_original_resources())
            .await
            .map_err(|error| format!("terminal settlement task failed; owner retained: {error}"))
            .and_then(|result| result);
        match result {
            Ok(status) => {
                if let Some(service) = session.service.upgrade() {
                    service.finish_session(&session, status).await
                } else {
                    *session
                        .completed
                        .lock()
                        .map_err(|_| "terminal completion lock poisoned".to_string())? =
                        Some(status);
                    Ok(())
                }
            }
            Err(message) => {
                if let Some(service) = session.service.upgrade() {
                    service.publish(TerminalEvent::Issue {
                        id: session.info.id.clone(),
                        message: message.clone(),
                    });
                }
                Err(message)
            }
        }
    })
}

fn spawn_reader(
    session: Arc<TerminalSession>,
    mut reader: Box<dyn Read + Send>,
) -> Result<std::thread::JoinHandle<Result<(), String>>, String> {
    std::thread::Builder::new()
        .name(format!("terminal-reader-{}", session.info.id))
        .spawn(move || {
            let mut buffer = [0_u8; 8192];
            let result = loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break Ok(()),
                    Ok(read) => {
                        if let Some(service) = session.service.upgrade() {
                            service.publish(TerminalEvent::Output {
                                id: session.info.id.clone(),
                                bytes: buffer.iter().take(read).copied().collect(),
                            });
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => break Err(format!("terminal output read failed: {error}")),
                }
            };
            #[cfg(test)]
            {
                session.test.reader_ended.store(true, Ordering::Release);
                while session.test.hold_reader.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            #[cfg(test)]
            let result = if session.test.fail_reader.load(Ordering::Acquire) {
                Err("injected output reader failure".to_string())
            } else {
                result
            };
            result
        })
        .map_err(|error| {
            format!("failed to spawn terminal reader; original child retained for cleanup: {error}")
        })
}

#[cfg(test)]
fn injected_failure(counter: &AtomicUsize) -> bool {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
            Some(remaining.saturating_sub(1))
        })
        .is_ok_and(|previous| previous > 0)
}

#[cfg(windows)]
fn default_shell() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
}
#[cfg(not(windows))]
fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn interactive_pty_has_xterm_capabilities_at_original_cwd_and_keeps_exit_status()
    -> Result<(), String> {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = TerminalService::new();
        let mut receiver = service.subscribe();
        service
            .create_with_shell_for_test(
                "term-capabilities".to_string(),
                Some(directory.path().to_path_buf()),
                31,
                93,
                "/bin/sh".to_string(),
            )
            .await?;
        let session = service.session("term-capabilities")?;
        assert_eq!(session.info.target, TerminalTarget::Local);
        service.write("term-capabilities", b"printf 'TERM_CAP=<%s>\\nCWD_CAP=<%s>\\n' \"$TERM\" \"$PWD\"; stty size; exit 13\r").await?;
        let (output, code) = tokio::time::timeout(Duration::from_secs(10), async {
            let mut output = Vec::new();
            loop {
                match receiver.recv().await {
                    Ok(TerminalEvent::Output { id, bytes }) if id == "term-capabilities" => {
                        output.extend(bytes)
                    }
                    Ok(TerminalEvent::Exited { id, exit_code, .. })
                        if id == "term-capabilities" =>
                    {
                        break Ok::<_, String>((output, exit_code));
                    }
                    Ok(TerminalEvent::Issue { message, .. }) => break Err(message),
                    Ok(_) => {}
                    Err(error) => break Err(error.to_string()),
                }
            }
        })
        .await
        .map_err(|_| "original PTY owner did not settle".to_string())??;
        assert_eq!(code, 13);
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("TERM_CAP=<xterm-256color>"), "{output}");
        assert!(output.contains("31 93"), "{output}");
        let cwd = directory
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        assert!(
            output.contains(&format!("CWD_CAP=<{}>", cwd.display())),
            "{output}"
        );
        assert!(!service.contains("term-capabilities"));
        Ok(())
    }

    #[tokio::test]
    async fn oversized_writes_are_rejected_before_session_lookup() {
        let service = TerminalService::new();
        let bytes = vec![0_u8; MAX_TERMINAL_WRITE_BYTES.saturating_add(1)];
        let error = service.write("missing", &bytes).await.err();
        assert!(error.is_some_and(|error| error.contains("maximum")));
    }

    #[tokio::test]
    async fn slow_subscriber_observes_bounded_lag() {
        let service = TerminalService::with_event_capacity(2);
        let mut receiver = service.subscribe();
        for value in 0_u8..4 {
            service.publish(TerminalEvent::Output {
                id: "terminal".to_string(),
                bytes: vec![value],
            });
        }
        assert!(matches!(
            receiver.recv().await,
            Err(broadcast::error::RecvError::Lagged(skipped)) if skipped > 0
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_exit_removes_session_and_emits_one_exit() -> Result<(), String> {
        let service = TerminalService::new();
        let mut receiver = service.subscribe();
        service
            .create("lifecycle".to_string(), None, 24, 80)
            .await?;
        service
            .write("lifecycle", b"printf terminal-ready; exit\r")
            .await?;

        let mut output = Vec::new();
        let exits = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                match receiver.recv().await {
                    Ok(TerminalEvent::Output { bytes, .. }) => output.extend(bytes),
                    Ok(TerminalEvent::Exited { .. }) => break Ok::<usize, String>(1),
                    Ok(TerminalEvent::Issue { message, .. }) => break Err(message),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        break Err("terminal event channel closed".to_string());
                    }
                }
            }
        })
        .await
        .map_err(|_| "terminal did not exit".to_string())??;

        assert!(!service.contains("lifecycle"));
        assert_eq!(exits, 1);
        assert!(String::from_utf8_lossy(&output).contains("terminal-ready"));
        assert!(!service.close("lifecycle").await?);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_create_starts_only_the_winning_child() -> Result<(), String> {
        let service = TerminalService::new();
        let left = service.create("same-id".to_string(), None, 24, 80);
        let right = service.create("same-id".to_string(), None, 24, 80);
        let (left, right) = tokio::join!(left, right);
        assert_ne!(left.is_ok(), right.is_ok());
        assert_eq!(service.list().len(), 1);
        assert_eq!(service.spawn_attempts.load(Ordering::Relaxed), 1);
        service.close_all().await?;
        Ok(())
    }

    #[test]
    fn ssh_target_uses_literal_argv_without_overriding_unspecified_config() -> Result<(), String> {
        let target = TerminalTarget::Ssh {
            host: " existing-alias ".to_string(),
            user: None,
            port: None,
        }
        .normalized()?;
        let command = command_for_target(&target, None, None);
        let args = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, ["ssh", "-tt", "--", "existing-alias"]);
        let target = TerminalTarget::Ssh {
            host: "host.example".to_string(),
            user: Some("name; literal".to_string()),
            port: Some(2222),
        }
        .normalized()?;
        let command = command_for_target(&target, None, None);
        let args = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "ssh",
                "-tt",
                "-l",
                "name; literal",
                "-p",
                "2222",
                "--",
                "host.example"
            ]
        );
        Ok(())
    }

    #[test]
    fn invalid_ssh_targets_and_zero_dimensions_are_rejected() {
        for host in [
            "",
            "  ",
            "-oProxyCommand=x",
            "someone@host",
            "ssh://host",
            "host\nnext",
        ] {
            assert!(
                TerminalTarget::Ssh {
                    host: host.to_string(),
                    user: None,
                    port: None
                }
                .normalized()
                .is_err()
            );
        }
        assert!(
            TerminalTarget::Ssh {
                host: "alias".to_string(),
                user: None,
                port: Some(0)
            }
            .normalized()
            .is_err()
        );
        assert!(validate_dimensions(0, 80).is_err());
        assert!(validate_dimensions(24, 0).is_err());
    }

    #[cfg(unix)]
    async fn local_test_session(
        service: &Arc<TerminalService>,
        id: &str,
    ) -> Result<Arc<TerminalSession>, String> {
        service
            .create_with_shell_for_test(id.to_string(), None, 24, 80, "/bin/sh".to_string())
            .await?;
        service.session(id)
    }

    #[cfg(unix)]
    async fn wait_for_output(
        receiver: &mut broadcast::Receiver<TerminalEvent>,
        marker: &str,
    ) -> Result<(), String> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut output = Vec::new();
            loop {
                match receiver.recv().await {
                    Ok(TerminalEvent::Output { bytes, .. }) => {
                        output.extend(bytes);
                        if String::from_utf8_lossy(&output).contains(marker) {
                            return Ok(());
                        }
                    }
                    Ok(TerminalEvent::Issue { message, .. }) => return Err(message),
                    Ok(TerminalEvent::Exited { .. }) => {
                        return Err("process exited before test marker".to_string());
                    }
                    Err(error) => return Err(format!("test observation failed: {error}")),
                }
            }
        })
        .await
        .map_err(|_| "test marker did not arrive".to_string())?
    }

    #[cfg(unix)]
    async fn wait_for_reader(session: &TerminalSession) -> Result<(), String> {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !session.test.reader_ended.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| "reader did not reach EOF".to_string())
    }

    // Ensure a failing test never leaves its deliberately held reader blocked.
    #[cfg(unix)]
    struct ReleaseReader(Arc<TerminalSession>);
    #[cfg(unix)]
    impl Drop for ReleaseReader {
        fn drop(&mut self) {
            self.0.test.hold_reader.store(false, Ordering::Release);
            self.0.close_requested.store(true, Ordering::Release);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invocation_guard_survives_cancelled_observer_until_original_reader_join()
    -> Result<(), String> {
        struct DropCounter(Arc<AtomicUsize>);
        impl Drop for DropCounter {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }
        let service = TerminalService::new();
        let dropped = Arc::new(AtomicUsize::new(0));
        service
            .create_inner_with_project(
                "resource-retention".into(),
                None,
                24,
                80,
                TerminalTarget::Local,
                Some("/bin/sh".into()),
                None,
                vec![InvocationResourceGuard::new(DropCounter(Arc::clone(
                    &dropped,
                )))],
            )
            .await?;
        let session = service.session("resource-retention")?;
        let _release = ReleaseReader(Arc::clone(&session));
        session.test.hold_reader.store(true, Ordering::Release);
        let closing = Arc::clone(&service);
        let observer = tokio::spawn(async move { closing.close("resource-retention").await });
        wait_for_reader(&session).await?;
        observer.abort();
        let _observation = observer.await;
        assert_eq!(dropped.load(Ordering::Acquire), 0);
        assert!(service.contains("resource-retention"));
        session.test.hold_reader.store(false, Ordering::Release);
        service.close("resource-retention").await?;
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hup_ignoring_original_child_is_killed_waited_and_reader_joined() -> Result<(), String>
    {
        let service = TerminalService::new();
        let mut receiver = service.subscribe();
        let session = local_test_session(&service, "ignore-hup").await?;
        service.write("ignore-hup", b"stty -echo; trap '' HUP; printf '\\162\\145\\141\\144\\171\\055\\150\\165\\160\\n'\r").await?;
        wait_for_output(&mut receiver, "ready-hup").await?;
        assert!(service.close("ignore-hup").await?);
        assert_eq!(session.test.kill_attempts.load(Ordering::Acquire), 1);
        assert_eq!(session.test.wait_attempts.load(Ordering::Acquire), 1);
        assert!(session.test.reader_ended.load(Ordering::Acquire));
        assert!(session.is_completed()?);
        assert!(!service.contains("ignore-hup"));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn original_kill_failure_retains_identity_until_retry_succeeds() -> Result<(), String> {
        let service = TerminalService::new();
        let session = local_test_session(&service, "kill-retry").await?;
        session.test.fail_kill.store(1, Ordering::Release);
        let error = service
            .close("kill-retry")
            .await
            .err()
            .ok_or_else(|| "injected kill unexpectedly succeeded".to_string())?;
        assert!(error.contains("kill failure"));
        assert!(Arc::ptr_eq(&session, &service.session("kill-retry")?));
        assert!(!session.is_completed()?);
        assert!(service.close("kill-retry").await?);
        assert_eq!(session.test.kill_attempts.load(Ordering::Acquire), 2);
        assert_eq!(session.test.wait_attempts.load(Ordering::Acquire), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn original_wait_failure_preserves_actual_nonzero_status_for_retry() -> Result<(), String>
    {
        let service = TerminalService::new();
        let mut receiver = service.subscribe();
        let session = local_test_session(&service, "wait-retry").await?;
        session.test.fail_wait.store(1, Ordering::Release);
        service.write("wait-retry", b"exit 37\r").await?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(receiver.recv().await, Ok(TerminalEvent::Issue { .. })) {
                    break;
                }
            }
        })
        .await
        .map_err(|_| "injected wait failure was not observed".to_string())?;
        assert!(service.contains("wait-retry"));
        assert!(service.close("wait-retry").await.is_err()); // observe original failed handle
        assert!(service.close("wait-retry").await?);
        let status = session
            .completed
            .lock()
            .map_err(|_| "completion poisoned".to_string())?;
        assert_eq!(status.as_ref().map(ExitStatus::exit_code), Some(37));
        assert_eq!(session.test.wait_attempts.load(Ordering::Acquire), 2);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_close_keeps_original_wait_and_unjoined_reader_for_retry()
    -> Result<(), String> {
        let service = TerminalService::new();
        let session = local_test_session(&service, "cancel-close").await?;
        let _release = ReleaseReader(Arc::clone(&session));
        session.test.hold_reader.store(true, Ordering::Release);
        let closing = Arc::clone(&service);
        let observer = tokio::spawn(async move { closing.close("cancel-close").await });
        wait_for_reader(&session).await?;
        observer.abort();
        let cancelled = observer.await;
        assert!(cancelled.is_err_and(|error| error.is_cancelled()));
        assert!(service.contains("cancel-close"));
        assert!(!session.is_completed()?);
        session.test.hold_reader.store(false, Ordering::Release);
        assert!(service.close("cancel-close").await?);
        assert_eq!(session.test.kill_attempts.load(Ordering::Acquire), 1);
        assert_eq!(session.test.wait_attempts.load(Ordering::Acquire), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_timeout_retains_original_owner_and_rejects_late_create() -> Result<(), String>
    {
        let mut service = TerminalService::new();
        Arc::get_mut(&mut service)
            .ok_or_else(|| "new service unexpectedly shared".to_string())?
            .observation_timeout = Duration::from_millis(40);
        let session = local_test_session(&service, "slow-reader").await?;
        let _release = ReleaseReader(Arc::clone(&session));
        session.test.hold_reader.store(true, Ordering::Release);
        assert!(service.close_all().await.is_err());
        assert!(Arc::ptr_eq(&session, &service.session("slow-reader")?));
        assert!(!session.is_completed()?);
        let attempts = service.spawn_attempts.load(Ordering::Acquire);
        assert!(
            service
                .create("late".to_string(), None, 24, 80)
                .await
                .is_err()
        );
        assert_eq!(service.spawn_attempts.load(Ordering::Acquire), attempts);
        session.test.hold_reader.store(false, Ordering::Release);
        // Use the original owner; this observation may still need its small
        // budget more than once on a loaded host, never relax the production bound.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if service.close_all().await.is_ok() {
                    break;
                }
            }
        })
        .await
        .map_err(|_| "original shutdown did not settle after reader release".to_string())?;
        assert!(session.is_completed()?);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cooperative_budget_cancellation_never_leaves_an_unregistered_child()
    -> Result<(), String> {
        // Exercise the real budget, not a fake sleep inside create. No task yield
        // occurs between exhausting it and dropping the first Pending create.
        let service = TerminalService::new();
        for _ in 0..127 {
            tokio::task::consume_budget().await;
        }
        let mut create = Box::pin(service.create_with_shell_for_test(
            "budget-cancel".to_string(),
            None,
            24,
            80,
            "/bin/sh".to_string(),
        ));
        let pending = std::future::poll_fn(|context| {
            std::task::Poll::Ready(matches!(
                std::future::Future::poll(create.as_mut(), context),
                std::task::Poll::Pending
            ))
        })
        .await;
        drop(create);
        assert!(
            pending,
            "test must actually cancel a budget-suspended create"
        );
        assert!(!service.contains("budget-cancel"));
        // prepare increments only immediately before spawning; no invisible
        // child may have passed that boundary when the original create yields.
        assert_eq!(service.spawn_attempts.load(Ordering::Acquire), 0);
        service.close_all().await?;
        Ok(())
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn irrecoverable_reader_error_is_visible_and_retry_cannot_erase_it() -> Result<(), String>
    {
        let service = TerminalService::new();
        let mut receiver = service.subscribe();
        let session = local_test_session(&service, "reader-error").await?;
        session.test.fail_reader.store(true, Ordering::Release);
        service.write("reader-error", b"exit 19\r").await?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match receiver.recv().await {
                    Ok(TerminalEvent::Issue { .. }) => return Ok::<(), String>(()),
                    Ok(TerminalEvent::Exited { .. }) => {
                        return Err("reader failure was reported as exit".to_string());
                    }
                    Ok(TerminalEvent::Output { .. }) => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
        })
        .await
        .map_err(|_| "reader failure was not visible".to_string())??;
        assert!(service.close("reader-error").await.is_err());
        assert!(service.close("reader-error").await.is_err());
        assert!(Arc::ptr_eq(&session, &service.session("reader-error")?));
        assert!(!session.is_completed()?);
        assert_eq!(session.test.wait_attempts.load(Ordering::Acquire), 1);
        Ok(())
    }

    #[test]
    fn session_metadata_round_trips_the_actual_target_without_invented_fields() -> Result<(), String>
    {
        let info = TerminalSessionInfo {
            id: "id".to_string(),
            pid: 42,
            target: TerminalTarget::Ssh {
                host: "alias".to_string(),
                user: None,
                port: None,
            },
        };
        let json = serde_json::to_value(&info).map_err(|error| error.to_string())?;
        assert!(json.get("cwd").is_none());
        assert!(json.get("created_at").is_none());
        assert_eq!(
            serde_json::from_value::<TerminalSessionInfo>(json)
                .map_err(|error| error.to_string())?,
            info
        );
        Ok(())
    }
}
