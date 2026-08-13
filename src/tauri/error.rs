//! Unified error type for Tauri IPC commands.

use serde::Serialize;
use std::fmt;

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
