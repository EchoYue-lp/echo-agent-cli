//! Unified error type for Tauri IPC commands.

use serde::Serialize;
use std::fmt;

#[derive(Debug)]
pub enum IpcError {
    NotFound(String),
    Validation(String),
    Internal(String),
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

impl Serialize for IpcError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
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
