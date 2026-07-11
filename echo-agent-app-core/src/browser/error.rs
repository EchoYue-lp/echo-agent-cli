use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("managed browser is disabled")]
    Disabled,
    #[error("browser prerequisite check failed: {0}")]
    Prerequisite(String),
    #[error("browser runtime I/O failed: {0}")]
    Io(String),
    #[error("Playwright MCP connection failed: {0}")]
    Connection(String),
    #[error("browser operation cancelled")]
    Cancelled,
    #[error("Playwright MCP tool '{tool}' failed: {message}")]
    Tool { tool: String, message: String },
}

pub type BrowserResult<T> = Result<T, BrowserError>;
