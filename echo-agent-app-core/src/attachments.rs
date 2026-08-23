//! Attachment handling: persist uploads to disk and build multimodal messages.
//!
//! EKO's chat path carries attachments (images, documents) from the frontend
//! to the agent. The frontend already base64-encodes file contents; this
//! module writes them to a per-workspace uploads directory and constructs a
//! [`Message`] with the right [`ContentPart`]s so the LLM actually sees them.
//!
//! Lifecycle:
//! 1. Frontend sends `AttachmentData { name, mime_type, data: base64, size }`.
//! 2. [`save_attachment`] decodes and writes the file under `uploads_dir`,
//!    returning the path (validation prevents path traversal).
//! 3. [`build_message`] reads each file back and builds a `Message::user_multimodal`
//!    with `ContentPart::ImageUrl` (images) or `ContentPart::File` (documents).

use std::io::Read as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use echo_agent::llm::types::{ContentPart, ImageUrl, Message};
use sha2::{Digest, Sha256};

use crate::types::{AttachmentData, AttachmentSource};

/// Errors from attachment handling.
#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    #[error("failed to create uploads directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid attachment name '{name}': {reason}")]
    InvalidName { name: String, reason: &'static str },
    #[error("base64 decode failed for attachment '{name}': {source}")]
    Decode {
        name: String,
        source: base64::DecodeError,
    },
    #[error("failed to write attachment '{name}' to {path}: {source}")]
    Write {
        name: String,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{primary}; cleanup after failed attachment staging also failed: {cleanup}")]
    Cleanup {
        primary: Box<AttachmentError>,
        cleanup: String,
    },
    #[error("failed to read attachment back from {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

type Result<T> = std::result::Result<T, AttachmentError>;

/// Resolve the uploads directory for the active workspace.
///
/// Prefers `{workspace_root}/.eko/uploads/` (per-workspace isolation). When no
/// workspace is active (first-turn, global chats), falls back to the global
/// `~/.eko/uploads/`.
pub fn resolve_uploads_dir(workspace_root: Option<&Path>) -> PathBuf {
    if let Some(root) = workspace_root {
        crate::workspace::layout::WorkspaceLayout::uploads(root)
    } else {
        crate::data_root::user_data_path("uploads")
    }
}

/// Stage a local file for a terminal/chat turn and return its durable ref.
pub fn stage_local_attachment(
    source: &Path,
    workspace_root: Option<&Path>,
) -> Result<AttachmentRef> {
    let bytes = std::fs::read(source).map_err(|source_error| AttachmentError::Read {
        path: source.to_path_buf(),
        source: source_error,
    })?;
    let name = source
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AttachmentError::InvalidName {
            name: source.display().to_string(),
            reason: "path has no UTF-8 filename",
        })?
        .to_string();
    let attachment = AttachmentData {
        name: name.clone(),
        mime_type: infer_mime_type(&name).to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        size: std::fs::metadata(source)
            .map(|value| value.len())
            .unwrap_or(0),
        source: AttachmentSource::Upload,
    };
    stage_attachment_data(&attachment, workspace_root)
}

/// Persist already-decoded transport data and return the shared durable ref.
pub fn stage_attachment_data(
    attachment: &AttachmentData,
    workspace_root: Option<&Path>,
) -> Result<AttachmentRef> {
    let path = save_attachment(attachment, &resolve_uploads_dir(workspace_root))?;
    Ok(AttachmentRef::from_saved(path, attachment))
}

/// Conservative MIME inference shared by terminal attachment commands.
pub fn infer_mime_type(name: &str) -> &'static str {
    let extension = name.rsplit('.').next().map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt") | Some("md") | Some("rs") | Some("py") | Some("ts") | Some("js") => {
            "text/plain"
        }
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        _ => "application/octet-stream",
    }
}

/// Sanitize an attachment filename into a path-safe base name.
///
/// Rejects empty names, path separators, `..`, and NUL bytes. The returned
/// string has no directory components — callers join it under `uploads_dir`.
fn sanitize_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AttachmentError::InvalidName {
            name: name.to_string(),
            reason: "empty filename",
        });
    }
    // Strip any directory components: keep only the final path segment.
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    if base == ".." || base == "." || base.contains('\0') || base.is_empty() {
        return Err(AttachmentError::InvalidName {
            name: name.to_string(),
            reason: "reserved or empty path segment",
        });
    }
    Ok(base.to_string())
}

/// Persist a single attachment to `uploads_dir`, returning the file path.
///
/// The file is written as `{uploads_dir}/{uuid}_{sanitized_name}` to avoid
/// collisions between uploads with the same filename. The directory is created
/// if missing.
pub fn save_attachment(att: &AttachmentData, uploads_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(uploads_dir).map_err(|source| AttachmentError::CreateDir {
        path: uploads_dir.to_path_buf(),
        source,
    })?;

    let base = sanitize_name(&att.name)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&att.data)
        .map_err(|source| AttachmentError::Decode {
            name: att.name.clone(),
            source,
        })?;

    let file_name = format!("{}_{}", uuid::Uuid::new_v4(), base);
    let path = uploads_dir.join(file_name);
    echo_agent::utils::fs::atomic_write(&path, &bytes).map_err(|source| {
        AttachmentError::Write {
            name: att.name.clone(),
            path: path.clone(),
            source,
        }
    })?;
    Ok(path)
}

/// Persist a batch of attachments, returning `(path, attachment)` pairs.
///
/// The batch is fail-closed: if one item cannot be staged, every file written
/// by this call is removed before the error is returned.
pub fn save_attachments<'a>(
    attachments: &'a [AttachmentData],
    uploads_dir: &Path,
) -> Result<Vec<(PathBuf, &'a AttachmentData)>> {
    let mut saved = Vec::new();
    for att in attachments {
        match save_attachment(att, uploads_dir) {
            Ok(path) => saved.push((path, att)),
            Err(error) => {
                let paths = saved
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>();
                return match remove_paths(&paths) {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(AttachmentError::Cleanup {
                        primary: Box::new(error),
                        cleanup,
                    }),
                };
            }
        }
    }
    Ok(saved)
}

fn remove_paths(paths: &[PathBuf]) -> std::result::Result<(), String> {
    let mut failures = Vec::new();
    for path in paths.iter().rev() {
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            failures.push(format!("{}: {error}", path.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Owns one successful flat staging batch until a prepared turn takes over.
#[must_use]
pub struct StagedAttachmentBatch {
    paths: Vec<PathBuf>,
    armed: bool,
}

impl StagedAttachmentBatch {
    pub fn from_saved(saved: &[(PathBuf, &AttachmentData)]) -> Self {
        Self {
            paths: saved.iter().map(|(path, _)| path.clone()).collect(),
            armed: true,
        }
    }

    pub fn commit(mut self) {
        self.armed = false;
    }

    pub fn rollback(mut self) -> std::result::Result<(), String> {
        let result = remove_paths(&self.paths);
        self.armed = false;
        result
    }
}

impl Drop for StagedAttachmentBatch {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = remove_paths(&self.paths)
        {
            tracing::error!(%error, "failed to roll back an uncommitted attachment batch");
        }
    }
}

/// Remove only this module's UUID-prefixed flat staging files.
pub fn discard_staged_attachment_refs(
    attachments: &[AttachmentRef],
) -> std::result::Result<(), String> {
    let paths = attachments
        .iter()
        .filter_map(|attachment| match validated_staged_path(&attachment.path) {
            Ok(path) => path.map(Ok),
            Err(error) => Some(Err(error)),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    remove_paths(&paths)
}

#[derive(Debug)]
pub(crate) struct StagedAttachmentRetirementError {
    primary: String,
    restoration_failures: Vec<String>,
    retained_scoped_paths: Vec<PathBuf>,
}

impl StagedAttachmentRetirementError {
    pub(crate) fn retained_scoped_paths(&self) -> &[PathBuf] {
        &self.retained_scoped_paths
    }
}

impl std::fmt::Display for StagedAttachmentRetirementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.primary)?;
        if !self.restoration_failures.is_empty() {
            write!(
                formatter,
                "; restoration after failed retirement also failed: {}",
                self.restoration_failures.join("; ")
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for StagedAttachmentRetirementError {}

#[derive(Debug)]
struct PreparedStagedRetirement {
    staged_path: PathBuf,
    scoped_path: PathBuf,
}

/// Retire staging files only after all scoped copies and identities validate.
pub(crate) fn retire_staged_attachment_refs(
    retirements: &[(&AttachmentRef, &Path, Option<&str>)],
) -> std::result::Result<(), StagedAttachmentRetirementError> {
    retire_staged_attachment_refs_inner(retirements, None)
}

fn retire_staged_attachment_refs_inner(
    retirements: &[(&AttachmentRef, &Path, Option<&str>)],
    fail_before_index: Option<usize>,
) -> std::result::Result<(), StagedAttachmentRetirementError> {
    let mut prepared = Vec::new();
    for (attachment, scoped_path, expected_sha256) in retirements {
        let Some(staged_path) =
            validated_staged_path(&attachment.path).map_err(retirement_error)?
        else {
            continue;
        };
        validate_regular_file(scoped_path, "scoped copy").map_err(retirement_error)?;
        let expected_sha256 = expected_sha256.ok_or_else(|| {
            retirement_error(format!(
                "{} has no content identity for staged retirement",
                scoped_path.display()
            ))
        })?;
        validate_identity(&staged_path, expected_sha256).map_err(retirement_error)?;
        validate_identity(scoped_path, expected_sha256).map_err(retirement_error)?;
        prepared.push(PreparedStagedRetirement {
            staged_path,
            scoped_path: scoped_path.to_path_buf(),
        });
    }

    let mut retired = Vec::new();
    for (index, item) in prepared.iter().enumerate() {
        if fail_before_index == Some(index) {
            return Err(restore_retired_staging(
                format!(
                    "injected staging retirement failure before {}",
                    item.staged_path.display()
                ),
                &retired,
            ));
        }
        if let Err(error) = std::fs::remove_file(&item.staged_path) {
            return Err(restore_retired_staging(
                format!("{}: {error}", item.staged_path.display()),
                &retired,
            ));
        }
        retired.push(item);
    }
    Ok(())
}

fn retirement_error(primary: String) -> StagedAttachmentRetirementError {
    StagedAttachmentRetirementError {
        primary,
        restoration_failures: Vec::new(),
        retained_scoped_paths: Vec::new(),
    }
}

fn restore_retired_staging(
    primary: String,
    retired: &[&PreparedStagedRetirement],
) -> StagedAttachmentRetirementError {
    let mut restoration_failures = Vec::new();
    let mut retained_scoped_paths = Vec::new();
    for item in retired.iter().rev() {
        let result = match std::fs::symlink_metadata(&item.staged_path) {
            Ok(_) => Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "staging path reappeared before restoration",
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::read(&item.scoped_path).and_then(|bytes| {
                    echo_agent::utils::fs::atomic_write(&item.staged_path, &bytes)
                })
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            restoration_failures.push(format!(
                "{} from {}: {error}",
                item.staged_path.display(),
                item.scoped_path.display()
            ));
            retained_scoped_paths.push(item.scoped_path.clone());
        }
    }
    StagedAttachmentRetirementError {
        primary,
        restoration_failures,
        retained_scoped_paths,
    }
}

fn validated_staged_path(path: &Path) -> std::result::Result<Option<PathBuf>, String> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    let owner = parent.parent();
    let owned_namespace = parent.file_name().and_then(|name| name.to_str()) == Some("uploads")
        && owner
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some(".eko");
    let owned_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.split_once('_').map(|(prefix, _)| prefix))
        .is_some_and(|prefix| uuid::Uuid::parse_str(prefix).is_ok());
    if !owned_namespace || !owned_name {
        return Ok(None);
    }
    validate_regular_directory(parent, "uploads directory")?;
    let Some(owner) = owner else {
        return Ok(None);
    };
    validate_regular_directory(owner, ".eko directory")?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(path.to_path_buf())),
        Ok(_) => Err(format!("{} is not a regular staged file", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn validate_regular_directory(path: &Path, label: &str) -> std::result::Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(format!("{} is not a regular {label}", path.display())),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn validate_regular_file(path: &Path, label: &str) -> std::result::Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(format!("{} is not a regular {label}", path.display())),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn validate_identity(path: &Path, expected_sha256: &str) -> std::result::Result<(), String> {
    let actual_sha256 = hash_file(path)?;
    if actual_sha256 == expected_sha256 {
        Ok(())
    } else {
        Err(format!(
            "{} content identity mismatch: expected sha256 {expected_sha256}, found {actual_sha256}",
            path.display()
        ))
    }
}

fn hash_file(path: &Path) -> std::result::Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        let chunk = buffer.get(..read).ok_or_else(|| {
            format!(
                "{} returned invalid read length {read} for {} byte buffer",
                path.display(),
                buffer.len()
            )
        })?;
        hasher.update(chunk);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Whether a MIME type is an image (routed to `ContentPart::ImageUrl`).
pub(crate) fn is_image_mime(mime: &str) -> bool {
    mime.starts_with("image/")
}

/// A persisted attachment reference (no base64 body — just enough to rebuild a
/// multimodal message by re-reading the file from disk).
///
/// Stored on `TaskRun` so every subagent in a complex-task run can see the same
/// user-uploaded images/files without re-sending them through plan JSON.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttachmentRef {
    /// Absolute path of the persisted file under the uploads directory.
    pub path: PathBuf,
    /// Original filename (for display + provider content blocks).
    pub name: String,
    /// MIME type, decides `ContentPart::ImageUrl` vs `ContentPart::File`.
    pub mime_type: String,
    #[serde(default)]
    pub source: AttachmentSource,
}

impl AttachmentRef {
    /// Build a ref from a saved `(path, attachment)` pair.
    pub fn from_saved(path: PathBuf, att: &AttachmentData) -> Self {
        Self {
            path,
            name: att.name.clone(),
            mime_type: att.mime_type.clone(),
            source: att.source,
        }
    }
}

/// Build a multimodal user [`Message`] from text + attachment refs.
///
/// Re-reads each file from disk (the refs carry no base64 body), so it is
/// suitable for subagents that reconstruct the message long after the original
/// upload. Returns a plain text `Message` when there are no refs. The five
/// entry points now go through [`PreparedUserTurn`](crate::prepared_turn::PreparedUserTurn)
/// instead; this helper remains for the subagent rebuild path in `executor.rs`.
pub fn build_message_from_refs(
    text: &str,
    attachments: &[AttachmentRef],
) -> std::io::Result<Message> {
    if attachments.is_empty() {
        return Ok(Message::user(text.to_string()));
    }

    let mut parts = Vec::with_capacity(attachments.len() + 1);
    parts.push(ContentPart::Text {
        text: text.to_string(),
    });

    for att in attachments {
        let bytes = std::fs::read(&att.path)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        if is_image_mime(&att.mime_type) {
            parts.push(ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: format!("data:{};base64,{}", att.mime_type, b64),
                    detail: None,
                },
            });
        } else {
            parts.push(ContentPart::File {
                name: att.name.clone(),
                content: b64,
            });
        }
    }

    Ok(Message::user_multimodal(parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256(value: &[u8]) -> String {
        hex::encode(Sha256::digest(value))
    }

    fn att(name: &str, mime: &str, data: &str) -> AttachmentData {
        AttachmentData {
            name: name.to_string(),
            mime_type: mime.to_string(),
            data: data.to_string(),
            size: data.len() as u64,
            source: AttachmentSource::Upload,
        }
    }

    #[test]
    fn sanitize_rejects_traversal() -> std::result::Result<(), String> {
        assert!(sanitize_name("../etc/passwd").is_ok()); // keeps "passwd"
        assert!(sanitize_name("").is_err());
        assert!(sanitize_name("   ").is_err());
        assert!(sanitize_name("..").is_err());
        assert!(sanitize_name("a\0b").is_err());
        // "passwd" is the last segment of "../etc/passwd"
        assert_eq!(
            sanitize_name("../etc/passwd").map_err(|error| error.to_string())?,
            "passwd"
        );
        Ok(())
    }

    #[test]
    fn save_and_build_image_roundtrip() -> std::result::Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let png_b64 = base64::engine::general_purpose::STANDARD.encode(b"fake-png");
        let attachments = [att("photo.png", "image/png", &png_b64)];
        let saved =
            save_attachments(&attachments, tmp.path()).map_err(|error| error.to_string())?;
        assert_eq!(saved.len(), 1);
        let refs: Vec<_> = saved
            .iter()
            .map(|(p, a)| AttachmentRef::from_saved(p.clone(), a))
            .collect();
        let msg = build_message_from_refs("look", &refs).map_err(|error| error.to_string())?;
        // Multimodal message: 1 text + 1 image part.
        let echo_agent::llm::types::MessageContent::Parts(parts) = &msg.content else {
            return Err(format!("expected Parts, got {:?}", msg.content));
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts.get(1), Some(ContentPart::ImageUrl { .. })));
        Ok(())
    }

    #[test]
    fn save_and_build_text_file() -> std::result::Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"hello world");
        let attachments = [att("notes.txt", "text/plain", &b64)];
        let saved =
            save_attachments(&attachments, tmp.path()).map_err(|error| error.to_string())?;
        let refs: Vec<_> = saved
            .iter()
            .map(|(p, a)| AttachmentRef::from_saved(p.clone(), a))
            .collect();
        let msg = build_message_from_refs("see notes", &refs).map_err(|error| error.to_string())?;
        let echo_agent::llm::types::MessageContent::Parts(parts) = &msg.content else {
            return Err(format!("expected Parts, got {:?}", msg.content));
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts.get(1), Some(ContentPart::File { .. })));
        Ok(())
    }

    #[test]
    fn build_message_no_attachments_is_text() -> std::result::Result<(), String> {
        let msg = build_message_from_refs("plain", &[]).map_err(|error| error.to_string())?;
        assert_eq!(msg.content.as_text(), Some("plain".to_string()));
        Ok(())
    }

    #[test]
    fn attachment_batch_failure_removes_every_staged_file() -> std::result::Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let valid = base64::engine::general_purpose::STANDARD.encode(b"first");
        let attachments = [
            att("first.txt", "text/plain", &valid),
            att("broken.txt", "text/plain", "not-base64%%%"),
        ];
        assert!(save_attachments(&attachments, tmp.path()).is_err());
        assert_eq!(
            std::fs::read_dir(tmp.path())
                .map_err(|error| error.to_string())?
                .count(),
            0
        );
        Ok(())
    }

    #[test]
    fn staged_retirement_restores_prior_items_and_can_retry() -> std::result::Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let uploads = temporary.path().join(".eko/uploads");
        let scoped = temporary
            .path()
            .join(".eko/artifacts/user-input/conversation/turn");
        std::fs::create_dir_all(&uploads).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&scoped).map_err(|error| error.to_string())?;
        let first_staged = uploads.join(format!("{}_first.txt", uuid::Uuid::new_v4()));
        let second_staged = uploads.join(format!("{}_second.txt", uuid::Uuid::new_v4()));
        let first_scoped = scoped.join("first.txt");
        let second_scoped = scoped.join("second.txt");
        std::fs::write(&first_staged, "first").map_err(|error| error.to_string())?;
        std::fs::write(&second_staged, "second").map_err(|error| error.to_string())?;
        std::fs::write(&first_scoped, "first").map_err(|error| error.to_string())?;
        std::fs::write(&second_scoped, "second").map_err(|error| error.to_string())?;
        let first = AttachmentRef {
            path: first_staged.clone(),
            name: "first.txt".to_string(),
            mime_type: "text/plain".to_string(),
            source: AttachmentSource::Upload,
        };
        let second = AttachmentRef {
            path: second_staged.clone(),
            name: "second.txt".to_string(),
            mime_type: "text/plain".to_string(),
            source: AttachmentSource::Upload,
        };
        let first_hash = sha256(b"first");
        let second_hash = sha256(b"second");
        let retirements = [
            (&first, first_scoped.as_path(), Some(first_hash.as_str())),
            (&second, second_scoped.as_path(), Some(second_hash.as_str())),
        ];

        let failure = retire_staged_attachment_refs_inner(&retirements, Some(1))
            .err()
            .ok_or_else(|| "injected retirement unexpectedly succeeded".to_string())?;
        assert!(failure.retained_scoped_paths().is_empty());
        assert_eq!(
            std::fs::read_to_string(&first_staged).map_err(|error| error.to_string())?,
            "first"
        );
        assert!(second_staged.exists());

        retire_staged_attachment_refs(&retirements).map_err(|error| error.to_string())?;
        assert!(!first_staged.exists());
        assert!(!second_staged.exists());
        assert!(first_scoped.exists());
        assert!(second_scoped.exists());
        Ok(())
    }

    #[test]
    fn discard_only_removes_owned_flat_staging_files() -> std::result::Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let uploads = temporary.path().join(".eko/uploads");
        std::fs::create_dir_all(&uploads).map_err(|error| error.to_string())?;
        let staged = uploads.join(format!("{}_selected.txt", uuid::Uuid::new_v4()));
        let arbitrary = uploads.join("notes.txt");
        std::fs::write(&staged, "staged").map_err(|error| error.to_string())?;
        std::fs::write(&arbitrary, "user").map_err(|error| error.to_string())?;
        let refs = [
            AttachmentRef {
                path: staged.clone(),
                name: "selected.txt".to_string(),
                mime_type: "text/plain".to_string(),
                source: AttachmentSource::Upload,
            },
            AttachmentRef {
                path: arbitrary.clone(),
                name: "notes.txt".to_string(),
                mime_type: "text/plain".to_string(),
                source: AttachmentSource::Upload,
            },
        ];
        discard_staged_attachment_refs(&refs)?;
        assert!(!staged.exists());
        assert!(arbitrary.exists());
        Ok(())
    }
}
