//! Shared terminal and LSP commands for every EKO interaction surface.

use crate::terminal::TerminalService;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeveloperCommandDescriptor {
    pub name: &'static str,
    pub summary: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeveloperCommandOutput {
    pub message: String,
    pub attached_terminal: Option<String>,
}

pub struct DeveloperCommandRegistry {
    terminal: Arc<TerminalService>,
    app_state: Option<Arc<crate::state::AppState>>,
    browser_conversation_id: Option<String>,
}

impl DeveloperCommandRegistry {
    pub const COMMANDS: [DeveloperCommandDescriptor; 3] = [
        DeveloperCommandDescriptor {
            name: "terminal",
            summary: "Manage and attach to interactive terminal sessions",
        },
        DeveloperCommandDescriptor {
            name: "lsp",
            summary: "Inspect and manage workspace language servers",
        },
        DeveloperCommandDescriptor {
            name: "browser",
            summary: "Inspect and directly control the workspace browser",
        },
    ];

    pub fn commands() -> &'static [DeveloperCommandDescriptor] {
        &Self::COMMANDS
    }

    pub fn new(
        terminal: Arc<TerminalService>,
        app_state: Option<Arc<crate::state::AppState>>,
    ) -> Self {
        Self {
            terminal,
            app_state,
            browser_conversation_id: None,
        }
    }

    pub fn with_browser_conversation_id(mut self, conversation_id: impl Into<String>) -> Self {
        self.browser_conversation_id = Some(conversation_id.into());
        self
    }

    pub async fn execute(
        &self,
        namespace: &str,
        args: &[&str],
    ) -> Result<DeveloperCommandOutput, String> {
        match namespace {
            "terminal" | "term" => self.execute_terminal(args).await,
            "lsp" => self.execute_lsp(args).await,
            "browser" => self.execute_browser(args).await,
            other => Err(format!("unknown developer command '{other}'")),
        }
    }

    async fn execute_terminal(&self, args: &[&str]) -> Result<DeveloperCommandOutput, String> {
        let action = args.first().copied().unwrap_or("list");
        match action {
            "list" | "ls" => {
                let sessions = self.terminal.list();
                if sessions.is_empty() {
                    return Ok(output("No terminal sessions are running.".to_string()));
                }
                let lines = sessions
                    .into_iter()
                    .map(|session| format!("{} (pid {})", session.id, session.pid))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(output(lines))
            }
            "create" | "new" => {
                let id = required_arg(args, 1, "terminal create <id> [cwd] [rows] [cols]")?;
                let cwd = args.get(2).map(PathBuf::from);
                let rows = optional_number(args, 3, 24, "rows")?;
                let cols = optional_number(args, 4, 80, "cols")?;
                let info = self
                    .terminal
                    .create(id.to_string(), cwd, rows, cols)
                    .await?;
                Ok(DeveloperCommandOutput {
                    message: format!("Terminal '{}' started (pid {}).", info.id, info.pid),
                    attached_terminal: Some(info.id),
                })
            }
            "attach" => {
                let id = required_arg(args, 1, "terminal attach <id>")?;
                if !self.terminal.contains(id) {
                    return Err(format!("terminal '{id}' not found"));
                }
                Ok(DeveloperCommandOutput {
                    message: format!("Attached to terminal '{id}'."),
                    attached_terminal: Some(id.to_string()),
                })
            }
            "write" | "send" => {
                let id = required_arg(args, 1, "terminal write <id> <data>")?;
                let data = args
                    .get(2..)
                    .filter(|values| !values.is_empty())
                    .ok_or_else(|| "terminal write requires data".to_string())?
                    .join(" ");
                self.terminal.write(id, data.as_bytes()).await?;
                Ok(output(format!(
                    "Wrote {} bytes to terminal '{id}'.",
                    data.len()
                )))
            }
            "resize" => {
                let id = required_arg(args, 1, "terminal resize <id> <rows> <cols>")?;
                let rows = required_number(args, 2, "rows")?;
                let cols = required_number(args, 3, "cols")?;
                self.terminal.resize(id, rows, cols).await?;
                Ok(output(format!("Terminal '{id}' resized to {cols}x{rows}.")))
            }
            "close" | "kill" => {
                let id = required_arg(args, 1, "terminal close <id>")?;
                if !self.terminal.close(id).await? {
                    return Err(format!("terminal '{id}' not found"));
                }
                Ok(output(format!("Terminal '{id}' closed.")))
            }
            _ => Err("usage: terminal <list|create|attach|write|resize|close> ...".to_string()),
        }
    }

    async fn execute_lsp(&self, args: &[&str]) -> Result<DeveloperCommandOutput, String> {
        let state = self
            .app_state
            .as_ref()
            .ok_or_else(|| "LSP runtime is not initialized".to_string())?;
        let action = args.first().copied().unwrap_or("status");
        state
            .extension_control
            .lsp_command(state, action, args.get(1).copied())
            .await
            .map(output)
            .map_err(|error| error.to_string())
    }

    async fn execute_browser(&self, args: &[&str]) -> Result<DeveloperCommandOutput, String> {
        let state = self
            .app_state
            .as_ref()
            .ok_or_else(|| "Browser runtime is not initialized".to_string())?;
        let conversation_id = self
            .browser_conversation_id
            .as_deref()
            .unwrap_or("developer-browser-control");
        state
            .extension_control
            .browser_command(state, conversation_id, args)
            .await
            .map(output)
            .map_err(|error| error.to_string())
    }
}

fn output(message: String) -> DeveloperCommandOutput {
    DeveloperCommandOutput {
        message,
        attached_terminal: None,
    }
}

fn required_arg<'a>(args: &'a [&str], index: usize, usage: &str) -> Result<&'a str, String> {
    args.get(index)
        .copied()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("usage: {usage}"))
}

fn required_number(args: &[&str], index: usize, name: &str) -> Result<u16, String> {
    args.get(index)
        .copied()
        .ok_or_else(|| format!("{name} is required"))?
        .parse::<u16>()
        .map_err(|error| format!("invalid {name}: {error}"))
}

fn optional_number(args: &[&str], index: usize, default: u16, name: &str) -> Result<u16, String> {
    match args.get(index) {
        Some(value) => value
            .parse::<u16>()
            .map_err(|error| format!("invalid {name}: {error}")),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminal_registry_reports_missing_attach_without_spawning() {
        let registry = DeveloperCommandRegistry::new(TerminalService::new(), None);
        let error = registry.execute("terminal", &["attach", "missing"]).await;
        assert!(error.is_err_and(|error| error.contains("not found")));
    }

    #[tokio::test]
    async fn terminal_registry_lists_the_shared_service() -> Result<(), String> {
        let terminal = TerminalService::new();
        let registry = DeveloperCommandRegistry::new(terminal, None);
        let result = registry.execute("term", &["list"]).await?;
        assert_eq!(
            result,
            DeveloperCommandOutput {
                message: "No terminal sessions are running.".to_string(),
                attached_terminal: None,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn lsp_registry_fails_closed_without_the_runtime() {
        let registry = DeveloperCommandRegistry::new(TerminalService::new(), None);
        let error = registry.execute("lsp", &["status"]).await;
        assert!(error.is_err_and(|error| error.contains("not initialized")));
    }

    #[tokio::test]
    async fn browser_registry_fails_closed_without_the_runtime() {
        let registry = DeveloperCommandRegistry::new(TerminalService::new(), None);
        let error = registry.execute("browser", &["status"]).await;
        assert!(error.is_err_and(|error| error.contains("not initialized")));
    }
}
