//! Bounded target reads for the original std::io::Read hashing/context path.

use super::ProjectSshConnection;
use super::channel::SftpChannel;
use super::sftp::sftp_error;
use echo_agent::error::Result;
use openssh_sftp_client::file::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::runtime::Handle;

const READ_CHUNK_BYTES: usize = 64 * 1024;

struct ReaderResources {
    file: File,
    channel: SftpChannel,
}

pub(super) struct ProjectSftpReader {
    runtime: Handle,
    resources: Option<ReaderResources>,
    failed: bool,
}

impl ProjectSftpReader {
    pub(super) async fn open(
        connection: Arc<ProjectSshConnection>,
        runtime: Handle,
        path: PathBuf,
    ) -> Result<Self> {
        let channel = SftpChannel::open(connection, Arc::new(OnceLock::new())).await?;
        let opened = {
            let client = channel.client();
            client.open(path).await
        };
        match opened {
            Ok(file) => Ok(Self {
                runtime,
                resources: Some(ReaderResources { file, channel }),
                failed: false,
            }),
            Err(error) => {
                let original = sftp_error(error, false);
                match channel.finish(false).await {
                    Ok(()) => Err(original),
                    Err(error) => Err(error),
                }
            }
        }
    }

    fn finish(&mut self) -> Result<()> {
        let Some(ReaderResources { file, channel }) = self.resources.take() else {
            return Ok(());
        };
        self.runtime.block_on(async move {
            let closed = file.close().await.map_err(|error| sftp_error(error, false));
            let settled = channel.finish(false).await;
            match (closed, settled) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), _) | (_, Err(error)) => Err(error),
            }
        })
    }
}

impl Read for ProjectSftpReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.failed {
            return Err(io::Error::other(
                "previous SFTP streaming read failed; the request is not replayed",
            ));
        }
        let Some(resources) = self.resources.as_mut() else {
            return Ok(0);
        };
        let count = buffer.len().min(READ_CHUNK_BYTES);
        let requested =
            u32::try_from(count).map_err(|_| io::Error::other("SFTP chunk count overflow"))?;
        let result = self.runtime.block_on(async {
            // Official File::read owns the actual remote file handle/offset.
            // The temporary BytesMut is inferred by the standard API, so this
            // adapter needs no new bytes dependency or whole-file allocation.
            match resources.file.read(requested, Default::default()).await {
                Ok(None) => Ok(0),
                Ok(Some(chunk)) if chunk.is_empty() => Err(io::Error::other(
                    "SFTP returned an empty non-EOF chunk; read stopped",
                )),
                Ok(Some(chunk)) => {
                    if chunk.len() > count {
                        return Err(io::Error::other(
                            "SFTP response exceeded the bounded requested chunk",
                        ));
                    }
                    let destination = buffer.get_mut(..chunk.len()).ok_or_else(|| {
                        io::Error::other("SFTP response exceeded the requested read buffer")
                    })?;
                    destination.copy_from_slice(&chunk);
                    Ok(chunk.len())
                }
                Err(error) => Err(io::Error::other(sftp_error(error, false))),
            }
        });
        match result {
            Ok(0) => {
                // A successful hash must not observe EOF before original file,
                // client/read/flush tasks, child and diagnostic pipe settle.
                if let Err(error) = self.finish() {
                    self.failed = true;
                    return Err(io::Error::other(error));
                }
                Ok(0)
            }
            Ok(count) => Ok(count),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }
}

impl Drop for ProjectSftpReader {
    fn drop(&mut self) {
        // FileSystem callers are already on the original blocking boundary.
        // Early exit still settles this same file/channel before their guard
        // can leave scope; no new runtime, thread or request retry is created.
        if self.resources.is_some() {
            let _unobserved_cleanup = self.finish();
        }
    }
}
