//! Application-owned interactive terminal sessions shared by every surface.

use dashmap::DashMap;
use portable_pty::{CommandBuilder, PtyPair, PtySize};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::{Mutex, broadcast};

pub const MAX_TERMINAL_WRITE_BYTES: usize = 64 * 1024;
const DEFAULT_EVENT_CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    Output {
        id: String,
        bytes: Vec<u8>,
    },
    Exited {
        id: String,
        reason: TerminalExitReason,
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
}

struct TerminalSession {
    info: TerminalSessionInfo,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child_killer: Mutex<Box<dyn portable_pty::ChildKiller + Send>>,
    exit_published: AtomicBool,
}

type PreparedTerminal = (
    Arc<TerminalSession>,
    Box<dyn Read + Send>,
    Box<dyn portable_pty::Child + Send + Sync>,
);

impl TerminalSession {
    fn prepare(
        id: String,
        cwd: Option<PathBuf>,
        rows: u16,
        cols: u16,
        shell: Option<String>,
    ) -> Result<PreparedTerminal, String> {
        validate_dimensions(rows, cols)?;
        let pair: PtyPair = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("failed to open PTY: {error}"))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("failed to clone PTY reader: {error}"))?;

        let mut command = CommandBuilder::new(shell.unwrap_or_else(default_shell));
        if let Some(cwd) = cwd {
            command.cwd(cwd);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("failed to spawn terminal shell: {error}"))?;
        drop(pair.slave);
        let pid = child.process_id().unwrap_or(0);
        let mut child_killer = child.clone_killer();
        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                let cleanup = child_killer.kill().err();
                return Err(match cleanup {
                    Some(cleanup) => format!(
                        "failed to take PTY writer: {error}; child cleanup failed: {cleanup}"
                    ),
                    None => format!("failed to take PTY writer: {error}"),
                });
            }
        };
        Ok((
            Arc::new(Self {
                info: TerminalSessionInfo { id, pid },
                master: Mutex::new(pair.master),
                writer: Mutex::new(writer),
                child_killer: Mutex::new(child_killer),
                exit_published: AtomicBool::new(false),
            }),
            reader,
            child,
        ))
    }

    async fn write(&self, bytes: &[u8]) -> Result<(), String> {
        let mut writer = self.writer.lock().await;
        writer
            .write_all(bytes)
            .map_err(|error| format!("terminal write failed: {error}"))?;
        writer
            .flush()
            .map_err(|error| format!("terminal flush failed: {error}"))
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

    async fn kill(&self) -> Result<(), String> {
        self.child_killer
            .lock()
            .await
            .kill()
            .map_err(|error| format!("terminal close failed: {error}"))
    }
}

pub struct TerminalService {
    sessions: DashMap<String, Arc<TerminalSession>>,
    events: broadcast::Sender<TerminalEvent>,
    creation_lock: Mutex<()>,
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
            #[cfg(test)]
            spawn_attempts: AtomicUsize::new(0),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TerminalEvent> {
        self.events.subscribe()
    }

    pub async fn create(
        self: &Arc<Self>,
        id: String,
        cwd: Option<PathBuf>,
        rows: u16,
        cols: u16,
    ) -> Result<TerminalSessionInfo, String> {
        self.create_inner(id, cwd, rows, cols, None).await
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
        self.create_inner(id, cwd, rows, cols, Some(shell)).await
    }

    async fn create_inner(
        self: &Arc<Self>,
        id: String,
        cwd: Option<PathBuf>,
        rows: u16,
        cols: u16,
        shell: Option<String>,
    ) -> Result<TerminalSessionInfo, String> {
        let id = id.trim().to_string();
        if id.is_empty() {
            return Err("terminal id cannot be empty".to_string());
        }
        let _creation_guard = self.creation_lock.lock().await;
        if self.sessions.contains_key(&id) {
            return Err(format!("terminal '{id}' already exists"));
        }
        #[cfg(test)]
        self.spawn_attempts.fetch_add(1, Ordering::Relaxed);
        let (session, reader, child) =
            TerminalSession::prepare(id.clone(), cwd, rows, cols, shell)?;
        let info = session.info.clone();
        self.sessions.insert(id.clone(), Arc::clone(&session));
        if let Err(error) = spawn_reader(Arc::downgrade(self), Arc::clone(&session), reader, child)
        {
            self.remove_exact(&session);
            let cleanup = session.kill().await.err();
            return Err(match cleanup {
                Some(cleanup) => format!("{error}; child cleanup failed: {cleanup}"),
                None => error,
            });
        }
        tracing::info!(terminal_id = %id, pid = info.pid, "terminal session created");
        Ok(info)
    }

    pub fn list(&self) -> Vec<TerminalSessionInfo> {
        let mut sessions = self
            .sessions
            .iter()
            .map(|entry| entry.value().info.clone())
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.id.cmp(&right.id));
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
        session.write(bytes).await
    }

    pub async fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<(), String> {
        let session = self.session(id)?;
        session.resize(rows, cols).await
    }

    pub async fn close(&self, id: &str) -> Result<bool, String> {
        let Some(session) = self.sessions.get(id).map(|entry| Arc::clone(entry.value())) else {
            return Ok(false);
        };
        session.kill().await?;
        self.finish_session(&session, TerminalExitReason::Closed);
        Ok(true)
    }

    pub async fn close_all(&self) -> Result<(), String> {
        let ids = self
            .sessions
            .iter()
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
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
                "failed to close terminal sessions: {}",
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

    fn remove_exact(&self, session: &Arc<TerminalSession>) {
        self.sessions
            .remove_if(&session.info.id, |_, current| Arc::ptr_eq(current, session));
    }

    fn publish(&self, event: TerminalEvent) {
        let _sent = self.events.send(event);
    }

    fn finish_session(&self, session: &Arc<TerminalSession>, reason: TerminalExitReason) {
        self.remove_exact(session);
        if !session.exit_published.swap(true, Ordering::AcqRel) {
            self.publish(TerminalEvent::Exited {
                id: session.info.id.clone(),
                reason,
            });
        }
    }
}

fn validate_dimensions(rows: u16, cols: u16) -> Result<(), String> {
    if rows == 0 || cols == 0 {
        return Err("terminal rows and columns must be greater than zero".to_string());
    }
    Ok(())
}

fn spawn_reader(
    service: Weak<TerminalService>,
    session: Arc<TerminalSession>,
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
) -> Result<(), String> {
    let thread_name = format!("terminal-reader-{}", session.info.id);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let mut buffer = [0_u8; 8192];
            let mut reason = loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break TerminalExitReason::ProcessExited,
                    Ok(read) => {
                        if let Some(service) = service.upgrade() {
                            if !session.exit_published.load(Ordering::Acquire) {
                                service.publish(TerminalEvent::Output {
                                    id: session.info.id.clone(),
                                    bytes: buffer.iter().take(read).copied().collect(),
                                });
                            }
                        } else {
                            return;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => break TerminalExitReason::ReadFailed(error.to_string()),
                }
            };
            if let TerminalExitReason::ReadFailed(message) = &mut reason
                && let Err(error) = child.kill()
            {
                message.push_str(&format!("; child cleanup failed: {error}"));
            }
            if let Err(error) = child.wait() {
                reason = TerminalExitReason::ReadFailed(format!(
                    "{reason:?}; child wait failed: {error}"
                ));
            }
            if let Some(service) = service.upgrade() {
                service.finish_session(&session, reason);
            }
        })
        .map(|_| ())
        .map_err(|error| format!("failed to spawn terminal reader: {error}"))
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
}
