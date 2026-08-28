//! EKO-owned interactive tool visibility policy.
//!
//! The framework remains the execution authority: each Agent projects this
//! policy through `ReactAgent::set_disabled_tools`, and the resulting run
//! snapshot filters model schemas and rejects forced calls. This service owns
//! only the product-level user choice and its monotonic generation.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use serde::Serialize;
use ts_rs::TS;

use crate::agent_handle::AgentHandle;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ToolControlError {
    #[error("tool name cannot be empty")]
    EmptyName,
    #[error("tool '{name}' is not registered")]
    NotRegistered { name: String },
    #[error("tool-control generation is exhausted")]
    GenerationExhausted,
    #[error("tool-control policy lock is poisoned")]
    StateUnavailable,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ToolControlSnapshot {
    pub revision: u64,
    pub disabled_tools: HashSet<String>,
}

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

pub(crate) struct ToolControlMutation {
    pub name: String,
    pub policy_enabled: bool,
    pub changed: bool,
    pub revision: u64,
}

impl ToolControlMutation {
    pub(crate) fn into_receipt(self, effective_enabled: bool) -> ToolControlReceipt {
        ToolControlReceipt {
            success: true,
            name: self.name,
            policy_enabled: self.policy_enabled,
            effective_enabled,
            changed: self.changed,
            revision: self.revision,
        }
    }
}

#[derive(Default)]
pub struct ToolControlService {
    state: RwLock<ToolControlSnapshot>,
}

impl ToolControlService {
    pub(crate) fn snapshot(&self) -> Result<ToolControlSnapshot, ToolControlError> {
        self.state
            .read()
            .map(|state| state.clone())
            .map_err(|_| ToolControlError::StateUnavailable)
    }

    pub(crate) fn set_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<ToolControlMutation, ToolControlError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ToolControlError::EmptyName);
        }
        let mut state = self
            .state
            .write()
            .map_err(|_| ToolControlError::StateUnavailable)?;
        let changed = if enabled {
            state.disabled_tools.contains(name)
        } else {
            !state.disabled_tools.contains(name)
        };
        let revision = if changed {
            state
                .revision
                .checked_add(1)
                .ok_or(ToolControlError::GenerationExhausted)?
        } else {
            state.revision
        };
        if enabled {
            state.disabled_tools.remove(name);
        } else {
            state.disabled_tools.insert(name.to_string());
        }
        state.revision = revision;
        Ok(ToolControlMutation {
            name: name.to_string(),
            policy_enabled: enabled,
            changed,
            revision: state.revision,
        })
    }
}

pub async fn execute_tool_control_command(
    state: &crate::state::AppState,
    agent: &AgentHandle,
    input: &str,
) -> String {
    let mut parts = input.split_whitespace();
    let action = parts.next().unwrap_or("list");
    match action {
        "list" => match state.get_tool_infos(agent).await {
            Ok(tools) => format_tool_infos(tools),
            Err(error) => format!("Unable to list tools: {error}"),
        },
        "enable" | "disable" => {
            let Some(name) = parts.next() else {
                return "Usage: /tools [list|enable <name>|disable <name>]".to_string();
            };
            if parts.next().is_some() {
                return "Usage: /tools [list|enable <name>|disable <name>]".to_string();
            }
            match state
                .set_tool_enabled(agent, name, action == "enable")
                .await
            {
                Ok(receipt) => format!(
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
                ),
                Err(error) => format!("Unable to {action} tool '{name}': {error}"),
            }
        }
        _ => "Usage: /tools [list|enable <name>|disable <name>]".to_string(),
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

pub(crate) fn disabled_option(snapshot: &ToolControlSnapshot) -> Option<HashSet<String>> {
    (!snapshot.disabled_tools.is_empty()).then(|| snapshot.disabled_tools.clone())
}

#[cfg(test)]
pub(crate) async fn snapshot_disabled_tools(agent: &AgentHandle) -> HashSet<String> {
    agent
        .read(|agent| {
            echo_agent::agent::snapshot::AgentRunSnapshot::from_agent(agent)
                .tools
                .disabled_tools
                .clone()
        })
        .await
}

pub(crate) fn shared(service: &Arc<ToolControlService>) -> Arc<ToolControlService> {
    Arc::clone(service)
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
