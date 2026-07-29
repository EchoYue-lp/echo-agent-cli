use std::collections::HashSet;

use crate::tasks::task_runtime::InteractionMode;

pub(crate) const MAX_MODEL_VISIBLE_TOOL_RESULT_TOKENS: usize = 4_000;

const CONTROL_TOOLS: &[&str] = &["final_answer", "tool_search"];
const FILE_TOOLS: &[&str] = &[
    "read_file",
    "read_artifact",
    "write_file",
    "edit_file",
    "grep",
    "glob",
];
const DIRECTORY_TOOLS: &[&str] = &["list_dir"];
const EXECUTION_TOOLS: &[&str] = &["shell"];
const CODE_EXECUTION_TOOLS: &[&str] = &["run_code"];
const TASK_TOOLS: &[&str] = &["task_create", "task_update", "task_list", "task_execute"];
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
        TASK_TOOLS,
        WEB_SEARCH_TOOLS,
    ];
    const TASK: &[&[&str]] = &[
        CONTROL_TOOLS,
        FILE_TOOLS,
        EXECUTION_TOOLS,
        CODE_EXECUTION_TOOLS,
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
    let mut disabled = [
        "spawn_background_task",
        "check_task_status",
        "list_background_tasks",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<HashSet<_>>();

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
                "edit_file",
                "final_answer",
                "glob",
                "grep",
                "list_dir",
                "read_artifact",
                "read_file",
                "run_code",
                "shell",
                "task_create",
                "task_execute",
                "task_list",
                "task_update",
                "tool_search",
                "web_search",
                "write_file",
            ]
        );
        assert_eq!(
            sorted_visible(InteractionMode::Task),
            vec![
                "diff",
                "edit_file",
                "final_answer",
                "glob",
                "grep",
                "read_artifact",
                "read_file",
                "read_skill_resource",
                "run_code",
                "shell",
                "task_create",
                "task_execute",
                "task_list",
                "task_update",
                "tool_search",
                "web_search",
                "write_file",
            ]
        );
        assert_eq!(
            sorted_visible(InteractionMode::Auto),
            vec![
                "diff",
                "edit_file",
                "final_answer",
                "glob",
                "grep",
                "list_dir",
                "read_artifact",
                "read_file",
                "recall",
                "search_memory",
                "shell",
                "task_create",
                "task_execute",
                "task_list",
                "task_update",
                "tool_search",
                "web_fetch",
                "web_search",
                "write_file",
            ]
        );
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
                InteractionMode::Chat => (12, 18),
                InteractionMode::Task => (16, 22),
                InteractionMode::Auto => (18, 25),
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
}
