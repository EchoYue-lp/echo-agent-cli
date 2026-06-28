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

use std::path::{Path, PathBuf};

use base64::Engine as _;
use echo_agent::llm::types::{ContentPart, ImageUrl, Message};

use crate::types::AttachmentData;

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
/// `~/.echo-agent/uploads/`.
pub fn resolve_uploads_dir(workspace_root: Option<&Path>) -> PathBuf {
    if let Some(root) = workspace_root {
        crate::workspace::layout::WorkspaceLayout::uploads(root)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".echo-agent").join("uploads")
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
    std::fs::write(&path, &bytes).map_err(|source| AttachmentError::Write {
        name: att.name.clone(),
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// Persist a batch of attachments, returning `(path, attachment)` pairs.
///
/// Failures are logged and skipped so a single bad attachment does not abort
/// the whole message; the caller proceeds with whatever saved successfully.
pub fn save_attachments<'a>(
    attachments: &'a [AttachmentData],
    uploads_dir: &Path,
) -> Vec<(PathBuf, &'a AttachmentData)> {
    let mut saved = Vec::new();
    for att in attachments {
        match save_attachment(att, uploads_dir) {
            Ok(path) => saved.push((path, att)),
            Err(e) => {
                tracing::warn!(error = %e, name = %att.name, "skipping attachment");
            }
        }
    }
    saved
}

/// Whether a MIME type is an image (routed to `ContentPart::ImageUrl`).
fn is_image_mime(mime: &str) -> bool {
    mime.starts_with("image/")
}

/// Build a multimodal user [`Message`] from text + saved attachments.
///
/// Images become `ContentPart::ImageUrl` with a `data:` URL (inline base64) so
/// the LLM sees the pixels. Everything else becomes `ContentPart::File` with
/// inline base64 content — the provider layer decides how to forward it
/// (Anthropic maps text-class files to inline text, PDFs to document blocks).
pub fn build_message(
    text: &str,
    attachments: &[(PathBuf, &AttachmentData)],
) -> std::io::Result<Message> {
    if attachments.is_empty() {
        return Ok(Message::user(text.to_string()));
    }

    let mut parts = Vec::with_capacity(attachments.len() + 1);
    parts.push(ContentPart::Text {
        text: text.to_string(),
    });

    for (path, att) in attachments {
        let bytes = std::fs::read(path)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        if is_image_mime(&att.mime_type) {
            // Inline data URL so providers that parse `image_url.url` (Anthropic
            // via data_url_to_image_source, OpenAI Vision) both work.
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

/// A persisted attachment reference (no base64 body — just enough to rebuild a
/// multimodal message by re-reading the file from disk).
///
/// Stored on `TaskRun` so every worker in a complex-task run can see the same
/// user-uploaded images/files without re-sending them through plan JSON.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttachmentRef {
    /// Absolute path of the persisted file under the uploads directory.
    pub path: PathBuf,
    /// Original filename (for display + provider content blocks).
    pub name: String,
    /// MIME type, decides `ContentPart::ImageUrl` vs `ContentPart::File`.
    pub mime_type: String,
}

impl AttachmentRef {
    /// Build a ref from a saved `(path, attachment)` pair.
    pub fn from_saved(path: PathBuf, att: &AttachmentData) -> Self {
        Self {
            path,
            name: att.name.clone(),
            mime_type: att.mime_type.clone(),
        }
    }
}

/// Build a multimodal user [`Message`] from text + attachment refs.
///
/// Unlike [`build_message`], this re-reads each file from disk (the refs carry
/// no base64 body), so it is suitable for workers that reconstruct the message
/// long after the original upload. Returns a plain text `Message` when there
/// are no refs.
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

    fn att(name: &str, mime: &str, data: &str) -> AttachmentData {
        AttachmentData {
            name: name.to_string(),
            mime_type: mime.to_string(),
            data: data.to_string(),
            size: data.len() as u64,
        }
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_name("../etc/passwd").is_ok()); // keeps "passwd"
        assert!(sanitize_name("").is_err());
        assert!(sanitize_name("   ").is_err());
        assert!(sanitize_name("..").is_err());
        assert!(sanitize_name("a\0b").is_err());
        // "passwd" is the last segment of "../etc/passwd"
        assert_eq!(sanitize_name("../etc/passwd").unwrap(), "passwd");
    }

    #[test]
    fn save_and_build_image_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let png_b64 = base64::engine::general_purpose::STANDARD.encode(b"fake-png");
        let attachments = [att("photo.png", "image/png", &png_b64)];
        let saved = save_attachments(&attachments, tmp.path());
        assert_eq!(saved.len(), 1);
        let msg = build_message("look", &saved).unwrap();
        // Multimodal message: 1 text + 1 image part.
        match &msg.content {
            echo_core::llm::types::MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[1], ContentPart::ImageUrl { .. }));
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    #[test]
    fn save_and_build_text_file() {
        let tmp = tempfile::tempdir().unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"hello world");
        let attachments = [att("notes.txt", "text/plain", &b64)];
        let saved = save_attachments(&attachments, tmp.path());
        let msg = build_message("see notes", &saved).unwrap();
        match &msg.content {
            echo_core::llm::types::MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[1], ContentPart::File { .. }));
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    #[test]
    fn build_message_no_attachments_is_text() {
        let msg = build_message("plain", &[]).unwrap();
        assert_eq!(msg.content.as_text(), Some("plain".to_string()));
    }
}
