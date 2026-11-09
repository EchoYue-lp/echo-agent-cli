//! 启动路径的延迟/操作计数夹具；不连接真实 SSH，不读取夹具外的宿主文件。
use echo_agent::error::{ReactError, Result};
use echo_agent::tools::files::LocalFileSystem;
use echo_agent::tools::filesystem::{FileEntry, FileMetadata, FileSystem, FileSystemReadSession};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Clone)]
pub(crate) struct CountingFiles {
    pub root: PathBuf,
    pub calls: Arc<Mutex<Vec<(&'static str, PathBuf)>>>,
    pub opened: Arc<AtomicUsize>,
    pub closed: Arc<AtomicUsize>,
    pub close_error: Arc<AtomicBool>,
    pub deny_listing: Arc<AtomicBool>,
    pub gate: Arc<ReadGate>,
    scoped: bool,
}
impl CountingFiles {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            calls: Arc::default(),
            opened: Arc::default(),
            closed: Arc::default(),
            close_error: Arc::default(),
            deny_listing: Arc::default(),
            gate: Arc::new(ReadGate::default()),
            scoped: false,
        }
    }
    fn wait_gate(&self) -> Result<()> {
        if self.gate.enabled.swap(false, Ordering::SeqCst) {
            self.gate.reached.add_permits(1);
            tokio::runtime::Handle::try_current()
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .block_on(async {
                    self.gate
                        .release
                        .acquire()
                        .await
                        .map(|permit| permit.forget())
                        .map_err(|e| std::io::Error::other(e.to_string()))
                })?;
        }
        Ok(())
    }
    fn record(&self, kind: &'static str, path: &Path) {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((kind, path.to_path_buf()));
    }
    pub fn count(&self, kind: &str, path: &Path) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(k, p)| *k == kind && p == path)
            .count()
    }
    fn bound(&self, path: &Path) -> Result<()> {
        if path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "outside fixture").into())
        }
    }
}
pub(crate) struct ReadGate {
    pub enabled: AtomicBool,
    pub reached: tokio::sync::Semaphore,
    pub release: tokio::sync::Semaphore,
}
impl Default for ReadGate {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            reached: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}
struct Session(CountingFiles);
impl Drop for Session {
    fn drop(&mut self) {
        self.0.closed.fetch_add(1, Ordering::SeqCst);
    }
}
impl FileSystemReadSession for Session {
    fn files(&self) -> &dyn FileSystem {
        &self.0
    }
    fn finish(self: Box<Self>) -> Result<()> {
        if self.0.close_error.load(Ordering::SeqCst) {
            Err(std::io::Error::other("injected read-session close failure").into())
        } else {
            Ok(())
        }
    }
}
fn readonly<T>() -> Result<T> {
    Err(ReactError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "readonly fixture",
    )))
}
impl FileSystem for CountingFiles {
    fn first_existing_path(&self, paths: &[PathBuf]) -> Result<Option<PathBuf>> {
        self.record("marker_batch", &self.root);
        self.wait_gate()?;
        for path in paths {
            self.record("batch_metadata", path);
            match self
                .bound(path)
                .and_then(|_| LocalFileSystem.metadata(path))
            {
                Ok(_) => return Ok(Some(path.clone())),
                Err(ReactError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    fn begin_read_session(&self) -> Result<Option<Box<dyn FileSystemReadSession>>> {
        if self.scoped {
            return Ok(None);
        }
        self.opened.fetch_add(1, Ordering::SeqCst);
        let mut files = self.clone();
        files.scoped = true;
        Ok(Some(Box::new(Session(files))))
    }
    fn current_dir(&self) -> Result<PathBuf> {
        Ok(self.root.clone())
    }
    fn canonicalize(&self, p: &Path) -> Result<PathBuf> {
        self.bound(p)?;
        LocalFileSystem.canonicalize(p)
    }
    fn metadata(&self, p: &Path) -> Result<FileMetadata> {
        self.record("metadata", p);
        self.bound(p)?;
        LocalFileSystem.metadata(p)
    }
    fn symlink_metadata(&self, p: &Path) -> Result<FileMetadata> {
        self.record("symlink_metadata", p);
        self.bound(p)?;
        LocalFileSystem.symlink_metadata(p)
    }
    fn read(&self, p: &Path) -> Result<Vec<u8>> {
        self.record("read", p);
        self.bound(p)?;
        LocalFileSystem.read(p)
    }
    fn read_dir(&self, p: &Path) -> Result<Vec<FileEntry>> {
        self.record("read_dir", p);
        if self.deny_listing.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "fixture cannot enumerate directory",
            )
            .into());
        }
        self.wait_gate()?;
        if self.bound(p).is_err() {
            return Ok(Vec::new());
        }
        LocalFileSystem.read_dir(p)
    }
    fn create_dir_all(&self, _: &Path) -> Result<()> {
        readonly()
    }
    fn create_file(&self, _: &Path) -> Result<()> {
        readonly()
    }
    fn write(&self, _: &Path, _: &[u8]) -> Result<()> {
        readonly()
    }
    fn append(&self, _: &Path, _: &[u8]) -> Result<()> {
        readonly()
    }
    fn rename(&self, _: &Path, _: &Path) -> Result<()> {
        readonly()
    }
    fn remove_file(&self, _: &Path) -> Result<()> {
        readonly()
    }
    fn remove_dir(&self, _: &Path) -> Result<()> {
        readonly()
    }
    fn remove_dir_all(&self, _: &Path) -> Result<()> {
        readonly()
    }
    fn set_mode(&self, _: &Path, _: u32) -> Result<()> {
        readonly()
    }
    fn atomic_write(&self, _: &Path, _: &[u8]) -> Result<()> {
        readonly()
    }
    fn checkpoint(&self, _: &Path) -> Result<Option<String>> {
        readonly()
    }
}

#[test]
fn startup_no_marker_uses_two_ordered_marker_batches_without_directory_enumeration()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let files = CountingFiles::new(temp.path().to_path_buf());
    assert!(crate::utils::find_project_root_with_file_system(temp.path(), &files)?.is_none());
    assert_eq!(files.count("marker_batch", temp.path()), 2);
    assert!(
        files
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .all(|(kind, _)| *kind != "read_dir")
    );
    let count = files
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(kind, _)| *kind == "batch_metadata")
        .count();
    assert_eq!(count, temp.path().ancestors().count().saturating_mul(10));
    Ok(())
}
#[test]
fn startup_root_keeps_vcs_precedence_and_dangling_marker_semantics() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let nested = root.join("nested");
    std::fs::create_dir(&nested)?;
    std::fs::write(nested.join("Cargo.toml"), "")?;
    std::fs::create_dir(root.join(".git"))?;
    let files = CountingFiles::new(root.to_path_buf());
    assert_eq!(
        crate::utils::find_project_root_with_file_system(&nested, &files)?.as_deref(),
        Some(root)
    );
    #[cfg(unix)]
    {
        std::fs::remove_dir(root.join(".git"))?;
        std::os::unix::fs::symlink(root.join("missing"), root.join(".git"))?;
        assert_eq!(
            crate::utils::find_project_root_with_file_system(&nested, &files)?.as_deref(),
            Some(nested.as_path())
        );
    }
    Ok(())
}
#[test]
fn startup_read_session_closes_on_read_failure_and_propagates_close_failure() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let files = CountingFiles::new(temp.path().to_path_buf());
    let failure =
        super::context::with_read_session(&files, |f| f.read(&temp.path().join("missing")));
    assert!(failure.is_err());
    assert_eq!(files.opened.load(Ordering::SeqCst), 1);
    assert_eq!(files.closed.load(Ordering::SeqCst), 1);
    files.close_error.store(true, Ordering::SeqCst);
    let failure = super::context::with_read_session(&files, |_| Ok(()));
    assert!(
        failure
            .err()
            .is_some_and(|e| e.to_string().contains("close failure"))
    );
    assert_eq!(files.closed.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
fn startup_marker_lookup_does_not_require_directory_listing() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::create_dir(temp.path().join(".git"))?;
    let files = CountingFiles::new(temp.path().to_path_buf());
    files.deny_listing.store(true, Ordering::SeqCst);
    assert_eq!(
        crate::utils::find_project_root_with_file_system(temp.path(), &files)?.as_deref(),
        Some(temp.path())
    );
    assert_eq!(files.count("batch_metadata", &temp.path().join(".git")), 1);
    Ok(())
}

#[tokio::test]
async fn startup_cancelled_observer_keeps_guards_until_original_read_session_closes()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let files = Arc::new(CountingFiles::new(temp.path().to_path_buf()));
    files.gate.enabled.store(true, Ordering::SeqCst);
    let (sender, mut settled) = tokio::sync::oneshot::channel();
    struct PhysicalGuard(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for PhysicalGuard {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let guard = echo_agent::tools::InvocationResourceGuard::new(PhysicalGuard(Some(sender)));
    let read_files = files.clone();
    let cwd = files.root.clone();
    let task = tokio::spawn(async move {
        crate::infra::prepare_dynamic_project_context(
            Some(&cwd),
            None,
            read_files,
            Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox()),
            vec![guard],
            32_000,
        )
        .await
    });
    struct Release(Arc<ReadGate>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release.add_permits(1);
        }
    }
    let release = Release(files.gate.clone());
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        files.gate.reached.acquire(),
    )
    .await??
    .forget();
    task.abort();
    assert!(task.await.is_err());
    assert!(matches!(
        settled.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(files.closed.load(Ordering::SeqCst), 0);
    drop(release);
    tokio::time::timeout(std::time::Duration::from_secs(10), settled).await??;
    assert_eq!(files.closed.load(Ordering::SeqCst), 1);
    Ok(())
}

pub(crate) fn task_store(
    files: Arc<CountingFiles>,
) -> anyhow::Result<Arc<crate::tasks::task_runtime::TaskRuntimeStore>> {
    let store = Arc::new(crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()?);
    let workspace = store.active_workspace_id().to_string();
    store.bind_project_environment(
        &workspace,
        crate::workspace::ProjectLocation::Local {
            path: files.root.clone(),
        },
        1,
        files,
        Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox()),
    )?;
    Ok(store)
}
