//! Unified error type and authorization gate for Tauri IPC commands.
//!
//! ## Authorization (Phase 6.2)
//!
//! Commands that spawn processes, write files outside the workspace, or
//! execute arbitrary code are gated behind `IpcAuth::require_full_auto()`.
//! This checks that the user has explicitly set the permission mode to
//! `full-auto` before the command runs.  Commands that read sensitive
//! files (e.g. `native_read_file` targeting `~/.ssh`) are gated behind
//! `IpcAuth::require_not_strict()`.

use serde::Serialize;
use std::fmt;

/// Permission level required for dangerous IPC operations (Phase 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IpcPermission {
    /// Only `full-auto` (or fullauto/bypass) mode allows the operation.
    FullAuto,
    /// `strict` mode (i.e., the default conservative mode) blocks the operation;
    /// any higher mode allows it.
    NotStrict,
}

impl IpcPermission {
    /// Check whether the given permission-mode string satisfies this requirement.
    pub fn is_satisfied_by(&self, mode: &str) -> bool {
        match self {
            IpcPermission::FullAuto => {
                matches!(mode, "full-auto" | "fullauto" | "bypass")
            }
            IpcPermission::NotStrict => {
                !matches!(mode, "strict" | "strict-confirm" | "strict-confirmation")
            }
        }
    }
}

/// Gate for dangerous Tauri IPC commands (Phase 6.2).
pub struct IpcAuth;

impl IpcAuth {
    /// Require that the user has set the permission mode high enough for
    /// process-spawning / file-writing commands.
    pub fn require_full_auto(mode: &str) -> Result<(), IpcError> {
        if IpcPermission::FullAuto.is_satisfied_by(mode) {
            Ok(())
        } else {
            Err(IpcError::Validation(format!(
                "This operation requires permission mode 'full-auto' (current: '{}'). \
                 Set it in Settings > Permissions.",
                mode
            )))
        }
    }

    /// Require that the user is not in strict mode (allows reading outside
    /// the immediate workspace, for example).
    pub fn require_not_strict(mode: &str) -> Result<(), IpcError> {
        if IpcPermission::NotStrict.is_satisfied_by(mode) {
            Ok(())
        } else {
            Err(IpcError::Validation(format!(
                "This operation is blocked in '{}' mode. \
                 Switch to a higher permission mode to proceed.",
                mode
            )))
        }
    }
}

#[derive(Debug)]
pub enum IpcError {
    NotFound(String),
    Validation(String),
    Internal(String),
}

impl IpcError {
    /// Machine-readable error kind for frontend matching.
    pub fn kind(&self) -> &'static str {
        match self {
            IpcError::NotFound(_) => "not_found",
            IpcError::Validation(_) => "validation",
            IpcError::Internal(_) => "internal",
        }
    }
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpcError::NotFound(msg) => write!(f, "{}", msg),
            IpcError::Validation(msg) => write!(f, "{}", msg),
            IpcError::Internal(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for IpcError {}

impl Serialize for IpcError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("IpcError", 3)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("message", &self.to_string())?;
        s.serialize_field("error", self.kind())?; // legacy compat
        s.end()
    }
}

impl From<String> for IpcError {
    fn from(s: String) -> Self {
        IpcError::Internal(s)
    }
}

impl From<anyhow::Error> for IpcError {
    fn from(e: anyhow::Error) -> Self {
        IpcError::Internal(e.to_string())
    }
}
