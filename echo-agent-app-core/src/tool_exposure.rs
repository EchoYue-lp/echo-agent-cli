use std::collections::HashSet;

use crate::tasks::task_runtime::DomainProfile;

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

pub(crate) fn invocation_rollout() -> ToolOptimizationRollout {
    ToolOptimizationRollout {
        deferred_schemas: true,
        cursor_pagination: true,
        bounded_results: true,
        content_free_telemetry: true,
    }
}

pub(crate) fn record_schema_budget(
    definitions: &[echo_agent::llm::types::ToolDefinition],
    visible: &HashSet<String>,
) {
    let rollout = invocation_rollout();
    let selected = definitions
        .iter()
        .filter(|definition| visible.contains(&definition.function.name))
        .cloned()
        .collect::<Vec<_>>();
    match echo_agent::tools::ToolManager::schema_stats_for(&selected) {
        Ok(stats) => tracing::info!(
            target: "eko::tool_budget",
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
const WEB_SEARCH_TOOLS: &[&str] = &["web_search"];
const WEB_FETCH_TOOLS: &[&str] = &["web_fetch"];
const REPOSITORY_INSPECTION_TOOLS: &[&str] = &["diff"];
const MEMORY_TOOLS: &[&str] = &["recall", "search_memory"];
const ACADEMIC_RESEARCH_TOOLS: &[&str] = &[
    "arxiv_search",
    "semantic_scholar_search",
    "pubmed_search",
    "clinical_trials_search",
];

fn initial_groups() -> &'static [&'static [&'static str]] {
    const INITIAL: &[&[&str]] = &[
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
    INITIAL
}

fn task_run_groups() -> &'static [&'static [&'static str]] {
    const TASK_RUN: &[&[&str]] = &[
        CONTROL_TOOLS,
        FILE_TOOLS,
        EXECUTION_TOOLS,
        CODE_EXECUTION_TOOLS,
        CELL_TOOLS,
        TASK_TOOLS,
        WEB_SEARCH_TOOLS,
        REPOSITORY_INSPECTION_TOOLS,
    ];
    TASK_RUN
}

fn visible_from_groups(groups: &[&[&str]], registered: &[String]) -> HashSet<String> {
    let registered = registered
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    groups
        .iter()
        .flat_map(|group| group.iter())
        .filter(|name| registered.contains(**name))
        .map(|name| (*name).to_string())
        .collect()
}

/// EKO selects an invocation-scoped first-turn surface from the framework's
/// single registry. Browser, MCP, extended memory, and domain tools remain in
/// the catalog and are activated through `tool_search` when needed.
pub(crate) fn initial_visible_tools(registered: &[String]) -> HashSet<String> {
    if !invocation_rollout().deferred_schemas {
        return registered.iter().cloned().collect();
    }
    visible_from_groups(initial_groups(), registered)
}

/// Select the first-turn surface for an explicit TaskRun or Subagent attempt.
/// TaskRun work receives the code-execution schema immediately; ordinary chat
/// can still activate the registered capability explicitly through tool_search.
pub(crate) fn initial_visible_tools_for_task_run(registered: &[String]) -> HashSet<String> {
    if !invocation_rollout().deferred_schemas {
        return registered.iter().cloned().collect();
    }
    visible_from_groups(task_run_groups(), registered)
}

/// Select the first-turn surface for one canonical TaskRun.
///
/// Academic research runs expose their provider search tools immediately so
/// a background `create_complex_task` driver can retrieve and persist evidence
/// without a separate foreground `tool_search` turn. Other domain tools remain
/// deferred and all non-research profiles keep the normal mode budget.
pub(crate) fn initial_visible_tools_for_profile(
    domain_profile: DomainProfile,
    registered: &[String],
) -> HashSet<String> {
    let mut visible = initial_visible_tools_for_task_run(registered);
    if domain_profile == DomainProfile::AcademicResearch {
        let registered = registered
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        visible.extend(
            ACADEMIC_RESEARCH_TOOLS
                .iter()
                .filter(|name| registered.contains(**name))
                .map(|name| (*name).to_string()),
        );
    }
    visible
}

pub(crate) fn disabled_tools() -> HashSet<String> {
    HashSet::new()
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
            CELL_TOOLS,
            TASK_TOOLS,
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

    fn sorted_visible() -> Vec<String> {
        let mut names = initial_visible_tools(&policy_tool_names())
            .into_iter()
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn invocation_capability_snapshot_is_stable() {
        assert_eq!(
            sorted_visible(),
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
    fn explicit_task_run_surface_keeps_code_execution_bounded() {
        let mut registered = policy_tool_names();
        registered.push("run_code".to_string());
        let visible = initial_visible_tools_for_task_run(&registered);
        assert!(visible.contains("run_code"));
        assert!(!initial_visible_tools(&registered).contains("run_code"));
    }

    #[test]
    fn academic_background_profile_exposes_only_registered_research_providers() {
        let mut registered = policy_tool_names();
        registered.extend([
            "semantic_scholar_search".to_string(),
            "pubmed_search".to_string(),
        ]);
        let academic =
            initial_visible_tools_for_profile(DomainProfile::AcademicResearch, &registered);
        let general = initial_visible_tools_for_profile(DomainProfile::General, &registered);

        assert!(academic.contains("semantic_scholar_search"));
        assert!(academic.contains("pubmed_search"));
        assert!(!academic.contains("arxiv_search"));
        assert!(!general.contains("semantic_scholar_search"));
    }

    #[test]
    fn invocation_optimizations_are_enabled() {
        let rollout = invocation_rollout();
        assert!(rollout.deferred_schemas);
        assert!(rollout.cursor_pagination);
        assert!(rollout.bounded_results);
        assert!(rollout.content_free_telemetry);
    }

    #[tokio::test]
    async fn invocation_schema_budget_does_not_regress() -> anyhow::Result<()> {
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

        let visible = initial_visible_tools(&registered);
        let selected = definitions
            .iter()
            .filter(|definition| visible.contains(&definition.function.name))
            .cloned()
            .collect::<Vec<_>>();
        let stats = ToolManager::schema_stats_for(&selected)?;
        eprintln!("invocation tool schema baseline: {stats:?}");
        assert!((15..=27).contains(&stats.tool_count));
        assert!(stats.schema_bytes <= 16_000);
        assert!(stats.estimated_tokens <= 4_000);
        let task_visible = initial_visible_tools_for_task_run(&registered);
        let task_selected = definitions
            .iter()
            .filter(|definition| task_visible.contains(&definition.function.name))
            .cloned()
            .collect::<Vec<_>>();
        let task_stats = ToolManager::schema_stats_for(&task_selected)?;
        eprintln!("task-run tool schema baseline: {task_stats:?}");
        assert!(task_stats.schema_bytes <= 16_000);
        assert!(task_stats.estimated_tokens <= 4_000);
        assert_eq!(MAX_MODEL_VISIBLE_TOOL_RESULT_TOKENS, 4_000);
        Ok(())
    }

    #[test]
    fn invocation_keeps_the_authoritative_task_graph_tools_visible() {
        let registered = policy_tool_names();
        let visible = initial_visible_tools(&registered);
        let disabled = disabled_tools();
        for tool in ["task_create", "task_update", "task_list", "task_execute"] {
            assert!(visible.contains(tool));
            assert!(!disabled.contains(tool));
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
        let visible = initial_visible_tools(&registered);
        for tool in echo_agent::mcp::MCP_RESOURCE_TOOL_NAMES {
            assert!(!visible.contains(tool));
        }
    }
}
