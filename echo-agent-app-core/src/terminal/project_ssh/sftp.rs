//! Standard SFTP operations; no terminal-output file protocol or local fallback.

use super::channel::{SftpChannel, SftpClient};
use super::reader::ProjectSftpReader;
use super::{ProjectSshConnection, remote_path, validate_remote_text};
use echo_agent::error::{ReactError, Result, ToolError};
use echo_agent::sandbox::SandboxExecutor;
use echo_agent::tools::filesystem::{
    FileEntry, FileMetadata, FileSystem, FileSystemReadSession, FileType,
};
use echo_agent::tools::{ToolFailure, ToolFailureCategory};
use futures::StreamExt;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::runtime::Handle;

pub struct ProjectSshFileSystem {
    connection: Arc<ProjectSshConnection>,
    directory: PathBuf,
    runtime: Handle,
    read_client: Option<Arc<SftpClient>>,
}

impl ProjectSshFileSystem {
    pub(super) fn new(
        connection: Arc<ProjectSshConnection>,
        remote_directory: String,
    ) -> std::result::Result<Self, String> {
        validate_remote_text(&remote_directory)?;
        if !remote_directory.starts_with('/') {
            return Err("project SFTP requires the verified absolute remote directory".to_string());
        }
        let runtime = Handle::try_current().map_err(|error| {
            format!("project SFTP requires the original application runtime: {error}")
        })?;
        Ok(Self {
            connection,
            directory: PathBuf::from(remote_directory),
            runtime,
            read_client: None,
        })
    }

    fn path(&self, path: &Path) -> Result<PathBuf> {
        remote_path(path).map_err(invalid)?;
        Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.directory.join(path)
        })
    }

    fn run<T, F, Fut>(&self, mutating: bool, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<SftpClient>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        if let Some(client) = &self.read_client {
            if mutating {
                return Err(invalid(
                    "a project preparation read session cannot mutate files",
                ));
            }
            return self.runtime.block_on(operation(Arc::clone(client)));
        }
        // FileSystem's existing contract requires a blocking caller. All tool
        // algorithms and model-before-context preparation are offloaded by the
        // original caller and retain their invocation resource guards.
        self.runtime
            .block_on(with_sftp(Arc::clone(&self.connection), mutating, operation))
    }
}

impl FileSystem for ProjectSshFileSystem {
    fn first_existing_path(&self, paths: &[PathBuf]) -> Result<Option<PathBuf>> {
        if paths.is_empty() {
            return Ok(None);
        }
        let paths = paths
            .iter()
            .map(|path| (path.clone(), self.path(path)))
            .collect::<Vec<_>>();
        self.run(false, move |sftp| async move {
            let mut pending = paths.into_iter();
            loop {
                // 只探测给定 marker，最多 32 项在途，成本与祖先深度相关，
                // 不随仓库/HOME 内无关文件数量增长。结果仍按原优先级解释。
                let batch = pending.by_ref().take(32).collect::<Vec<_>>();
                if batch.is_empty() {
                    return Ok(None);
                }
                let results =
                    futures::future::join_all(batch.into_iter().map(|(original, path)| {
                        let sftp = Arc::clone(&sftp);
                        async move {
                            let result = match path {
                                Ok(path) => match sftp.fs().metadata(path).await {
                                    Ok(_) => Ok(true),
                                    Err(error) => match sftp.error(error, false) {
                                        ReactError::Io(error)
                                            if error.kind() == std::io::ErrorKind::NotFound =>
                                        {
                                            Ok(false)
                                        }
                                        error => Err(error),
                                    },
                                },
                                Err(error) => Err(error),
                            };
                            (original, result)
                        }
                    }))
                    .await;
                for (path, result) in results {
                    if result? {
                        return Ok(Some(path));
                    }
                }
            }
        })
    }

    fn cache_namespace(&self) -> Option<String> {
        // 连接 generation 是控制目录（本会话私有 tempfile）派生的 sha256：
        // 同一物理连接内稳定，重连后必然变化。再拼远程根目录，隔离同一连接上
        // 不同项目根里的同名路径。远程根已由 new/verify_directory 校验为 UTF-8
        // 绝对路径，lossy 转换不会改变这里要表达的身份。
        Some(format!(
            "{}:{}",
            self.connection.generation(),
            self.directory.to_string_lossy()
        ))
    }

    fn begin_read_session(&self) -> Result<Option<Box<dyn FileSystemReadSession>>> {
        if self.read_client.is_some() {
            return Ok(None);
        }
        let registration = Arc::new(OnceLock::new());
        let channel = self.runtime.block_on(SftpChannel::open(
            Arc::clone(&self.connection),
            registration,
        ))?;
        let files = Self {
            connection: Arc::clone(&self.connection),
            directory: self.directory.clone(),
            runtime: self.runtime.clone(),
            read_client: Some(channel.client()),
        };
        Ok(Some(Box::new(ProjectReadSession {
            files,
            channel: Some(channel),
        })))
    }

    fn open_read(&self, path: &Path) -> Result<Box<dyn std::io::Read + Send>> {
        if self.read_client.is_some() {
            return Err(invalid(
                "streaming readers cannot escape a preparation read session",
            ));
        }
        let path = self.path(path)?;
        let reader = self.runtime.block_on(ProjectSftpReader::open(
            Arc::clone(&self.connection),
            self.runtime.clone(),
            path,
        ))?;
        Ok(Box::new(reader))
    }
    fn current_dir(&self) -> Result<PathBuf> {
        Ok(self.directory.clone())
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
        let path = self.path(path)?;
        self.run(false, move |sftp| async move {
            sftp.fs()
                .canonicalize(path)
                .await
                .map_err(|error| sftp.error(error, false))
        })
    }

    fn metadata(&self, path: &Path) -> Result<FileMetadata> {
        let path = self.path(path)?;
        self.run(false, move |sftp| async move {
            let metadata = sftp
                .fs()
                .metadata(path)
                .await
                .map_err(|error| sftp.error(error, false))?;
            metadata_value(metadata)
        })
    }

    fn symlink_metadata(&self, path: &Path) -> Result<FileMetadata> {
        let path = self.path(path)?;
        self.run(false, move |sftp| async move {
            let metadata = sftp
                .fs()
                .symlink_metadata(path)
                .await
                .map_err(|error| sftp.error(error, false))?;
            metadata_value(metadata)
        })
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        let path = self.path(path)?;
        self.run(false, move |sftp| async move {
            sftp.fs()
                .read(path)
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|error| sftp.error(error, false))
        })
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<FileEntry>> {
        let path = self.path(path)?;
        self.run(false, move |sftp| async move {
            let directory = sftp
                .fs()
                .open_dir(&path)
                .await
                .map_err(|error| sftp.error(error, false))?;
            let entries = directory.read_dir();
            futures::pin_mut!(entries);
            let mut result = Vec::new();
            while let Some(entry) = entries.next().await {
                let entry = entry.map_err(|error| sftp.error(error, false))?;
                let name = entry.filename();
                if name == Path::new(".") || name == Path::new("..") {
                    continue;
                }
                // A server entry is one filename, not a second pathname whose
                // parent components could silently redirect the original walk.
                if name.components().count() != 1 || name.is_absolute() {
                    return Err(invalid("remote directory returned an invalid filename"));
                }
                let metadata = if entry.file_type().is_some() {
                    entry.metadata()
                } else {
                    sftp.fs()
                        .symlink_metadata(path.join(name))
                        .await
                        .map_err(|error| sftp.error(error, false))?
                };
                result.push(FileEntry {
                    path: path.join(name),
                    file_name: name.as_os_str().to_os_string(),
                    file_type: metadata_value(metadata)?.file_type,
                });
            }
            Ok(result)
        })
    }

    fn create_dir_all(&self, path: &Path) -> Result<()> {
        let path = self.path(path)?;
        self.run(true, move |sftp| async move {
            create_directories(&sftp, &path).await.map(|_| ())
        })
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        let path = self.path(path)?;
        self.run(true, move |sftp| async move {
            sftp.fs()
                .create_dir(path)
                .await
                .map_err(|error| sftp.error(error, true))
        })
    }

    fn create_file(&self, path: &Path) -> Result<()> {
        let path = self.path(path)?;
        self.run(true, move |sftp| async move {
            let file = sftp
                .options()
                .write(true)
                .create_new(true)
                .open(path)
                .await
                .map_err(|error| sftp.error(error, true))?;
            file.close().await.map_err(|error| sftp.error(error, true))
        })
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let path = self.path(path)?;
        let bytes = bytes.to_vec();
        self.run(true, move |sftp| async move {
            let mut file = sftp
                .options()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .await
                .map_err(|error| sftp.error(error, true))?;
            file.write_all(&bytes)
                .await
                .map_err(|error| sftp.error(error, true))?;
            file.close().await.map_err(|error| sftp.error(error, true))
        })
    }

    fn append(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let path = self.path(path)?;
        let bytes = bytes.to_vec();
        self.run(true, move |sftp| async move {
            let mut file = sftp
                .options()
                .write(true)
                .create(true)
                .append(true)
                .open(path)
                .await
                .map_err(|error| sftp.error(error, true))?;
            file.write_all(&bytes)
                .await
                .map_err(|error| sftp.error(error, true))?;
            file.close().await.map_err(|error| sftp.error(error, true))
        })
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        let from = self.path(from)?;
        let to = self.path(to)?;
        self.run(true, move |sftp| async move {
            sftp.fs()
                .rename(from, to)
                .await
                .map_err(|error| sftp.error(error, true))
        })
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        let path = self.path(path)?;
        self.run(true, move |sftp| async move {
            sftp.fs()
                .remove_file(path)
                .await
                .map_err(|error| sftp.error(error, true))
        })
    }

    fn remove_dir(&self, path: &Path) -> Result<()> {
        let path = self.path(path)?;
        self.run(true, move |sftp| async move {
            sftp.fs()
                .remove_dir(path)
                .await
                .map_err(|error| sftp.error(error, true))
        })
    }

    fn remove_dir_all(&self, path: &Path) -> Result<()> {
        let path = self.path(path)?;
        self.run(true, move |sftp| async move {
            let mut fs = sftp.fs();
            let mut pending = vec![(path, false)];
            let mut mutation_completed = false;
            while let Some((path, visited)) = pending.pop() {
                let metadata = fs.symlink_metadata(&path).await.map_err(|error| sftp.error_with_effects(error, false, mutation_completed))?;
                if !metadata.file_type().is_some_and(|kind| kind.is_dir()) {
                    fs.remove_file(&path).await.map_err(|error| sftp.error(error, true))?;
                    mutation_completed = true;
                } else if visited {
                    fs.remove_dir(&path).await.map_err(|error| sftp.error(error, true))?;
                    mutation_completed = true;
                } else {
                    pending.push((path.clone(), true));
                    let entries = fs.open_dir(&path).await.map_err(|error| sftp.error_with_effects(error, false, mutation_completed))?.read_dir();
                    futures::pin_mut!(entries);
                    while let Some(entry) = entries.next().await {
                        let entry = entry.map_err(|error| sftp.error_with_effects(error, false, mutation_completed))?;
                        let name = entry.filename();
                        if name == Path::new(".") || name == Path::new("..") { continue; }
                        if name.is_absolute() || name.components().count() != 1 {
                            return Err(remote_failure("remote traversal returned an invalid filename after possible removals", true));
                        }
                        pending.push((path.join(name), false));
                    }
                }
            }
            Ok(())
        })
    }

    fn set_mode(&self, path: &Path, mode: u32) -> Result<()> {
        let path = self.path(path)?;
        self.run(true, move |sftp| async move {
            let permissions = openssh_sftp_client::metadata::Permissions::from_raw(
                openssh_sftp_client::metadata::RawPermissions::from_bits_truncate(mode & 0o7777),
            );
            sftp.fs()
                .set_permissions(path, permissions)
                .await
                .map_err(|error| sftp.error(error, true))
        })
    }

    fn atomic_write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let path = self.path(path)?;
        let bytes = bytes.to_vec();
        self.run(true, move |sftp| async move {
            if !sftp.support_posix_rename() {
                return Err(unavailable("remote server does not support atomic posix-rename; original file was not truncated"));
            }
            let parent = path.parent().ok_or_else(|| invalid("remote atomic-write target has no parent"))?;
            let parent_created = create_directories(&sftp, parent).await?;
            let permissions = match sftp.fs().metadata(&path).await {
                Ok(metadata) => metadata.permissions(),
                Err(openssh_sftp_client::Error::SftpError(openssh_sftp_client::error::SftpErrorKind::NoSuchFile, _)) => None,
                Err(error) => return Err(sftp.error_with_effects(error, false, parent_created)),
            };
            let temporary = parent.join(format!(".eko-write-{}", uuid::Uuid::new_v4()));
            let mut file = sftp.options().write(true).create_new(true).open(&temporary).await.map_err(|error| sftp.error(error, true))?;
            let failed = |error| {
                sftp.note_error(&error, true);
                remote_failure(format!("atomic-write staging {} may remain; verify before cleanup or retry: {error}", temporary.display()), true)
            };
            if let Some(permissions) = permissions {
                file.set_permissions(permissions).await.map_err(failed)?;
            }
            file.write_all(&bytes).await.map_err(failed)?;
            if sftp.support_fsync() {
                file.sync_all().await.map_err(failed)?;
            }
            file.close().await.map_err(failed)?;
            // Do not guess rollback or replay after a lost rename response.
            sftp.fs().rename(&temporary, &path).await.map_err(failed)
        })
    }

    fn checkpoint(&self, path: &Path) -> Result<Option<String>> {
        if self.read_client.is_some() {
            return Err(invalid(
                "checkpoint mutation is unavailable in a read session",
            ));
        }
        let path = self.path(path)?;
        let execute = |command| {
            let sandbox = self.connection.sandbox();
            self.runtime.block_on(sandbox.execute(command))
        };
        let checkpoint = echo_agent::tools::git_checkpoint::create_checkpoint_with_backend(
            &path, self, &execute,
        )?;
        if checkpoint.is_some() {
            echo_agent::tools::git_checkpoint::cleanup_old_checkpoints_with_backend(
                &path, 10, self, &execute,
            )?;
        }
        Ok(checkpoint)
    }

    fn atomic_compare_and_swap(&self, path: &Path, expected: &[u8], bytes: &[u8]) -> Result<bool> {
        if self.read_client.is_some() {
            return Err(invalid("compare-and-swap is unavailable in a read session"));
        }
        let path = self.path(path)?;
        self.runtime.block_on(super::cas::compare_and_swap(
            Arc::clone(&self.connection),
            self.directory.clone(),
            path,
            expected,
            bytes,
        ))
    }
}

struct ProjectReadSession {
    files: ProjectSshFileSystem,
    channel: Option<SftpChannel>,
}

impl ProjectReadSession {
    fn close(&mut self) -> Result<()> {
        // 必须先释放最后一个 scoped client，原 close 才能证明请求全部结算。
        self.files.read_client.take();
        match self.channel.take() {
            Some(channel) => self.files.runtime.block_on(channel.finish(false)),
            None => Ok(()),
        }
    }
}

impl FileSystemReadSession for ProjectReadSession {
    fn files(&self) -> &dyn FileSystem {
        &self.files
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.close()
    }
}

impl Drop for ProjectReadSession {
    fn drop(&mut self) {
        // 准备任务在 blocking 线程自然结束；取消外层 await 不会跳过这里，
        // 原 invocation guards 也一直由该线程持有到物理关闭结束。
        if let Err(error) = self.close() {
            tracing::error!(%error, "project read session close failed during unwinding");
        }
    }
}

async fn create_directories(sftp: &SftpClient, path: &Path) -> Result<bool> {
    let mut mutation_completed = false;
    let mut current = PathBuf::new();
    let mut fs = sftp.fs();
    for component in path.components() {
        match component {
            Component::RootDir => current.push("/"),
            Component::CurDir => {}
            Component::Normal(name) => {
                current.push(name);
                match fs.metadata(&current).await {
                    Ok(metadata) if metadata.file_type().is_some_and(|kind| kind.is_dir()) => {}
                    Ok(_) => {
                        return Err(remote_failure(
                            "remote ancestor is not a directory; earlier ancestors may have been created",
                            true,
                        ));
                    }
                    Err(openssh_sftp_client::Error::SftpError(
                        openssh_sftp_client::error::SftpErrorKind::NoSuchFile,
                        _,
                    )) => {
                        fs.create_dir(&current)
                            .await
                            .map_err(|error| sftp.error(error, true))?;
                        mutation_completed = true;
                    }
                    Err(error) => {
                        return Err(sftp.error_with_effects(error, false, mutation_completed));
                    }
                }
            }
            _ => {
                return Err(remote_failure(
                    "remote directory path is not normalized; earlier ancestors may have been created",
                    true,
                ));
            }
        }
    }
    Ok(mutation_completed)
}

pub(super) async fn verify_directory(
    connection: Arc<ProjectSshConnection>,
    path: String,
) -> std::result::Result<String, String> {
    let verification = with_sftp(connection, false, move |sftp| async move {
        let mut fs = sftp.fs();
        let path = fs
            .canonicalize(path)
            .await
            .map_err(|error| sftp.error(error, false))?;
        let metadata = fs
            .metadata(&path)
            .await
            .map_err(|error| sftp.error(error, false))?;
        if !metadata.file_type().is_some_and(|kind| kind.is_dir()) {
            return Err(invalid("selected remote project path is not a directory"));
        }
        remote_path(&path).map(str::to_string).map_err(invalid)
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), verification)
        .await
        .map_err(|_| "remote directory verification remains unsettled; the original project connection retains its operation".to_string())?
        .map_err(|error| error.to_string())
}

async fn with_sftp<T, F, Fut>(
    connection: Arc<ProjectSshConnection>,
    mutating: bool,
    operation: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(Arc<SftpClient>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T>> + Send + 'static,
{
    // 每次文件操作不再重新启动标准 SFTP 子进程：操作在连接级车道上执行，
    // 车道独占、可疑即弃，关闭时由 observe_idle 统一结算。观察者被丢弃
    // 不取消已在途的请求，这一语义由 SftpLanes::run 的独立任务保持。
    connection.sftp_lanes().run(mutating, operation).await
}

fn metadata_value(metadata: openssh_sftp_client::metadata::MetaData) -> Result<FileMetadata> {
    let file_type = match metadata.file_type() {
        Some(kind) if kind.is_file() => FileType::File,
        Some(kind) if kind.is_dir() => FileType::Directory,
        Some(kind) if kind.is_symlink() => FileType::Symlink,
        Some(_) => FileType::Other,
        None => {
            return Err(unavailable(
                "remote server did not provide the requested file type",
            ));
        }
    };
    Ok(FileMetadata {
        file_type,
        len: match metadata.len() {
            Some(len) => len,
            None if file_type == FileType::File => {
                return Err(unavailable(
                    "remote server did not provide the file length required by the original read budget",
                ));
            }
            None => 0,
        },
        mode: metadata
            .permissions()
            .map(|permissions| permissions.as_raw().bits()),
        // SFTP v3 只给秒级时间戳；缺失时保持 None，绝不用本机时间顶替。
        modified: metadata
            .modified()
            .map(openssh_sftp_client::UnixTimeStamp::as_system_time),
    })
}

pub(super) fn sftp_error(error: openssh_sftp_client::Error, mutating: bool) -> ReactError {
    if !mutating && let openssh_sftp_client::Error::SftpError(kind, _) = &error {
        if matches!(kind, openssh_sftp_client::error::SftpErrorKind::NoSuchFile) {
            return std::io::Error::new(std::io::ErrorKind::NotFound, error).into();
        }
        if matches!(kind, openssh_sftp_client::error::SftpErrorKind::PermDenied) {
            return std::io::Error::new(std::io::ErrorKind::PermissionDenied, error).into();
        }
    }
    remote_failure(error.to_string(), mutating)
}

pub(super) fn remote_failure(message: impl Into<String>, mutating: bool) -> ReactError {
    ToolError::RemoteFailure {
        tool: "project_ssh".to_string(),
        message: message.into(),
        http_status: None,
        failure: Box::new(ToolFailure::new(if mutating {
            ToolFailureCategory::PartialSideEffect
        } else {
            ToolFailureCategory::Unavailable
        })),
    }
    .into()
}

pub(super) fn unavailable(message: impl Into<String>) -> ReactError {
    remote_failure(message, false)
}

fn invalid(message: impl Into<String>) -> ReactError {
    ToolError::InvalidParameter {
        name: "remote_path".to_string(),
        message: message.into(),
    }
    .into()
}

#[cfg(all(test, target_os = "macos"))]
mod startup_protocol_tests {
    use super::*;
    use echo_agent::tools::filesystem::FileSystemReadSession;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn startup_one_real_sftp_subsystem_serves_all_preparation_reads_and_settles()
    -> anyhow::Result<()> {
        // 真实标准协议经本地 stdio fixture；不认证、不访问真实 SSH 服务器。
        let temp = tempfile::tempdir()?;
        std::fs::write(temp.path().join("AGENTS.md"), "本地协议夹具 🧪")?;
        let service = crate::terminal::TerminalService::new();
        let target = crate::terminal::TerminalTarget::Ssh {
            host: "fixture.invalid".into(),
            user: Some("fixture".into()),
            port: Some(22),
        };
        let connection =
            ProjectSshConnection::prepare(&service, "startup-local-sftp", target.clone())
                .map_err(anyhow::Error::msg)?;
        connection
            .effective_target
            .set(target.clone())
            .map_err(|_| anyhow::anyhow!("duplicate fixture target"))?;
        let operation = connection.operation().map_err(anyhow::Error::msg)?;
        let child = tokio::process::Command::new("/usr/libexec/sftp-server")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let channel = SftpChannel::from_spawned(
            connection.clone(),
            Arc::new(OnceLock::new()),
            child,
            operation,
            target,
        )
        .await?;
        // 任何意外重新开通道均在原 operation 门禁处失败，不产生网络请求。
        connection.begin_shutdown();
        let files = ProjectSshFileSystem {
            connection: connection.clone(),
            directory: temp.path().to_path_buf(),
            runtime: Handle::try_current()?,
            read_client: Some(channel.client()),
        };
        let session = ProjectReadSession {
            files,
            channel: Some(channel),
        };
        let root = temp.path().to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let files = session.files();
            assert!(files.metadata(&root)?.is_dir());
            assert!(files.symlink_metadata(&root.join("AGENTS.md"))?.is_file());
            assert_eq!(
                files.read_to_string(&root.join("AGENTS.md"))?,
                "本地协议夹具 🧪"
            );
            assert!(!files.exists(&root.join("missing"))?);
            let mut markers = (0..70)
                .map(|n| root.join(format!("missing-{n}")))
                .collect::<Vec<_>>();
            markers.push(root.join("AGENTS.md"));
            assert_eq!(
                files.first_existing_path(&markers)?,
                Some(root.join("AGENTS.md"))
            );
            assert_eq!(files.read_dir(&root)?.len(), 1);
            assert!(files.write(&root.join("AGENTS.md"), b"wrong").is_err());
            assert!(
                files
                    .atomic_compare_and_swap(&root.join("AGENTS.md"), b"", b"wrong")
                    .is_err()
            );
            assert!(files.checkpoint(&root).is_err());
            assert!(files.open_read(&root.join("AGENTS.md")).is_err());
            Box::new(session).finish()
        })
        .await??;
        assert_eq!(connection.active_operations.load(Ordering::Acquire), 0);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("AGENTS.md"))?,
            "本地协议夹具 🧪"
        );
        Ok(())
    }
}
