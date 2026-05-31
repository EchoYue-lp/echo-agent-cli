//! Tool dynamic visibility by mode
//!
//! Filters the tool list based on the current Agent mode, reducing
//! irrelevant tool noise and improving Agent decision quality.

use echo_agent::agent::AgentMode;
use echo_agent::prelude::ToolDefinition;

/// Filter tool list by mode.
///
/// - General mode: all tools visible
/// - Other modes: only mode-recommended tools + essential tools
pub fn filter_tools_by_mode(mode: &AgentMode, tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    let recommended = recommended_tools_for_mode(mode);

    // General mode: all tools visible
    if recommended.is_empty() {
        return tools;
    }

    tools
        .into_iter()
        .filter(|t| {
            let name = &t.function.name;
            // Always include essential tools
            is_essential_tool(name) || recommended.contains(&name.as_str())
        })
        .collect()
}

/// Essential tools that are always visible regardless of mode.
fn is_essential_tool(name: &str) -> bool {
    matches!(
        name,
        "final_answer" | "think" | "answer" | "memory_save" | "memory_search"
    )
}

/// Recommended tool list for each mode.
fn recommended_tools_for_mode(mode: &AgentMode) -> Vec<&'static str> {
    match mode {
        AgentMode::Coding => vec![
            "shell", "read_file", "write_file", "edit_file", "create_file",
            "glob", "grep", "diff", "git",
        ],
        AgentMode::Research => vec![
            "arxiv_search", "semantic_scholar_search", "pdf_fetch",
            "bibtex_generate", "web_fetch", "web_search",
            "read_file", "write_file",
        ],
        AgentMode::Data => vec![
            "shell", "read_file", "write_file", "data_analyze",
            "chart", "excel_read", "csv_read",
        ],
        AgentMode::Writing => vec![
            "read_file", "write_file", "edit_file", "web_search",
        ],
        AgentMode::General => vec![], // empty = all visible
        _ => vec![],
    }
}

/// Get the recommended tool list for a mode (public interface).
pub fn get_recommended_tools(mode: &AgentMode) -> Vec<&'static str> {
    recommended_tools_for_mode(mode)
}
