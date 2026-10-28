//! Two real disjoint directories with the same visible namespace. This is a
//! routing fixture, not SSH acceptance or a substitute file implementation.

use super::store::TaskRuntimeStore;
use super::types::{AttendedMode, DomainProfile, TaskRunStatus};
use echo_agent::error::{ReactError, Result};
use echo_agent::sandbox::{
    CommandKind, ExecutionResult, IsolationLevel, SandboxCommand, SandboxExecutor,
};
use echo_agent::tools::files::LocalFileSystem;
use echo_agent::tools::filesystem::{FileEntry, FileMetadata, FileSystem};
use futures::future::BoxFuture;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Namespace {
    visible: PathBuf,
    physical: PathBuf,
}

impl Namespace {
    fn physical(&self, path: &Path) -> Result<PathBuf> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.visible.join(path)
        };
        let suffix = path
            .strip_prefix(&self.visible)
            .map_err(|_| ReactError::Other("fixture target escaped its namespace".to_string()))?;
        Ok(self.physical.join(suffix))
    }

    fn visible(&self, path: &Path) -> Result<PathBuf> {
        let suffix = path.strip_prefix(&self.physical).map_err(|_| {
            ReactError::Other("fixture physical path escaped its namespace".to_string())
        })?;
        Ok(self.visible.join(suffix))
    }
}

struct TargetFiles(
    Namespace,
    Arc<Mutex<Vec<usize>>>,
    Arc<std::sync::atomic::AtomicBool>,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::atomic::AtomicBool>,
);

struct TrackedFileReader {
    file: std::fs::File,
    requests: Arc<Mutex<Vec<usize>>>,
    fail_after: Arc<std::sync::atomic::AtomicUsize>,
    consumed: usize,
}

impl std::io::Read for TrackedFileReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.requests
            .lock()
            .map_err(|_| std::io::Error::other("reader observation lock failed"))?
            .push(buffer.len());
        if buffer.len() > 64 * 1024 {
            return Err(std::io::Error::other("unbounded artifact read request"));
        }
        let limit = self.fail_after.load(std::sync::atomic::Ordering::Acquire);
        if self.consumed >= limit {
            return Err(std::io::Error::other("fixture streaming read failed"));
        }
        let available = buffer.len().min(limit.saturating_sub(self.consumed));
        let chunk = buffer
            .get_mut(..available)
            .ok_or_else(|| std::io::Error::other("invalid read bound"))?;
        let read = std::io::Read::read(&mut self.file, chunk)?;
        self.consumed = self.consumed.saturating_add(read);
        Ok(read)
    }
}

impl FileSystem for TargetFiles {
    fn current_dir(&self) -> Result<PathBuf> {
        Ok(self.0.visible.join("project"))
    }
    fn canonicalize(&self, p: &Path) -> Result<PathBuf> {
        self.0
            .visible(&LocalFileSystem.canonicalize(&self.0.physical(p)?)?)
    }
    fn metadata(&self, p: &Path) -> Result<FileMetadata> {
        if self.4.load(std::sync::atomic::Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "fixture target metadata is unavailable",
            )
            .into());
        }
        LocalFileSystem.metadata(&self.0.physical(p)?)
    }
    fn symlink_metadata(&self, p: &Path) -> Result<FileMetadata> {
        LocalFileSystem.symlink_metadata(&self.0.physical(p)?)
    }
    fn read(&self, p: &Path) -> Result<Vec<u8>> {
        if self.2.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ReactError::Other(
                "whole-file read is forbidden by this fixture".to_string(),
            ));
        }
        LocalFileSystem.read(&self.0.physical(p)?)
    }
    fn open_read(&self, path: &Path) -> Result<Box<dyn std::io::Read + Send>> {
        Ok(Box::new(TrackedFileReader {
            file: std::fs::File::open(self.0.physical(path)?)?,
            requests: self.1.clone(),
            fail_after: self.3.clone(),
            consumed: 0,
        }))
    }
    fn read_dir(&self, p: &Path) -> Result<Vec<FileEntry>> {
        LocalFileSystem
            .read_dir(&self.0.physical(p)?)?
            .into_iter()
            .map(|mut entry| {
                entry.path = self.0.visible(&entry.path)?;
                Ok(entry)
            })
            .collect()
    }
    fn create_dir_all(&self, p: &Path) -> Result<()> {
        LocalFileSystem.create_dir_all(&self.0.physical(p)?)
    }
    fn create_dir(&self, p: &Path) -> Result<()> {
        LocalFileSystem.create_dir(&self.0.physical(p)?)
    }
    fn create_file(&self, p: &Path) -> Result<()> {
        LocalFileSystem.create_file(&self.0.physical(p)?)
    }
    fn write(&self, p: &Path, bytes: &[u8]) -> Result<()> {
        LocalFileSystem.write(&self.0.physical(p)?, bytes)
    }
    fn append(&self, p: &Path, bytes: &[u8]) -> Result<()> {
        LocalFileSystem.append(&self.0.physical(p)?, bytes)
    }
    fn rename(&self, a: &Path, b: &Path) -> Result<()> {
        LocalFileSystem.rename(&self.0.physical(a)?, &self.0.physical(b)?)
    }
    fn remove_file(&self, p: &Path) -> Result<()> {
        LocalFileSystem.remove_file(&self.0.physical(p)?)
    }
    fn remove_dir(&self, p: &Path) -> Result<()> {
        LocalFileSystem.remove_dir(&self.0.physical(p)?)
    }
    fn remove_dir_all(&self, p: &Path) -> Result<()> {
        LocalFileSystem.remove_dir_all(&self.0.physical(p)?)
    }
    fn set_mode(&self, p: &Path, mode: u32) -> Result<()> {
        LocalFileSystem.set_mode(&self.0.physical(p)?, mode)
    }
    fn atomic_write(&self, p: &Path, bytes: &[u8]) -> Result<()> {
        LocalFileSystem.atomic_write(&self.0.physical(p)?, bytes)
    }
    fn checkpoint(&self, _p: &Path) -> Result<Option<String>> {
        Ok(None)
    }
}

struct TargetExecutor {
    namespace: Namespace,
    calls: Arc<Mutex<Vec<PathBuf>>>,
}

impl SandboxExecutor for TargetExecutor {
    fn name(&self) -> &str {
        "project-routing-fixture"
    }
    fn isolation_level(&self) -> IsolationLevel {
        IsolationLevel::None
    }
    fn is_available(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
    }
    fn execute(&self, command: SandboxCommand) -> BoxFuture<'_, Result<ExecutionResult>> {
        Box::pin(async move {
            let directory = command
                .working_dir
                .ok_or_else(|| ReactError::Other("actual invocation cwd missing".to_string()))?;
            self.calls
                .lock()
                .map_err(|_| ReactError::Other("fixture calls lock poisoned".to_string()))?
                .push(directory.clone());
            let (program, mut args) = match command.kind {
                CommandKind::Program { program, args } => (program, args),
                CommandKind::Shell(script) => ("sh".to_string(), vec!["-c".to_string(), script]),
                _ => {
                    return Err(ReactError::Other(
                        "routing fixture requires a program or shell".to_string(),
                    ));
                }
            };
            // git reports physical paths; translate only paths in this fixture
            // namespace so production worktree algorithms see target paths.
            for arg in &mut args {
                if Path::new(arg).starts_with(&self.namespace.visible) {
                    *arg = self
                        .namespace
                        .physical(Path::new(arg))?
                        .to_string_lossy()
                        .to_string();
                }
            }
            let output = std::process::Command::new(program)
                .args(args)
                .current_dir(self.namespace.physical(&directory)?)
                .output()?;
            let stdout = String::from_utf8_lossy(&output.stdout).replace(
                &self.namespace.physical.to_string_lossy().to_string(),
                &self.namespace.visible.to_string_lossy(),
            );
            Ok(ExecutionResult {
                exit_code: output.status.code().unwrap_or(1),
                stdout_bytes: u64::try_from(stdout.len()).unwrap_or(u64::MAX),
                stderr_bytes: u64::try_from(output.stderr.len()).unwrap_or(u64::MAX),
                stdout,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                duration: std::time::Duration::ZERO,
                sandbox_type: self.name().to_string(),
                timed_out: false,
                cancelled: false,
                output_truncated: false,
            })
        })
    }
}

pub(crate) struct EnvironmentFixture {
    _temp: tempfile::TempDir,
    pub(crate) visible_root: PathBuf,
    pub(crate) target_root: PathBuf,
    pub(crate) files: Arc<dyn FileSystem>,
    pub(crate) executor: Arc<dyn SandboxExecutor>,
    pub(crate) calls: Arc<Mutex<Vec<PathBuf>>>,
    pub(crate) data_root: PathBuf,
    pub(crate) read_requests: Arc<Mutex<Vec<usize>>>,
    pub(crate) reject_whole_reads: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) fail_after: Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) fail_metadata: Arc<std::sync::atomic::AtomicBool>,
}

impl EnvironmentFixture {
    pub(crate) fn new() -> std::result::Result<Self, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let base = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let visible = base.join("same-name");
        let physical = base.join("target");
        let visible_root = visible.join("project");
        let target_root = physical.join("project");
        std::fs::create_dir_all(&visible_root).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&target_root).map_err(|error| error.to_string())?;
        let namespace = Namespace { visible, physical };
        let calls = Arc::new(Mutex::new(Vec::new()));
        let read_requests = Arc::new(Mutex::new(Vec::new()));
        let reject_whole_reads = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fail_after = Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let fail_metadata = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let files = Arc::new(TargetFiles(
            namespace.clone(),
            read_requests.clone(),
            reject_whole_reads.clone(),
            fail_after.clone(),
            fail_metadata.clone(),
        ));
        let executor = Arc::new(TargetExecutor {
            namespace,
            calls: calls.clone(),
        });
        let data_root = temp.path().join("product-data/tasks");
        Ok(Self {
            _temp: temp,
            visible_root,
            target_root,
            files,
            executor,
            calls,
            data_root,
            read_requests,
            reject_whole_reads,
            fail_after,
            fail_metadata,
        })
    }

    pub(crate) fn open(
        &self,
        host: &str,
        revision: u64,
    ) -> std::result::Result<TaskRuntimeStore, String> {
        let store = TaskRuntimeStore::open_for_workspace(&self.data_root, "workspace")
            .map_err(|error| error.to_string())?;
        store
            .bind_project_environment(
                "workspace",
                crate::workspace::ProjectLocation::Ssh {
                    host: host.to_string(),
                    user: Some("echo".to_string()),
                    port: Some(22),
                    path: self.visible_root.to_string_lossy().to_string(),
                },
                revision,
                self.files.clone(),
                self.executor.clone(),
            )
            .map_err(|error| error.to_string())?;
        Ok(store)
    }
}

#[tokio::test]
async fn run_created_reopen_rejects_same_path_on_another_host_without_events()
-> std::result::Result<(), String> {
    let fixture = EnvironmentFixture::new()?;
    let store = fixture.open("original.invalid", 4)?;
    store
        .create_run_with_profile(
            "bound-run",
            "workspace",
            "conversation",
            "root",
            DomainProfile::General,
            "verify location",
            "test",
            AttendedMode::Attended,
            super::types::TaskRunExecutionProfile::orchestrated(
                super::types::RunPlanPolicy::AllowDirect,
            ),
        )
        .map_err(|error| error.to_string())?;
    store
        .transition_run("bound-run", TaskRunStatus::Running)
        .map_err(|error| error.to_string())?;
    store
        .transition_run("bound-run", TaskRunStatus::Paused)
        .map_err(|error| error.to_string())?;
    let binding = store
        .get_run_state("bound-run")
        .map_err(|error| error.to_string())?
        .and_then(|state| state.project_binding)
        .ok_or_else(|| "RunCreated binding missing".to_string())?;
    assert_eq!(Some(binding.clone()), store.project_execution_binding());
    let before = store
        .list_events("bound-run", 0)
        .map_err(|error| error.to_string())?
        .len();
    drop(store);
    let wrong = fixture.open("different.invalid", 4)?;
    let error = wrong
        .resume_task_run("bound-run")
        .err()
        .ok_or_else(|| "wrong-host resume was accepted".to_string())?;
    assert!(error.to_string().contains("project location or revision"));
    assert_eq!(
        before,
        wrong
            .list_events("bound-run", 0)
            .map_err(|error| error.to_string())?
            .len()
    );
    drop(wrong);
    let same = fixture.open("original.invalid", 4)?;
    assert_eq!(
        Some(binding),
        same.get_run_state("bound-run")
            .map_err(|error| error.to_string())?
            .and_then(|state| state.project_binding)
    );
    assert!(same.validate_run_project_binding("bound-run").is_ok());
    same.resume_task_run("bound-run")
        .map_err(|error| error.to_string())?;
    same.transition_run("bound-run", TaskRunStatus::Paused)
        .map_err(|error| error.to_string())?;
    drop(same);
    let relinked = fixture.open("original.invalid", 5)?;
    assert!(relinked.resume_task_run("bound-run").is_err());
    Ok(())
}

#[tokio::test]
async fn original_worktree_lifecycle_uses_target_git_and_keeps_invocation_checkout()
-> std::result::Result<(), String> {
    use echo_agent::subagent::IsolationProvider;
    let fixture = EnvironmentFixture::new()?;
    for (root, contents) in [
        (&fixture.visible_root, "LOCAL"),
        (&fixture.target_root, "TARGET"),
    ] {
        super::worktree::run_git(root, &["init", "-b", "main"])
            .map_err(|error| error.to_string())?;
        super::worktree::run_git(root, &["config", "user.name", "routing-test"])
            .map_err(|error| error.to_string())?;
        super::worktree::run_git(root, &["config", "user.email", "routing@local"])
            .map_err(|error| error.to_string())?;
        std::fs::write(root.join("same.txt"), contents).map_err(|error| error.to_string())?;
        super::worktree::run_git(root, &["add", "same.txt"]).map_err(|error| error.to_string())?;
        super::worktree::run_git(
            root,
            &["-c", "commit.gpgsign=false", "commit", "-m", "fixture"],
        )
        .map_err(|error| error.to_string())?;
    }
    let store = fixture.open("target.invalid", 1)?;
    let provider = store.project_isolation_provider(Some(fixture.visible_root.clone()));
    let checkout = provider
        .isolate(&echo_agent::subagent::IsolationRequest {
            kind: "worktree".to_string(),
            label: "writer-中文".to_string(),
        })
        .map_err(|error| error.to_string())?;
    assert_ne!(checkout.path, fixture.visible_root);
    assert_eq!(
        fixture
            .files
            .read(&checkout.path.join("same.txt"))
            .map_err(|error| error.to_string())?,
        b"TARGET"
    );
    fixture
        .files
        .write(&checkout.path.join("same.txt"), b"TARGET CHANGED")
        .map_err(|error| error.to_string())?;
    let outcome = (checkout.finalize)().map_err(|error| error.to_string())?;
    assert!(outcome.summary.contains("TARGET CHANGED"));
    assert_eq!(
        std::fs::read(fixture.visible_root.join("same.txt")).map_err(|error| error.to_string())?,
        b"LOCAL"
    );
    assert!(
        fixture
            .calls
            .lock()
            .map_err(|error| error.to_string())?
            .iter()
            .any(|cwd| cwd != &fixture.visible_root)
    );
    Ok(())
}

#[tokio::test]
async fn scoped_command_cell_uses_actual_executor_and_checkout_and_rejects_foreign_executor()
-> std::result::Result<(), String> {
    use echo_agent::tools::cell::{CommandCellOwner, CommandCellRequest};
    let fixture = EnvironmentFixture::new()?;
    let checkout = fixture.visible_root.join("checkout");
    fixture
        .files
        .create_dir_all(&checkout)
        .map_err(|error| error.to_string())?;
    fixture
        .files
        .write(&checkout.join("same.txt"), b"TARGET CHECKOUT")
        .map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&checkout).map_err(|error| error.to_string())?;
    std::fs::write(checkout.join("same.txt"), b"LOCAL CHECKOUT")
        .map_err(|error| error.to_string())?;
    let store = Arc::new(fixture.open("target.invalid", 1)?);
    let events = Arc::new(
        crate::chat_event_log::ChatEventLog::open(
            fixture.data_root.join("chat-events"),
            crate::chat_event_log::ChatEventRetention::default(),
        )
        .map_err(|error| error.to_string())?,
    );
    let fallback: Arc<dyn SandboxExecutor> = Arc::new(echo_agent::sandbox::LocalSandbox::new(
        echo_agent::sandbox::LocalConfig::default(),
    ));
    let service = super::command_cells::CommandCellRuntimeService::new(
        fallback.clone(),
        events,
        crate::product_data_io::ProductDataIoService::new(),
    )?;
    let scope = crate::workspace::WorkspaceExecutionScope::workspace(
        &crate::workspace::WorkspaceId::from_raw("workspace".to_string()),
        &fixture.visible_root,
    );
    let registry =
        service.scoped_with_executor(scope, Some(store.clone()), fixture.executor.clone());
    let request = CommandCellRequest {
        command: "cat same.txt".to_string(),
        working_dir: Some(checkout.to_string_lossy().to_string()),
        owner: CommandCellOwner {
            conversation_id: Some("conversation".to_string()),
            message_id: Some("root".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let foreign = registry
        .launch(CommandCellRequest {
            executor: Some(fallback),
            ..request.clone()
        })
        .await;
    assert!(foreign.is_err_and(|error| error.to_string().contains("executor does not belong")));
    let receipt = registry
        .launch(request)
        .await
        .map_err(|error| error.to_string())?;
    let mut cursor = 0;
    let mut output = String::new();
    let mut terminal = false;
    for _ in 0..20 {
        let delta = registry
            .wait(&receipt.cell_id, cursor, 250)
            .await
            .map_err(|error| error.to_string())?;
        cursor = delta.next_cursor;
        output.push_str(&delta.new_output);
        if delta.snapshot.phase.is_terminal() {
            terminal = true;
            break;
        }
    }
    assert!(terminal, "original command-cell owner did not settle");
    assert!(
        output.contains("TARGET CHECKOUT"),
        "actual target output missing: {output}"
    );
    assert!(!output.contains("LOCAL CHECKOUT"));
    assert!(
        fixture
            .calls
            .lock()
            .map_err(|error| error.to_string())?
            .contains(&checkout)
    );
    service.shutdown().await?;
    Ok(())
}

struct PendingPhysicalExecutor {
    started: tokio::sync::Notify,
    cancel_observed: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl SandboxExecutor for PendingPhysicalExecutor {
    fn name(&self) -> &str {
        "pending-project-operation"
    }
    fn isolation_level(&self) -> IsolationLevel {
        IsolationLevel::None
    }
    fn is_available(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
    }
    fn execute(&self, command: SandboxCommand) -> BoxFuture<'_, Result<ExecutionResult>> {
        Box::pin(async move {
            let _physical_guards = command.resource_guards;
            let cancel = command.cancel.ok_or_else(|| {
                ReactError::Other("original cell cancellation token missing".to_string())
            })?;
            self.started.notify_one();
            cancel.cancelled().await;
            self.cancel_observed.notify_one();
            self.release.notified().await;
            Ok(ExecutionResult {
                exit_code: 130,
                stdout: String::new(),
                stderr: String::new(),
                duration: std::time::Duration::ZERO,
                sandbox_type: self.name().to_string(),
                timed_out: false,
                cancelled: true,
                output_truncated: false,
                stdout_bytes: 0,
                stderr_bytes: 0,
            })
        })
    }
}

struct GuardDropProbe(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for GuardDropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[tokio::test]
async fn data_workspace_finalization_uses_the_same_bounded_artifact_reader()
-> std::result::Result<(), String> {
    use echo_agent::subagent::IsolationProvider;
    use sha2::{Digest, Sha256};
    let fixture = EnvironmentFixture::new()?;
    let store = fixture.open("target.invalid", 1)?;
    let provider = store.project_isolation_provider(Some(fixture.visible_root.clone()));
    let isolated = provider
        .isolate(&echo_agent::subagent::IsolationRequest {
            kind: "workspace".to_string(),
            label: "large-output".to_string(),
        })
        .map_err(|error| error.to_string())?;
    let path = isolated.path.join("large.bin");
    let bytes = vec![0x36_u8; 4 * 64 * 1024 + 23];
    fixture
        .files
        .write(&path, &bytes)
        .map_err(|error| error.to_string())?;
    fixture
        .reject_whole_reads
        .store(true, std::sync::atomic::Ordering::Release);
    let outcome = (isolated.finalize)().map_err(|error| error.to_string())?;
    let artifact = outcome
        .artifacts
        .iter()
        .find(|artifact| artifact.path == path.to_string_lossy())
        .ok_or_else(|| "target output artifact missing".to_string())?;
    assert!(artifact.available);
    assert_eq!(artifact.bytes, u64::try_from(bytes.len()).ok());
    assert_eq!(
        artifact.sha256.as_deref(),
        Some(hex::encode(Sha256::digest(&bytes)).as_str())
    );
    let requests = fixture
        .read_requests
        .lock()
        .map_err(|error| error.to_string())?;
    assert!(requests.len() >= 5);
    assert!(requests.iter().all(|requested| *requested <= 64 * 1024));
    Ok(())
}

#[tokio::test]
async fn task_tool_scope_uses_bound_local_data_identity_and_rejects_foreign_guards()
-> std::result::Result<(), String> {
    let fixture = EnvironmentFixture::new()?;
    let data_root = fixture
        .data_root
        .parent()
        .ok_or_else(|| "fixture product-data root missing".to_string())?
        .to_path_buf();
    let store = TaskRuntimeStore::open_for_workspace(&fixture.data_root, "global")
        .map_err(|error| error.to_string())?;
    store
        .bind_project_environment(
            "global",
            crate::workspace::ProjectLocation::Ssh {
                host: "target.invalid".to_string(),
                user: Some("echo".to_string()),
                port: Some(22),
                path: fixture.visible_root.to_string_lossy().to_string(),
            },
            1,
            fixture.files.clone(),
            fixture.executor.clone(),
        )
        .map_err(|error| error.to_string())?;
    store
        .bind_workspace_io_identity(crate::workspace::WorkspaceIoIdentity::global(&data_root))
        .map_err(|error| error.to_string())?;
    let receipt = crate::state::ScopedWorkspaceIoReceipt::global_for_test(&data_root);
    let context = echo_agent::tools::ToolContext {
        working_dir: Some(fixture.visible_root.clone()),
        resource_guards: receipt.invocation().resource_guards(),
        ..Default::default()
    };
    let scope = store
        .workspace_io_from_tool_context(&context)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "bound product scope missing".to_string())?;
    assert_eq!(scope.data_root(), data_root.as_path());
    assert_ne!(scope.data_root(), fixture.visible_root.as_path());
    let projected = super::task_tools::scoped_with_ctx_run_id(&context, Some(scope), || async {
        super::task_tools::current_workspace_io().map(|scope| scope.data_root().to_path_buf())
    })
    .await;
    assert_eq!(projected, Some(data_root.clone()));
    let foreign = crate::state::ScopedWorkspaceIoReceipt::global_for_test(&fixture.visible_root);
    let wrong = echo_agent::tools::ToolContext {
        resource_guards: foreign.invocation().resource_guards(),
        ..context.clone()
    };
    assert!(store.workspace_io_from_tool_context(&wrong).is_err());
    let duplicated = echo_agent::tools::ToolContext {
        resource_guards: context
            .resource_guards
            .iter()
            .chain(context.resource_guards.iter())
            .cloned()
            .collect(),
        ..context.clone()
    };
    assert!(store.workspace_io_from_tool_context(&duplicated).is_err());
    let no_guard = echo_agent::tools::ToolContext {
        working_dir: Some(data_root),
        ..Default::default()
    };
    assert!(store.workspace_io_from_tool_context(&no_guard).is_err());
    Ok(())
}

struct PhysicalReleaseOnDrop(Arc<PendingPhysicalExecutor>);
impl Drop for PhysicalReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release.notify_one();
    }
}

#[tokio::test]
async fn cancelled_scoped_cell_retains_original_guard_until_physical_receipt()
-> std::result::Result<(), String> {
    use echo_agent::tools::cell::{CommandCellOwner, CommandCellRequest};
    let mut fixture = EnvironmentFixture::new()?;
    let physical = Arc::new(PendingPhysicalExecutor {
        started: tokio::sync::Notify::new(),
        cancel_observed: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    fixture.executor = physical.clone();
    let _release_on_drop = PhysicalReleaseOnDrop(physical.clone());
    let store = Arc::new(fixture.open("pending.invalid", 1)?);
    let events = Arc::new(
        crate::chat_event_log::ChatEventLog::open(
            fixture.data_root.join("chat-events"),
            crate::chat_event_log::ChatEventRetention::default(),
        )
        .map_err(|error| error.to_string())?,
    );
    let service = super::command_cells::CommandCellRuntimeService::new(
        fixture.executor.clone(),
        events,
        crate::product_data_io::ProductDataIoService::new(),
    )?;
    let scope = crate::workspace::WorkspaceExecutionScope::workspace(
        &crate::workspace::WorkspaceId::from_raw("workspace".to_string()),
        &fixture.visible_root,
    );
    let registry =
        service.scoped_with_executor(scope, Some(store.clone()), fixture.executor.clone());
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let receipt = registry
        .launch(CommandCellRequest {
            command: "pending write".to_string(),
            working_dir: Some(fixture.visible_root.to_string_lossy().to_string()),
            owner: CommandCellOwner {
                conversation_id: Some("conversation".to_string()),
                message_id: Some("root".to_string()),
                ..Default::default()
            },
            resource_guards: vec![echo_agent::tools::InvocationResourceGuard::new(
                GuardDropProbe(drops.clone()),
            )],
            ..Default::default()
        })
        .await
        .map_err(|error| error.to_string())?;
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        physical.started.notified(),
    )
    .await
    .map_err(|error| error.to_string())?;
    assert!(registry.stop(&receipt.cell_id));
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        physical.cancel_observed.notified(),
    )
    .await
    .map_err(|error| error.to_string())?;
    let observation = registry
        .wait(&receipt.cell_id, 0, 20)
        .await
        .map_err(|error| error.to_string())?;
    assert!(
        !observation.snapshot.phase.is_terminal(),
        "cancellation request was confused with physical settlement"
    );
    assert_eq!(drops.load(std::sync::atomic::Ordering::Acquire), 0);
    physical.release.notify_one();
    let mut terminal = false;
    for _ in 0..20 {
        let observation = registry
            .wait(&receipt.cell_id, 0, 250)
            .await
            .map_err(|error| error.to_string())?;
        if observation.snapshot.phase.is_terminal() {
            terminal = true;
            break;
        }
    }
    assert!(terminal);
    service.shutdown().await?;
    assert_eq!(drops.load(std::sync::atomic::Ordering::Acquire), 1);
    Ok(())
}

#[tokio::test]
async fn gui_local_git_uses_native_process_but_automatic_git_keeps_project_executor()
-> std::result::Result<(), String> {
    let fixture = EnvironmentFixture::new()?;
    let store = TaskRuntimeStore::open_for_workspace(&fixture.data_root, "workspace")
        .map_err(|error| error.to_string())?;
    store
        .bind_project_environment(
            "workspace",
            crate::workspace::ProjectLocation::Local {
                path: fixture.visible_root.clone(),
            },
            1,
            fixture.files.clone(),
            fixture.executor.clone(),
        )
        .map_err(|error| error.to_string())?;
    let version = store
        .project_run_git(&fixture.visible_root, &["--version"])
        .map_err(|error| error.to_string())?;
    assert!(version.starts_with("git version "));
    assert!(
        fixture
            .calls
            .lock()
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    let environment = store
        .project_environment()
        .ok_or_else(|| "original project environment missing".to_string())?;
    super::worktree::run_git_in_environment(
        &fixture.visible_root,
        &["--version"],
        Some(&environment),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        fixture
            .calls
            .lock()
            .map_err(|error| error.to_string())?
            .as_slice(),
        &[fixture.visible_root.clone()]
    );
    Ok(())
}

#[tokio::test]
async fn gui_ssh_git_keeps_original_project_executor_without_local_fallback()
-> std::result::Result<(), String> {
    let fixture = EnvironmentFixture::new()?;
    let store = fixture.open("original.invalid", 1)?;
    let version = store
        .project_run_git(&fixture.visible_root, &["--version"])
        .map_err(|error| error.to_string())?;
    assert!(version.starts_with("git version "));
    assert_eq!(
        fixture
            .calls
            .lock()
            .map_err(|error| error.to_string())?
            .as_slice(),
        &[fixture.visible_root.clone()]
    );
    Ok(())
}
