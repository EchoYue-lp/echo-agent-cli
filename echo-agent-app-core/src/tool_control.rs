//! EKO product command and receipt surface for framework tool visibility.
//!
//! The revisioned policy authority is `echo_agent::tools::control`; this module
//! owns only EKO's command text and effective-enabled receipt.

#[cfg(test)]
use std::collections::HashSet;

use serde::Serialize;
use ts_rs::TS;

use crate::agent_handle::AgentHandle;

pub use echo_agent::tools::control::{
    ToolControlError, ToolControlMutation, ToolControlService, ToolControlSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, rename = "ToolControlReceipt")]
pub struct ToolControlReceipt {
    pub success: bool,
    pub name: String,
    pub policy_enabled: bool,
    pub effective_enabled: bool,
    pub changed: bool,
    #[ts(type = "number")]
    pub revision: u64,
}

/// Projection errors preserve parse rejection separately from an operation
/// whose original policy owner returned an error after possible mutation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolControlCommandError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Operation(String),
}

pub async fn execute_tool_control_command(
    state: &crate::state::AppState,
    agent: &AgentHandle,
    input: &str,
) -> String {
    execute_tool_control_command_result(state, agent, input)
        .await
        .unwrap_or_else(|error| error.to_string())
}

/// One executor serves both the legacy renderer and observed channel controls.
/// Callers inspect this native result before any error is turned into text.
pub(crate) async fn execute_tool_control_command_result(
    state: &crate::state::AppState,
    agent: &AgentHandle,
    input: &str,
) -> Result<String, ToolControlCommandError> {
    let mut parts = input.split_whitespace();
    let action = parts.next().unwrap_or("list");
    let usage = || {
        ToolControlCommandError::InvalidInput(
            "Usage: /tools [list|enable <name>|disable <name>]".to_string(),
        )
    };
    match action {
        "list" => state
            .get_tool_infos(agent)
            .await
            .map(format_tool_infos)
            .map_err(|error| {
                ToolControlCommandError::Unavailable(format!("Unable to list tools: {error}"))
            }),
        "enable" | "disable" => {
            let name = parts.next().ok_or_else(usage)?;
            if parts.next().is_some() {
                return Err(usage());
            }
            state
                .set_tool_enabled(agent, name, action == "enable")
                .await
                .map(|receipt| {
                    format!(
                        "Tool '{}' policy {} (effective: {}, generation {}).",
                        receipt.name,
                        if receipt.policy_enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        if receipt.effective_enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        receipt.revision
                    )
                })
                .map_err(|error| {
                    ToolControlCommandError::Operation(format!(
                        "Unable to {action} tool '{name}': {error}"
                    ))
                })
        }
        _ => Err(usage()),
    }
}

fn format_tool_infos(mut tools: Vec<crate::types::ToolInfo>) -> String {
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    if tools.is_empty() {
        return "No tools registered.".to_string();
    }
    let mut output = format!("Registered tools ({}):", tools.len());
    for tool in tools {
        output.push_str(&format!(
            "\n  [{}] {} - {}",
            if tool.enabled { "enabled" } else { "disabled" },
            tool.name,
            tool.description.chars().take(80).collect::<String>()
        ));
    }
    output
}

#[cfg(test)]
pub(crate) async fn snapshot_disabled_tools(agent: &AgentHandle) -> HashSet<String> {
    agent
        .read(|agent| {
            echo_agent::agent::AgentRunSnapshot::from_agent(agent)
                .tools
                .disabled_tools
                .clone()
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_receipt_is_monotonic_and_idempotent() -> Result<(), ToolControlError> {
        let service = ToolControlService::default();
        let disabled = service.set_enabled("shell", false)?;
        assert!(disabled.changed);
        assert_eq!(disabled.revision, 1);
        let duplicate = service.set_enabled("shell", false)?;
        assert!(!duplicate.changed);
        assert_eq!(duplicate.revision, 1);
        let enabled = service.set_enabled("shell", true)?;
        assert!(enabled.changed);
        assert_eq!(enabled.revision, 2);
        assert!(service.snapshot()?.disabled_tools.is_empty());
        Ok(())
    }
}
