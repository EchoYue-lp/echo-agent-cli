//! Tool dynamic visibility by mode
//!
//! Filters the tool list based on the current Agent mode, reducing
//! irrelevant tool noise and improving Agent decision quality.

use super::modes::AgentMode;
use echo_agent::prelude::ToolDefinition;

/// Filter tool list by mode.
///
/// - General mode: all tools visible
/// - Other modes: only mode-recommended tools + essential tools
pub fn filter_tools_by_mode(mode: &AgentMode, tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    let recommended = super::modes::recommended_tools(mode);

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
