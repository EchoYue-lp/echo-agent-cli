//! EKO product command and receipt surface for framework tool visibility.
//!
//! The revisioned policy authority is `echo_agent::tools::control`; this module
//! owns EKO的必需依赖、显示分类/来源与命令回执，不另持注册或执行权威.

#[cfg(test)]
use std::collections::HashSet;

use serde::Serialize;
use ts_rs::TS;

use crate::agent_handle::AgentHandle;

pub use echo_agent::tools::control::{
    ToolControlMutation, ToolControlService, ToolControlSnapshot,
};

/// EKO产品拒绝信息适配；通用revisioned选择仍由原Framework service持有。
#[derive(Debug, thiserror::Error)]
pub enum ToolControlError {
    #[error("tool '{name}' is not registered")]
    NotRegistered { name: String },
    #[error("系统必需工具 {name} 不能关闭：{reason}")]
    Required { name: String, reason: String },
    #[error(transparent)]
    Framework(#[from] echo_agent::tools::control::ToolControlError),
}

/// 仅锁定EKO真实依赖，而不是所有内置或初始曝光工具。
/// 本地仍需此约束：防止产品自己的设置操作破坏唯一任务/必读原文路径。
pub(crate) fn required_tool_reason(name: &str) -> Option<&'static str> {
    match name {
        // tool_exposure::initial_visible_tools与ToolSearchTool是延迟能力发现唯一入口。
        "tool_search" => Some("按需发现并启用未在首轮展开的工具"),
        // infra/factory.rs的任务指导与tasks/task_runtime/register.rs共用同一revisioned graph。
        "task_create" | "task_update" | "task_list" | "task_execute" => {
            Some("维护并执行唯一的任务图，接纳目标、变更与真实执行结果")
        }
        // compact_context.rs的长Goal/artifact要求阅读回执；不能用猜测替代完整原文。
        "read_artifact" => Some("读取完整目标、约束原文和工具产物"),
        _ => None,
    }
}

pub(crate) fn validate_tool_choice(name: &str, enabled: bool) -> Result<(), ToolControlError> {
    if !enabled && let Some(reason) = required_tool_reason(name) {
        return Err(ToolControlError::Required {
            name: name.to_string(),
            reason: reason.to_string(),
        });
    }
    Ok(())
}

/// 明确的产品显示归类；名称只用于分类，不推断来源/必需性/执行许可。
pub(crate) fn tool_category(name: &str) -> Option<crate::types::ToolCategory> {
    use crate::types::ToolCategory::*;
    match name {
        "tool_search" | "task_create" | "task_update" | "task_list" | "task_execute"
        | "agent_tool" | "agent_spawn" | "agent_list" | "agent_inspect" | "agent_wait"
        | "agent_followup" | "agent_message" | "agent_interrupt" | "agent_resume"
        | "agent_handoff" | "agent_group" | "human_in_loop" => Some(TaskCollaboration),
        "read_file" | "read_artifact" | "apply_patch" | "grep" | "glob" | "list_dir"
        | "code_search" | "repo_map" | "diff" | "git_status" | "git_diff" | "git_log"
        | "git_blame" | "git_branch" | "git_commit" | "enter_worktree" | "exit_worktree"
        | "list_worktrees" => Some(FilesCode),
        "shell"
        | "run_code"
        | "wait"
        | "stop_cell"
        | "list_cells"
        | "watch_cell"
        | "interrupt_command_cell_watch" => Some(CommandsRuntime),
        "web_search"
        | "web_fetch"
        | "web_extract"
        | "arxiv_search"
        | "semantic_scholar_search"
        | "pubmed_search"
        | "crossref_search"
        | "browser_backend"
        | "browser_navigate"
        | "browser_snapshot"
        | "browser_click"
        | "browser_fill"
        | "browser_screenshot"
        | "browser_back"
        | "browser_reload"
        | "browser_tabs"
        | "browser_click_at"
        | "browser_type_at"
        | "browser_scroll"
        | "browser_console"
        | "browser_network"
        | "browser_dom_inspect"
        | "browser_performance_trace"
        | "browser_developer_mode" => Some(SearchBrowse),
        "view_image" | "image_fetch" | "pdf_extract" | "pdf_info" | "excel_read" | "excel_info"
        | "excel_profile" | "excel_to_csv" | "excel_write" | "word_read" | "word_info"
        | "word_structure" | "text_search" | "text_stats" | "text_process" | "text_export"
        | "generate_chart" | "sql_query" | "list_tables" | "describe_table" | "bibtex_generate" => {
            Some(DataDocuments)
        }
        "recall" | "search_memory" | "store_memory" | "load_skill" | "list_skills"
        | "read_skill" | "skill" => Some(MemorySkills),
        _ => None,
    }
}

/// 只投影真实连接的producer身份，不拆工具名猜MCP或插件来源。
/// ToolManager尚无非MCP来源合同，未提供的来源明确Unknown，不能假称Builtin。
pub(crate) fn mcp_tool_sources(
    agent: &echo_agent::agent::ReactAgent,
) -> std::collections::HashMap<String, crate::types::ToolSource> {
    use echo_agent::mcp::{McpServerOwner, McpToolAdapter};
    let mut sources = std::collections::HashMap::new();
    // 原manager快照保留owner；local_name不能重建插件连接身份。
    for (id, client) in agent.mcp_clients_by_id() {
        let plugin_id = match &id.owner {
            McpServerOwner::Direct => None,
            McpServerOwner::Plugin(plugin) => Some(plugin.clone()),
        };
        for tool in client.tools() {
            sources.insert(
                McpToolAdapter::exposed_name_for_identity(&id, &tool.name),
                crate::types::ToolSource::Mcp {
                    server: id.local_name.clone(),
                    tool: tool.name.clone(),
                    plugin_id: plugin_id.clone(),
                },
            );
        }
    }
    sources
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

/// 原文本通道与结构化控制共用此执行入口。
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

#[cfg(test)]
mod product_policy_tests {
    use super::*;

    #[test]
    fn system_dependencies_reject_disable_without_mutating_framework_policy()
    -> Result<(), ToolControlError> {
        let service = ToolControlService::default();
        for name in [
            "tool_search",
            "task_create",
            "task_update",
            "task_list",
            "task_execute",
            "read_artifact",
        ] {
            assert!(matches!(
                validate_tool_choice(name, false),
                Err(ToolControlError::Required { .. })
            ));
            validate_tool_choice(name, true)?;
        }
        assert!(service.snapshot()?.disabled_tools.is_empty());
        Ok(())
    }

    #[test]
    fn optional_builtin_is_not_required_and_unknown_metadata_is_not_guessed()
    -> Result<(), ToolControlError> {
        validate_tool_choice("shell", false)?;
        assert!(required_tool_reason("shell").is_none());
        assert!(required_tool_reason("task_create_custom").is_none());
        assert!(tool_category("mcp__random__shell").is_none());
        assert!(tool_category("new_dynamic_capability").is_none());
        Ok(())
    }

    #[test]
    fn registered_human_input_tool_is_optional_task_collaboration() {
        use echo_agent::tools::Tool;

        let provider = std::sync::Arc::new(echo_agent::human_loop::HumanLoopManager::default());
        let tool = echo_agent::tools::builtin::human_in_loop::HumanInLoop::new(provider);
        assert_eq!(
            tool_category(tool.name()),
            Some(crate::types::ToolCategory::TaskCollaboration)
        );
        assert!(required_tool_reason(tool.name()).is_none());
        assert!(tool_category("ask_user").is_none());
    }

    #[test]
    fn six_groups_do_not_change_registry_or_execution_policy() {
        assert_eq!(
            tool_category("task_list"),
            Some(crate::types::ToolCategory::TaskCollaboration)
        );
        assert_eq!(
            tool_category("read_file"),
            Some(crate::types::ToolCategory::FilesCode)
        );
        assert_eq!(
            tool_category("shell"),
            Some(crate::types::ToolCategory::CommandsRuntime)
        );
        assert_eq!(
            tool_category("browser_navigate"),
            Some(crate::types::ToolCategory::SearchBrowse)
        );
        assert_eq!(
            tool_category("pdf_info"),
            Some(crate::types::ToolCategory::DataDocuments)
        );
        assert_eq!(
            tool_category("search_memory"),
            Some(crate::types::ToolCategory::MemorySkills)
        );
    }
}
