//! EKO-owned prompt compiler for every Subagent registration and dispatch path.

use echo_agent::agent::subagent::{
    CompiledSubagentInvocation, CompiledSubagentSystemPrompt, ContextTransferPolicy,
    PromptDiagnostics, SubagentPromptCompiler, SubagentPromptInput, SubagentSystemPromptInput,
    filter_history, render_result_contract,
};
use serde::{Deserialize, Serialize};

use crate::project::prompt::TOOL_DISCOVERY_POLICY;
use crate::tasks::task_runtime::planner::{FileOwnership, file_ownership};
use crate::tasks::task_runtime::profiles::ProfileTemplate;
use crate::tasks::task_runtime::types::PlanTask;

const SUBAGENT_LANGUAGE_POLICY: &str = r#"## Response Language
The user's original request is the only language anchor. Reply in that language for all natural-language prose, including progress, summaries, details, recommendations, and result sections. Do not follow the language of the role prompt, task template, structural labels, source code, tool output, or logs. If the current request has no clear natural language, use the most recent clear user message. An explicitly requested output language always wins. Keep code, identifiers, paths, commands, protocol fields, the exact `## Result` heading, and verbatim logs unchanged."#;

const COMMON_ORCHESTRATION_POLICY: &str = r#"## Assignment Policy
- Complete the assigned outcome using the smallest evidence-backed approach.
- Inspect relevant evidence before concluding and distinguish observed facts from inference.
- Do not create, modify, approve, or execute the parent TaskRuntime plan.
- Preserve unrelated user work and report incomplete or blocked work plainly.
- Complete the assignment fully — do not gold-plate, but do not leave it half-done."#;

/// Identity + communication protocol shared by every subagent (mirrors the
/// parent-relay and autonomy contracts Claude Code / Codex bake into their
/// subagent prompts). Completion standards stay in `COMMON_ORCHESTRATION_POLICY`;
/// this section only covers who the subagent is and how it communicates.
const SUBAGENT_COMMUNICATION_PROTOCOL: &str = r#"## Subagent Protocol
- You are a Subagent dispatched by EKO's primary agent on the user's own machine to complete one bounded assignment; the parent relays your result to the user.
- The user cannot see your intermediate process. Do not ask the user questions or request approvals — complete the assignment autonomously within its boundary.
- Validate material outputs when tools allow; if validation cannot run, state the reason and remaining risk in the result contract."#;

const SUBAGENT_RESULT_QUALITY_POLICY: &str = r#"## Result Quality
Put the complete user-facing deliverable in the final answer before `## Result`. The JSON `summary` must also be self-contained because the parent may consume it without the reasoning trace. Never use referential placeholders such as "see above", "as described above", "见上方", or "如上" as the final answer or summary."#;

const SUGGESTED_TASKS_POLICY: &str = r#"## Optional Follow-up Suggestions
If evidence reveals genuinely required work outside the assignment, you may place this optional fenced JSON block before `## Result`:
```json
{"suggested_tasks":[{"title":"short title","description":"specific follow-up","kind":"investigation","agent_role":"explorer","dependencies":[],"why_needed":"why this is needed","risk":"low|medium|high"}]}
```
Suggest only independently executable work necessary for the parent goal. Suggestions never modify the plan and are not a substitute for completing the assignment."#;

/// Product compiler installed on the primary Agent and every registered Subagent.
#[derive(Debug, Default)]
pub struct EkoSubagentPromptCompiler;

impl EkoSubagentPromptCompiler {
    /// Compile a planned TaskRuntime invocation executed by the primary Agent.
    ///
    /// Registered Subagents receive these sections from their fixed system
    /// prompt. The primary Agent does not, so the same compiler appends them to
    /// this invocation exactly once.
    pub fn compile_primary_invocation(
        &self,
        input: &SubagentPromptInput<'_>,
    ) -> CompiledSubagentInvocation {
        let mut compiled = self.compile_invocation(input);
        compiled
            .diagnostics
            .record("suggested-tasks", "eko.optional_follow_up_policy");
        compiled
            .diagnostics
            .record("language", "eko.language_policy");
        compiled
            .diagnostics
            .record("result-quality", "eko.result_quality_policy");
        compiled
            .diagnostics
            .record("contract", "echo_agent.render_result_contract");
        compiled.task_input = [
            compiled.task_input,
            SUGGESTED_TASKS_POLICY.to_string(),
            SUBAGENT_LANGUAGE_POLICY.to_string(),
            SUBAGENT_RESULT_QUALITY_POLICY.to_string(),
            render_result_contract(),
        ]
        .join("\n\n");
        compiled
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyPrompt {
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "boundary", rename_all = "snake_case")]
pub enum TaskBoundaryPrompt {
    ReadOnly,
    ExclusiveScopedWrite,
    IsolatedUnknownScope { reason: String },
    PrimaryWorkspace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedTaskPrompt {
    pub workspace_root: Option<String>,
    pub domain_key: String,
    pub domain_label: String,
    pub execution_guidance: String,
    pub user_goal: Option<String>,
    pub task_title: String,
    pub task_description: String,
    pub dependency_summaries: Vec<DependencyPrompt>,
    pub files: Vec<String>,
    pub execution_checks: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub required_artifacts: Vec<String>,
    pub task_boundary: TaskBoundaryPrompt,
    pub can_delegate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "eko_prompt_kind", rename_all = "snake_case")]
pub enum EkoPromptPayload {
    PlannedTask { task: PlannedTaskPrompt },
}

impl EkoPromptPayload {
    pub fn planned_task(
        task: &PlanTask,
        dependency_summaries: &[(String, String)],
        can_delegate: bool,
        user_goal: Option<&str>,
        workspace_root: Option<&std::path::Path>,
    ) -> Self {
        let profile = ProfileTemplate::for_profile(task.domain_profile);
        let task_boundary = if task.kind.is_read_only() {
            TaskBoundaryPrompt::ReadOnly
        } else {
            match file_ownership(task) {
                FileOwnership::Known(_) => TaskBoundaryPrompt::ExclusiveScopedWrite,
                FileOwnership::Unknown { reason } => TaskBoundaryPrompt::IsolatedUnknownScope {
                    reason: reason.to_string(),
                },
                FileOwnership::ReadOnly => TaskBoundaryPrompt::PrimaryWorkspace,
            }
        };

        Self::PlannedTask {
            task: PlannedTaskPrompt {
                workspace_root: workspace_root.map(|path| path.display().to_string()),
                domain_key: profile.key.to_string(),
                domain_label: profile.label.to_string(),
                execution_guidance: profile.execution_guidance.to_string(),
                user_goal: user_goal
                    .map(str::trim)
                    .filter(|goal| !goal.is_empty())
                    .map(str::to_string),
                task_title: task.title.clone(),
                task_description: task.description.clone(),
                dependency_summaries: dependency_summaries
                    .iter()
                    .map(|(title, summary)| DependencyPrompt {
                        title: title.clone(),
                        summary: summary.clone(),
                    })
                    .collect(),
                files: task.files.clone(),
                execution_checks: task.execution_checks.clone(),
                acceptance_criteria: task.acceptance_criteria.clone(),
                required_artifacts: task.required_artifacts.clone(),
                task_boundary,
                can_delegate,
            },
        }
    }

    pub fn to_value(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(self).map_err(|error| error.to_string())
    }
}

impl SubagentPromptCompiler for EkoSubagentPromptCompiler {
    fn compile_system(
        &self,
        input: &SubagentSystemPromptInput<'_>,
    ) -> CompiledSubagentSystemPrompt {
        let mut diagnostics = PromptDiagnostics::default();
        diagnostics.record("role", "subagent_definition.markdown");
        diagnostics.record("common-rules", "eko.common_policy");
        diagnostics.record("protocol", "eko.subagent_protocol");
        diagnostics.record("tool-discovery", "eko.tool_discovery_policy");
        diagnostics.record("capability", "subagent_definition.frontmatter");
        diagnostics.record("suggested-tasks", "eko.optional_follow_up_policy");
        diagnostics.record("language", "eko.language_policy");
        diagnostics.record("result-quality", "eko.result_quality_policy");
        diagnostics.record("contract", "echo_agent.render_result_contract");

        let access = if input.readonly {
            "Read-only. Do not edit files, install dependencies, change repository state, or perform mutating side effects."
        } else {
            "Write-capable within the assignment and runtime-established workspace boundary."
        };
        let delegation = if input.can_delegate {
            "Tightly scoped child Subagent delegation is allowed. Summarize child results into this assignment and never delegate control of the global plan."
        } else {
            "Child Subagent delegation is disabled. Complete the assignment directly."
        };
        let capabilities = format!(
            "## Runtime Capabilities\n- Access: {access}\n- Isolation: {}\n- Delegation: {delegation}",
            input.isolation
        );

        let mut sections = vec![
            input.role_prompt.trim().to_string(),
            COMMON_ORCHESTRATION_POLICY.to_string(),
            SUBAGENT_COMMUNICATION_PROTOCOL.to_string(),
            TOOL_DISCOVERY_POLICY.to_string(),
            capabilities,
            SUGGESTED_TASKS_POLICY.to_string(),
            SUBAGENT_LANGUAGE_POLICY.to_string(),
            SUBAGENT_RESULT_QUALITY_POLICY.to_string(),
            render_result_contract(),
        ];
        // Static environment grounding (OS/arch/date) is registration-time
        // stable, so it belongs in the system prompt. Per-dispatch state
        // (working dir, workspace root) must NOT be compiled here — it changes
        // with worktree/workspace isolation and is rendered per invocation.
        if let Some(env) = input
            .environment
            .as_deref()
            .map(str::trim)
            .filter(|env| !env.is_empty())
        {
            diagnostics.record("environment", "eko.static_environment");
            sections.push(format!("## Environment\n{env}"));
        }

        CompiledSubagentSystemPrompt {
            system_prompt: sections.join("\n\n"),
            diagnostics,
        }
    }

    fn compile_invocation(&self, input: &SubagentPromptInput<'_>) -> CompiledSubagentInvocation {
        let history = if input.transfer_policy == ContextTransferPolicy::InheritStructured {
            input
                .parent_context
                .map(|context| filter_history(&context.messages, input.inherit_history))
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        if let Some(payload) = input.payload {
            match serde_json::from_value::<EkoPromptPayload>(payload.clone()) {
                Ok(EkoPromptPayload::PlannedTask { task }) => {
                    return compile_planned_invocation(task, history);
                }
                Err(error) => {
                    tracing::warn!(
                        subagent = input.agent_name,
                        error = %error,
                        "invalid EKO Subagent prompt payload; using direct framing"
                    );
                }
            }
        }

        compile_direct_invocation(input, history)
    }
}

fn compile_direct_invocation(
    input: &SubagentPromptInput<'_>,
    history: Vec<echo_core::llm::types::Message>,
) -> CompiledSubagentInvocation {
    let mut diagnostics = PromptDiagnostics::default();
    let mut sections = Vec::new();
    if let Some(goal) = input
        .parent_context
        .and_then(|context| context.parent_goal.as_deref())
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
    {
        diagnostics.record("user-goal", "parent_context.parent_goal");
        sections.push(format!("[user_request]\n{goal}\n[/user_request]"));
    }
    diagnostics.record("task", "dispatch_request.task");
    sections.push(format!(
        "[task_context]\nAssigned Subagent: {}\n\nTask:\n{}\n[/task_context]",
        input.agent_name,
        input.task.trim()
    ));
    // Defensive rendering: the direct-dispatch path currently inherits an empty
    // Explicit dispatch constraints (the `agent_tool` `constraints` parameter)
    // render for fresh-context dispatches too — they are the caller's task
    // boundary, not inherited conversation state.
    if !input.constraints.is_empty() {
        diagnostics.record("constraints", "dispatch_request.constraints");
        sections.push(format!(
            "[constraints]\n{}\n[/constraints]",
            input.constraints.join("\n")
        ));
    }
    CompiledSubagentInvocation {
        task_input: sections.join("\n\n"),
        history,
        diagnostics,
    }
}

fn compile_planned_invocation(
    task: PlannedTaskPrompt,
    history: Vec<echo_core::llm::types::Message>,
) -> CompiledSubagentInvocation {
    let mut diagnostics = PromptDiagnostics::default();
    let mut sections = Vec::new();
    if let Some(root) = task.workspace_root.as_deref() {
        diagnostics.record("workspace", "task_runtime.working_dir");
        sections.push(format!("[workspace]\n- root: {root}\n[/workspace]"));
    }

    let mut context = String::from("[task_context]\n");
    diagnostics.record("domain", "task.domain_profile");
    context.push_str(&format!(
        "Domain profile: {} ({})\nExecution standard: {}\n\n",
        task.domain_key, task.domain_label, task.execution_guidance
    ));
    if let Some(goal) = task.user_goal.as_deref() {
        diagnostics.record("user-goal", "task_run.goal");
        context.push_str(&format!("User goal:\n{goal}\n\n"));
    }
    diagnostics.record("task", "plan_task");
    context.push_str(&format!(
        "Task: {}\n\n{}\n\n",
        task.task_title, task.task_description
    ));
    append_dependencies(&mut context, &task.dependency_summaries, &mut diagnostics);
    append_list(
        &mut context,
        if matches!(task.task_boundary, TaskBoundaryPrompt::ReadOnly) {
            "Read targets:"
        } else {
            "Declared write targets:"
        },
        &task.files,
        "files",
        "plan_task.files",
        &mut diagnostics,
    );
    append_list(
        &mut context,
        "Execution checks:",
        &task.execution_checks,
        "execution-checks",
        "plan_task.execution_checks",
        &mut diagnostics,
    );
    append_list(
        &mut context,
        "Acceptance criteria:",
        &task.acceptance_criteria,
        "acceptance-criteria",
        "plan_task.acceptance_criteria",
        &mut diagnostics,
    );
    append_list(
        &mut context,
        "Required artifacts:",
        &task.required_artifacts,
        "required-artifacts",
        "plan_task.required_artifacts",
        &mut diagnostics,
    );

    diagnostics.record("boundary", "task_runtime.file_ownership");
    context.push_str(&format!(
        "Execution boundary: {}\n",
        render_task_boundary(&task.task_boundary)
    ));
    diagnostics.record("delegation", "task_runtime.delegation_policy");
    if task.can_delegate {
        context.push_str("Delegation: tightly scoped child Subagent help is allowed within this PlanTask. Child results must be summarized here; child Subagents must not control the global plan.\n");
    } else {
        context.push_str("Delegation: disabled for this PlanTask.\n");
    }
    context.push_str("[/task_context]");
    sections.push(context);

    CompiledSubagentInvocation {
        task_input: sections.join("\n\n"),
        history,
        diagnostics,
    }
}

fn append_dependencies(
    output: &mut String,
    dependencies: &[DependencyPrompt],
    diagnostics: &mut PromptDiagnostics,
) {
    if dependencies.is_empty() {
        return;
    }
    diagnostics.record("dependencies", "task_runtime.summary_chain");
    output.push_str("Context from completed upstream tasks:\n");
    for dependency in dependencies {
        output.push_str(&format!("- {}: {}\n", dependency.title, dependency.summary));
    }
    output.push('\n');
}

fn append_list(
    output: &mut String,
    heading: &str,
    items: &[String],
    diagnostic_id: &str,
    source: &str,
    diagnostics: &mut PromptDiagnostics,
) {
    if items.is_empty() {
        return;
    }
    diagnostics.record(diagnostic_id, source);
    output.push_str(heading);
    output.push('\n');
    for item in items {
        output.push_str(&format!("- {item}\n"));
    }
    output.push('\n');
}

fn render_task_boundary(boundary: &TaskBoundaryPrompt) -> String {
    match boundary {
        TaskBoundaryPrompt::ReadOnly => "READ-ONLY. Inspect available evidence with non-mutating tools. Do not alter files, dependencies, or repository state.".to_string(),
        TaskBoundaryPrompt::ExclusiveScopedWrite => "EXCLUSIVE SCOPED WRITE. Change only declared targets. Runtime validates the actual diff; preserve unrelated user work.".to_string(),
        TaskBoundaryPrompt::IsolatedUnknownScope { reason } => format!("ISOLATED UNKNOWN-SCOPE WRITE ({reason}). The runtime serializes this writer and establishes isolation. Keep changes narrow and report every changed file."),
        TaskBoundaryPrompt::PrimaryWorkspace => "PRIMARY WORKSPACE. Operate only within the assigned task and preserve unrelated user work.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::subagent::{ExecutionMode, SubagentContext};
    use echo_core::llm::types::Message;

    fn parent_context() -> SubagentContext {
        let mut context = SubagentContext::empty();
        context.parent_goal = Some("请核对提示词架构".to_string());
        context.messages = vec![
            Message::user("中文历史".to_string()),
            Message::tool_result(
                "call-1".to_string(),
                "read_file".to_string(),
                "tool".to_string(),
            ),
            Message::assistant("final history".to_string()),
        ];
        context
    }

    #[test]
    fn builtin_system_prompts_have_single_owned_sections() {
        let compiler = EkoSubagentPromptCompiler;
        for definition in crate::subagent_loader::discover_subagents(None, None) {
            let isolation = crate::subagent_loader::subagent_isolation(&definition);
            let compiled = compiler.compile_system(&SubagentSystemPromptInput {
                name: &definition.name,
                description: &definition.description,
                role_prompt: &definition.system_prompt,
                readonly: definition.readonly,
                can_delegate: definition.can_delegate,
                isolation,
                environment: None,
            });
            assert_eq!(compiled.diagnostics.count("role"), 1);
            assert_eq!(compiled.diagnostics.count("protocol"), 1);
            assert_eq!(compiled.diagnostics.count("tool-discovery"), 1);
            assert_eq!(compiled.diagnostics.count("language"), 1);
            assert_eq!(compiled.diagnostics.count("result-quality"), 1);
            assert_eq!(compiled.diagnostics.count("contract"), 1);
            assert_eq!(
                compiled
                    .system_prompt
                    .matches("## Efficient Code Discovery")
                    .count(),
                1
            );
            assert_eq!(
                compiled
                    .system_prompt
                    .matches("## Subagent Protocol")
                    .count(),
                1
            );
            assert_eq!(
                compiled.system_prompt.matches("## Environment").count(),
                0,
                "environment section must be absent when no environment is provided"
            );
        }
    }

    #[test]
    fn environment_section_rendered_once_when_provided() {
        let compiler = EkoSubagentPromptCompiler;
        let isolation = "context";
        let with_env = compiler.compile_system(&SubagentSystemPromptInput {
            name: "env-test",
            description: "d",
            role_prompt: "# Role\nbody",
            readonly: true,
            can_delegate: false,
            isolation,
            environment: Some("- OS: macos (aarch64)\n- Date: 2026-08-01".to_string()),
        });
        assert_eq!(with_env.diagnostics.count("environment"), 1);
        assert_eq!(with_env.system_prompt.matches("## Environment").count(), 1);
        assert!(with_env.system_prompt.contains("- OS: macos (aarch64)"));

        let without_env = compiler.compile_system(&SubagentSystemPromptInput {
            name: "env-test",
            description: "d",
            role_prompt: "# Role\nbody",
            readonly: true,
            can_delegate: false,
            isolation,
            environment: None,
        });
        assert_eq!(without_env.diagnostics.count("environment"), 0);
        assert_eq!(
            without_env.system_prompt.matches("## Environment").count(),
            0
        );
    }

    #[test]
    fn can_delegate_declaration_reaches_system_prompt_wording() {
        let compiler = EkoSubagentPromptCompiler;
        let isolation = "context";
        let enabled = compiler.compile_system(&SubagentSystemPromptInput {
            name: "delegate-test",
            description: "d",
            role_prompt: "# Role\nbody",
            readonly: false,
            can_delegate: true,
            isolation,
            environment: None,
        });
        assert!(
            enabled.system_prompt.contains("delegation is allowed"),
            "can_delegate=true must render the allowed wording, got: {}",
            enabled.system_prompt
        );

        let disabled = compiler.compile_system(&SubagentSystemPromptInput {
            name: "delegate-test",
            description: "d",
            role_prompt: "# Role\nbody",
            readonly: false,
            can_delegate: false,
            isolation,
            environment: None,
        });
        assert!(disabled.system_prompt.contains("delegation is disabled"));
    }

    #[test]
    fn dispatch_constraints_render_in_direct_invocation() {
        let compiler = EkoSubagentPromptCompiler;
        let context = parent_context();
        let compiled = compiler.compile_invocation(&SubagentPromptInput {
            agent_name: "explorer",
            task: "检查约束",
            mode: ExecutionMode::Sync,
            transfer_policy: ContextTransferPolicy::Fresh,
            parent_context: Some(&context),
            inherit_history: None,
            payload: None,
            constraints: &[
                "只读，不修改文件".to_string(),
                "不要碰 src/legacy".to_string(),
            ],
        });
        assert_eq!(compiled.diagnostics.count("constraints"), 1);
        assert!(compiled.task_input.contains("[constraints]"));
        assert!(compiled.task_input.contains("只读，不修改文件"));
        assert!(compiled.task_input.contains("src/legacy"));

        let empty = compiler.compile_invocation(&SubagentPromptInput {
            agent_name: "explorer",
            task: "检查约束",
            mode: ExecutionMode::Sync,
            transfer_policy: ContextTransferPolicy::Fresh,
            parent_context: None,
            inherit_history: None,
            payload: None,
            constraints: &[],
        });
        assert_eq!(empty.diagnostics.count("constraints"), 0);
        assert!(!empty.task_input.contains("[constraints]"));
    }

    #[test]
    fn direct_planned_and_fork_invocations_have_structured_cardinality() -> Result<(), String> {
        let compiler = EkoSubagentPromptCompiler;
        let context = parent_context();
        let task = PlanTask {
            title: "统一提示词".to_string(),
            description: "实现单一编译入口".to_string(),
            files: vec!["src/prompt.rs".to_string()],
            execution_checks: vec!["cargo test".to_string()],
            acceptance_criteria: vec!["目标只出现一次".to_string()],
            required_artifacts: vec!["src/prompt.rs".to_string()],
            ..PlanTask::default()
        };
        let payload = EkoPromptPayload::planned_task(
            &task,
            &[("依赖".to_string(), "证据".to_string())],
            false,
            context.parent_goal.as_deref(),
            Some(std::path::Path::new("/workspace")),
        )
        .to_value()?;

        for definition in crate::subagent_loader::discover_subagents(None, None) {
            let direct = compiler.compile_invocation(&SubagentPromptInput {
                agent_name: &definition.name,
                task: "检查直接派发",
                mode: ExecutionMode::Sync,
                transfer_policy: ContextTransferPolicy::Fresh,
                parent_context: Some(&context),
                inherit_history: None,
                payload: None,
                constraints: &[],
            });
            assert_eq!(direct.diagnostics.count("user-goal"), 1);
            assert_eq!(direct.diagnostics.count("task"), 1);
            assert!(direct.history.is_empty());

            let planned = compiler.compile_invocation(&SubagentPromptInput {
                agent_name: &definition.name,
                task: &task.description,
                mode: ExecutionMode::Fork,
                transfer_policy: ContextTransferPolicy::Fresh,
                parent_context: Some(&context),
                inherit_history: None,
                payload: Some(&payload),
                constraints: &[],
            });
            assert_eq!(planned.diagnostics.count("user-goal"), 1);
            assert_eq!(planned.diagnostics.count("task"), 1);
            assert_eq!(planned.diagnostics.count("dependencies"), 1);
            assert!(planned.history.is_empty());

            let fork = compiler.compile_invocation(&SubagentPromptInput {
                agent_name: &definition.name,
                task: "检查 fork",
                mode: ExecutionMode::Fork,
                transfer_policy: ContextTransferPolicy::InheritStructured,
                parent_context: Some(&context),
                inherit_history: Some(2),
                payload: None,
                constraints: &[],
            });
            assert_eq!(fork.history.len(), 2);
            assert!(
                fork.history
                    .iter()
                    .all(|message| message.tool_call_id.is_none())
            );

            let teammate = compiler.compile_invocation(&SubagentPromptInput {
                agent_name: &definition.name,
                task: "检查 teammate",
                mode: ExecutionMode::Teammate,
                transfer_policy: ContextTransferPolicy::Fresh,
                parent_context: Some(&context),
                inherit_history: Some(2),
                payload: None,
                constraints: &[],
            });
            assert!(teammate.history.is_empty());
        }
        Ok(())
    }

    #[test]
    fn primary_planned_invocation_owns_missing_system_sections_once() -> Result<(), String> {
        let compiler = EkoSubagentPromptCompiler;
        let task = PlanTask {
            title: "运行验证".to_string(),
            description: "执行测试".to_string(),
            ..PlanTask::default()
        };
        let payload =
            EkoPromptPayload::planned_task(&task, &[], false, Some("完成验证"), None).to_value()?;
        let compiled = compiler.compile_primary_invocation(&SubagentPromptInput {
            agent_name: "primary",
            task: &task.description,
            mode: ExecutionMode::Sync,
            transfer_policy: ContextTransferPolicy::Fresh,
            parent_context: None,
            inherit_history: None,
            payload: Some(&payload),
            constraints: &[],
        });

        assert_eq!(compiled.diagnostics.count("user-goal"), 1);
        assert_eq!(compiled.diagnostics.count("language"), 1);
        assert_eq!(compiled.diagnostics.count("result-quality"), 1);
        assert_eq!(compiled.diagnostics.count("contract"), 1);
        assert_eq!(
            compiled.task_input.matches("## Response Language").count(),
            1
        );
        assert_eq!(compiled.task_input.matches("## Result Quality").count(), 1);
        assert_eq!(
            compiled
                .task_input
                .lines()
                .filter(|line| *line == "## Result")
                .count(),
            1
        );
        Ok(())
    }
}
