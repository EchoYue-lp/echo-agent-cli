//! Standard SFTP operations; no terminal-output file protocol or local fallback.

use super::channel::{SftpChannel, SftpClient};
use super::reader::ProjectSftpReader;
use super::{ProjectSshConnection, RemoteOperationResources, remote_path, validate_remote_text};
use echo_agent::error::{ReactError, Result, ToolError};
use echo_agent::sandbox::SandboxExecutor;
use echo_agent::tools::filesystem::{FileEntry, FileMetadata, FileSystem, FileType};
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
        // FileSystem's existing contract requires a blocking caller. All tool
        // algorithms and model-before-context preparation are offloaded by the
        // original caller and retain their invocation resource guards.
        self.runtime
            .block_on(with_sftp(Arc::clone(&self.connection), mutating, operation))
    }
}

impl FileSystem for ProjectSshFileSystem {
    fn open_read(&self, path: &Path) -> Result<Box<dyn std::io::Read + Send>> {
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
    // Dropping an observer does not cancel the operation or the standard
    // client's flush/read tasks. All callers share the original channel path.
    let original_resources = Arc::new(OnceLock::<Arc<RemoteOperationResources>>::new());
    let registration = Arc::clone(&original_resources);
    match tokio::spawn(async move {
        let channel = SftpChannel::open(connection, registration).await?;
        let result = operation(channel.client()).await;
        let settled = channel.finish(mutating).await;
        match (result, settled) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            if let Some(resources) = original_resources.get()
                && !resources.is_settled()
            {
                resources.mark_stop_unknown(
                    "original SFTP physical observation was lost after subsystem spawn",
                );
                resources.wait_for_settlement().await;
            }
            Err(remote_failure(
                format!("SFTP result observation failed: {error}"),
                mutating,
            ))
        }
    }
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
