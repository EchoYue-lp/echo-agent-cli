use std::collections::HashSet;

use crate::tasks::task_runtime::InteractionMode;

pub(crate) const MAX_MODEL_VISIBLE_TOOL_RESULT_TOKENS: usize = 4_000;
#[cfg(test)]
const MAX_REGISTERED_BUILTIN_TOOLS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolOptimizationRollout {
    pub deferred_schemas: bool,
    pub cursor_pagination: bool,
    pub bounded_results: bool,
    pub content_free_telemetry: bool,
}

pub(crate) fn rollout_for_mode(_interaction_mode: InteractionMode) -> ToolOptimizationRollout {
    ToolOptimizationRollout {
        deferred_schemas: true,
        cursor_pagination: true,
        bounded_results: true,
        content_free_telemetry: true,
    }
}

pub(crate) fn record_mode_schema_budget(
    interaction_mode: InteractionMode,
    definitions: &[echo_agent::llm::types::ToolDefinition],
    visible: &HashSet<String>,
) {
    let rollout = rollout_for_mode(interaction_mode);
    let selected = definitions
        .iter()
        .filter(|definition| visible.contains(&definition.function.name))
        .cloned()
        .collect::<Vec<_>>();
    match echo_agent::tools::ToolManager::schema_stats_for(&selected) {
        Ok(stats) => tracing::info!(
            target: "eko::tool_budget",
            mode = interaction_mode.as_str(),
            tool_count = stats.tool_count,
            schema_bytes = stats.schema_bytes,
            schema_estimated_tokens = stats.estimated_tokens,
            deferred_schemas = rollout.deferred_schemas,
            cursor_pagination = rollout.cursor_pagination,
            bounded_results = rollout.bounded_results,
            content_free_telemetry = rollout.content_free_telemetry,
            "interaction tool budget"
        ),
        Err(error) => tracing::warn!(
            target: "eko::tool_budget",
            mode = interaction_mode.as_str(),
            %error,
            "failed to measure interaction tool budget"
        ),
    }
}

const CONTROL_TOOLS: &[&str] = &["final_answer", "tool_search"];
const FILE_TOOLS: &[&str] = &["read_file", "read_artifact", "apply_patch", "grep", "glob"];
const DIRECTORY_TOOLS: &[&str] = &["list_dir"];
const EXECUTION_TOOLS: &[&str] = &["shell"];
const CODE_EXECUTION_TOOLS: &[&str] = &["run_code"];
const TASK_TOOLS: &[&str] = &["task_create", "task_update", "task_list", "task_execute"];
// Background command cells: long-poll primitives for commands started with
// shell(background=true). They share the foreground shell safety classifier
// and are not a second task API.
const CELL_TOOLS: &[&str] = &[
    "wait",
    "stop_cell",
    "list_cells",
    "watch_cell",
    "interrupt_awaiter",
];
const SKILL_RESOURCE_TOOLS: &[&str] = &["read_skill_resource"];
const WEB_SEARCH_TOOLS: &[&str] = &["web_search"];
const WEB_FETCH_TOOLS: &[&str] = &["web_fetch"];
const REPOSITORY_INSPECTION_TOOLS: &[&str] = &["diff"];
const MEMORY_TOOLS: &[&str] = &["recall", "search_memory"];

fn groups_for_mode(interaction_mode: InteractionMode) -> &'static [&'static [&'static str]] {
    const CHAT: &[&[&str]] = &[
        CONTROL_TOOLS,
        FILE_TOOLS,
        DIRECTORY_TOOLS,
        EXECUTION_TOOLS,
        CODE_EXECUTION_TOOLS,
        CELL_TOOLS,
        TASK_TOOLS,
        WEB_SEARCH_TOOLS,
    ];
    const TASK: &[&[&str]] = &[
        CONTROL_TOOLS,
        FILE_TOOLS,
        EXECUTION_TOOLS,
        CODE_EXECUTION_TOOLS,
        CELL_TOOLS,
        TASK_TOOLS,
        SKILL_RESOURCE_TOOLS,
        WEB_SEARCH_TOOLS,
        REPOSITORY_INSPECTION_TOOLS,
    ];
    const AUTO: &[&[&str]] = &[
        CONTROL_TOOLS,
        FILE_TOOLS,
        DIRECTORY_TOOLS,
        EXECUTION_TOOLS,
        CELL_TOOLS,
        TASK_TOOLS,
        WEB_SEARCH_TOOLS,
        WEB_FETCH_TOOLS,
        REPOSITORY_INSPECTION_TOOLS,
        MEMORY_TOOLS,
    ];

    match interaction_mode {
        InteractionMode::Chat => CHAT,
        InteractionMode::Task => TASK,
        InteractionMode::Auto => AUTO,
    }
}

/// EKO selects an invocation-scoped first-turn surface from the framework's
/// single registry. Browser, MCP, extended memory, and domain tools remain in
/// the catalog and are activated through `tool_search` when needed.
pub(crate) fn initial_visible_tools(
    interaction_mode: InteractionMode,
    registered: &[String],
) -> HashSet<String> {
    if !rollout_for_mode(interaction_mode).deferred_schemas {
        return registered.iter().cloned().collect();
    }
    let registered = registered
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    groups_for_mode(interaction_mode)
        .iter()
        .flat_map(|group| group.iter())
        .filter(|name| registered.contains(**name))
        .map(|name| (*name).to_string())
        .collect()
}

pub(crate) fn disabled_tools_for_mode(interaction_mode: InteractionMode) -> HashSet<String> {
    let mut disabled = HashSet::new();

    match interaction_mode {
        InteractionMode::Chat => disabled.extend(
            ["create_complex_task", "check_run_status", "cancel_run"]
                .into_iter()
                .map(str::to_string),
        ),
        InteractionMode::Task => disabled.extend(
            [
                "agent_tool",
                "create_complex_task",
                "check_run_status",
                "cancel_run",
            ]
            .into_iter()
            .map(str::to_string),
        ),
        InteractionMode::Auto => {
            disabled.insert("agent_tool".to_string());
        }
    }

    disabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::{Agent, ReactAgentBuilder};
    use echo_agent::tools::ToolManager;

    fn policy_tool_names() -> Vec<String> {
        [
            CONTROL_TOOLS,
            FILE_TOOLS,
            DIRECTORY_TOOLS,
            EXECUTION_TOOLS,
            CODE_EXECUTION_TOOLS,
            CELL_TOOLS,
            TASK_TOOLS,
            SKILL_RESOURCE_TOOLS,
            WEB_SEARCH_TOOLS,
            WEB_FETCH_TOOLS,
            REPOSITORY_INSPECTION_TOOLS,
            MEMORY_TOOLS,
        ]
        .into_iter()
        .flat_map(|group| group.iter())
        .map(|name| (*name).to_string())
        .collect()
    }

    fn sorted_visible(mode: InteractionMode) -> Vec<String> {
        let mut names = initial_visible_tools(mode, &policy_tool_names())
            .into_iter()
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn mode_exposure_snapshots_are_stable() {
        assert_eq!(
            sorted_visible(InteractionMode::Chat),
            vec![
                "apply_patch",
                "final_answer",
                "glob",
                "grep",
                "interrupt_awaiter",
                "list_cells",
                "list_dir",
                "read_artifact",
                "read_file",
                "run_code",
                "shell",
                "stop_cell",
                "task_create",
                "task_execute",
                "task_list",
                "task_update",
                "tool_search",
                "wait",
                "watch_cell",
                "web_search",
            ]
        );
        assert_eq!(
            sorted_visible(InteractionMode::Task),
            vec![
                "apply_patch",
                "diff",
                "final_answer",
                "glob",
                "grep",
                "interrupt_awaiter",
                "list_cells",
                "read_artifact",
                "read_file",
                "read_skill_resource",
                "run_code",
                "shell",
                "stop_cell",
                "task_create",
                "task_execute",
                "task_list",
                "task_update",
                "tool_search",
                "wait",
                "watch_cell",
                "web_search",
            ]
        );
        assert_eq!(
            sorted_visible(InteractionMode::Auto),
            vec![
                "apply_patch",
                "diff",
                "final_answer",
                "glob",
                "grep",
                "interrupt_awaiter",
                "list_cells",
                "list_dir",
                "read_artifact",
                "read_file",
                "recall",
                "search_memory",
                "shell",
                "stop_cell",
                "task_create",
                "task_execute",
                "task_list",
                "task_update",
                "tool_search",
                "wait",
                "watch_cell",
                "web_fetch",
                "web_search",
            ]
        );
    }

    #[test]
    fn optimizations_are_enabled_after_chat_task_auto_rollout() {
        for mode in [
            InteractionMode::Chat,
            InteractionMode::Task,
            InteractionMode::Auto,
        ] {
            let rollout = rollout_for_mode(mode);
            assert!(rollout.deferred_schemas);
            assert!(rollout.cursor_pagination);
            assert!(rollout.bounded_results);
            assert!(rollout.content_free_telemetry);
        }
    }

    #[tokio::test]
    async fn mode_schema_budgets_do_not_regress() -> anyhow::Result<()> {
        let mut agent = ReactAgentBuilder::new()
            .model("test-model")
            .name("audit")
            .system_prompt("test")
            .enable_tools()
            .enable_memory()
            .enable_subagent()
            .enable_human_in_loop()
            .build()?;
        let skill_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../skills");
        agent.load_skills_from_dir(skill_root).await?;
        let agent = crate::agent_handle::AgentHandle::new(agent);
        let task_store =
            std::sync::Arc::new(crate::tasks::task_runtime::store::TaskRuntimeStore::new()?);
        crate::tasks::task_runtime::register_task_tools_on_agent(&agent, task_store).await;
        let (definitions, registered) = agent
            .read(|agent| (Agent::tool_definitions(agent), agent.tool_names()))
            .await;
        eprintln!("registered built-in tool catalog: {}", registered.len());

        assert!(registered.contains(&"apply_patch".to_string()));
        assert!(registered.contains(&"view_image".to_string()));
        for superseded in [
            "edit_file",
            "write_file",
            "append_file",
            "create_file",
            "delete_file",
            "update_file",
            "move_file",
            "analyze_image",
        ] {
            assert!(
                !registered.contains(&superseded.to_string()),
                "superseded default tool remains registered: {superseded}"
            );
        }
        assert!(
            registered.len() <= MAX_REGISTERED_BUILTIN_TOOLS,
            "registered built-in catalog exceeded {MAX_REGISTERED_BUILTIN_TOOLS}: {}",
            registered.len()
        );
        for removed in [
            "read_data",
            "filter_data",
            "aggregate_data",
            "data_stats",
            "transform_data",
            "export_data",
            "profile_data",
            "topn_data",
            "contribution_data",
            "bin_data",
            "ratio_data",
            "multi_read_data",
            "join_data",
            "correlate_data",
            "pivot_data",
            "missing_value_analysis",
            "outlier_detection",
            "consistency_check",
            "exploratory_statistics",
            "excel_load",
        ] {
            assert!(
                !registered.iter().any(|tool| tool == removed),
                "script-first EKO build must not register legacy data tool '{removed}'"
            );
        }

        for mode in [
            InteractionMode::Chat,
            InteractionMode::Task,
            InteractionMode::Auto,
        ] {
            let visible = initial_visible_tools(mode, &registered);
            let selected = definitions
                .iter()
                .filter(|definition| visible.contains(&definition.function.name))
                .cloned()
                .collect::<Vec<_>>();
            let stats = ToolManager::schema_stats_for(&selected)?;
            eprintln!("{} tool schema baseline: {stats:?}", mode.as_str());
            let (minimum_tools, maximum_tools) = match mode {
                InteractionMode::Chat => (13, 20),
                InteractionMode::Task => (14, 24),
                InteractionMode::Auto => (16, 27),
            };
            assert!(
                stats.tool_count >= minimum_tools && stats.tool_count <= maximum_tools,
                "{} initial tool count exceeded its range: {stats:?}",
                mode.as_str()
            );
            assert!(
                stats.schema_bytes <= 16_000,
                "{} initial schema byte budget exceeded: {stats:?}",
                mode.as_str()
            );
            assert!(
                stats.estimated_tokens <= 4_000,
                "{} initial schema budget exceeded: {stats:?}",
                mode.as_str()
            );
            assert_eq!(MAX_MODEL_VISIBLE_TOOL_RESULT_TOKENS, 4_000);
        }
        Ok(())
    }

    #[test]
    fn modes_keep_the_authoritative_task_graph_tools_visible() {
        let registered = policy_tool_names();
        for mode in [
            InteractionMode::Chat,
            InteractionMode::Task,
            InteractionMode::Auto,
        ] {
            let visible = initial_visible_tools(mode, &registered);
            let disabled = disabled_tools_for_mode(mode);
            for tool in ["task_create", "task_update", "task_list", "task_execute"] {
                assert!(visible.contains(tool));
                assert!(!disabled.contains(tool));
            }
        }
    }

    #[test]
    fn mcp_resource_tools_stay_in_the_progressive_catalog() {
        let mut registered = policy_tool_names();
        registered.extend(
            echo_agent::mcp::MCP_RESOURCE_TOOL_NAMES
                .into_iter()
                .map(str::to_string),
        );
        for mode in [
            InteractionMode::Chat,
            InteractionMode::Task,
            InteractionMode::Auto,
        ] {
            let visible = initial_visible_tools(mode, &registered);
            for tool in echo_agent::mcp::MCP_RESOURCE_TOOL_NAMES {
                assert!(!visible.contains(tool));
            }
        }
    }
}
