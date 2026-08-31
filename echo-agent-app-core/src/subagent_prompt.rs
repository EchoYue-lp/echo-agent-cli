//! EKO-owned prompt compiler for every Subagent registration and dispatch path.

use echo_agent::subagent::{
    CompiledSubagentInvocation, CompiledSubagentSystemPrompt, ContextTransferPolicy, PromptActor,
    PromptDiagnostics, SubagentAccessMode, SubagentExecutionBoundary, SubagentInvocation,
    SubagentPromptCompiler, SubagentSystemPromptInput, ToolCapabilitySnapshot,
    compiled_current_message, filter_history, remove_duplicate_current_message,
    render_result_contract,
};
use serde::{Deserialize, Serialize};

use crate::project::prompt::TOOL_DISCOVERY_POLICY;
use crate::tasks::task_runtime::planner::{FileOwnership, file_ownership};
use crate::tasks::task_runtime::profiles::ProfileTemplate;
use crate::tasks::task_runtime::types::PlanTask;

const PRIMARY_PROFILE_BEGIN: &str = "<!-- eko:primary-task-profile:begin -->";
const PRIMARY_PROFILE_END: &str = "<!-- eko:primary-task-profile:end -->";

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

const PRIMARY_TASK_RUNTIME_POLICY: &str = r#"## TaskRuntime Assignment Mode
The assignment, follow-up suggestion, result-quality, and Result Contract rules below apply only when the current invocation contains a `[task_context]` block. Ordinary chat replies keep the primary Agent's normal response contract."#;

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
    /// The primary Agent receives its stable TaskRuntime policy from
    /// `compile_system`; this method generates only dynamic invocation messages.
    pub fn compile_primary_invocation(
        &self,
        input: &SubagentInvocation<'_>,
    ) -> CompiledSubagentInvocation {
        self.compile_invocation(input)
    }
}

pub fn refresh_primary_system_prompt(
    agent: &mut echo_agent::agent::ReactAgent,
    disabled_tools: &std::collections::HashSet<String>,
) {
    use echo_agent::agent::Agent;

    let current = agent.current_system_prompt();
    let (base, trailing) = split_primary_profile(&current);
    let mut effective_disabled = agent.disabled_tool_names();
    effective_disabled.extend(disabled_tools.iter().cloned());
    let capabilities =
        ToolCapabilitySnapshot::from_definitions(&agent.tool_definitions(), &effective_disabled);
    let compiler = EkoSubagentPromptCompiler;
    let compiled = compiler.compile_system(&SubagentSystemPromptInput {
        actor: PromptActor::Primary,
        name: agent.config().get_agent_name(),
        description: "EKO primary Agent executing a TaskRuntime assignment",
        role_prompt: &base,
        capabilities: &capabilities,
        boundary: SubagentExecutionBoundary {
            access: SubagentAccessMode::Write,
            isolation: "runtime-selected",
            can_delegate: true,
        },
    });
    let suffix = compiled
        .system_prompt
        .strip_prefix(&base)
        .unwrap_or(compiled.system_prompt.as_str())
        .trim_start();
    let trailing = trailing.trim();
    let prompt = if trailing.is_empty() {
        format!("{base}\n\n{PRIMARY_PROFILE_BEGIN}\n{suffix}\n{PRIMARY_PROFILE_END}")
    } else {
        format!("{base}\n\n{PRIMARY_PROFILE_BEGIN}\n{suffix}\n{PRIMARY_PROFILE_END}\n\n{trailing}")
    };
    agent.replace_system_prompt(prompt);
}

fn split_primary_profile(prompt: &str) -> (String, String) {
    if let Some(begin) = prompt.find(PRIMARY_PROFILE_BEGIN) {
        let before = prompt.get(..begin).unwrap_or_default().trim_end();
        let after_begin = prompt
            .get(begin.saturating_add(PRIMARY_PROFILE_BEGIN.len())..)
            .unwrap_or_default();
        let trailing = after_begin
            .find(PRIMARY_PROFILE_END)
            .and_then(|end| after_begin.get(end.saturating_add(PRIMARY_PROFILE_END.len())..))
            .unwrap_or_default()
            .trim_start();
        return (before.to_string(), trailing.to_string());
    }
    (prompt.trim_end().to_string(), String::new())
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
    pub domain_key: String,
    pub domain_label: String,
    pub execution_guidance: String,
    pub dependency_summaries: Vec<DependencyPrompt>,
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
                domain_key: profile.key.to_string(),
                domain_label: profile.label.to_string(),
                execution_guidance: profile.execution_guidance.to_string(),
                dependency_summaries: dependency_summaries
                    .iter()
                    .map(|(title, summary)| DependencyPrompt {
                        title: title.clone(),
                        summary: summary.clone(),
                    })
                    .collect(),
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
        diagnostics.record(
            "role",
            if input.actor == PromptActor::Primary {
                "primary_agent.system_prompt"
            } else {
                "subagent_definition.markdown"
            },
        );
        diagnostics.record("common-rules", "eko.common_policy");
        if input.actor == PromptActor::Subagent {
            diagnostics.record("protocol", "eko.subagent_protocol");
        }
        diagnostics.record("tool-discovery", "eko.tool_discovery_policy");
        diagnostics.record("capability", "subagent_definition.frontmatter");
        diagnostics.record("suggested-tasks", "eko.optional_follow_up_policy");
        diagnostics.record("language", "eko.language_policy");
        diagnostics.record("result-quality", "eko.result_quality_policy");
        diagnostics.record("contract", "echo_agent.render_result_contract");

        let access = if input.boundary.access == SubagentAccessMode::ReadOnly {
            "Read-only. Do not edit files, install dependencies, change repository state, or perform mutating side effects."
        } else {
            "Write-capable within the assignment and runtime-established workspace boundary."
        };
        let delegation = if input.boundary.can_delegate {
            "Tightly scoped child Subagent delegation is allowed. Summarize child results into this assignment and never delegate control of the global plan."
        } else {
            "Child Subagent delegation is disabled. Complete the assignment directly."
        };
        let mut capabilities = String::from("## Registered Capabilities\n\nAvailable tools:\n");
        if input.capabilities.visible_tools.is_empty() {
            capabilities.push_str("- none\n");
        } else {
            for tool in &input.capabilities.tools {
                if input
                    .capabilities
                    .visible_tools
                    .iter()
                    .any(|name| name == &tool.name)
                {
                    capabilities.push_str(&format!("- {}: {}\n", tool.name, tool.description));
                }
            }
        }
        if !input.capabilities.disabled_tools.is_empty() {
            capabilities.push_str("\nDisabled tools:\n");
            for name in &input.capabilities.disabled_tools {
                capabilities.push_str(&format!("- {name}\n"));
            }
        }
        capabilities.push_str(&format!(
            "\nExecution boundary:\n- access: {access}\n- write scope: {}\n- isolation: {}\n- delegation: {delegation}",
            if input.boundary.access == SubagentAccessMode::ReadOnly {
                "none"
            } else {
                "runtime-assigned declared targets"
            },
            input.boundary.isolation
        ));

        let mut sections = vec![input.role_prompt.trim().to_string()];
        if input.actor == PromptActor::Primary {
            diagnostics.record("primary-task-mode", "eko.primary_task_runtime_policy");
            sections.push(TOOL_DISCOVERY_POLICY.to_string());
            sections.push(capabilities);
            sections.push(SUBAGENT_LANGUAGE_POLICY.to_string());
            sections.push(PRIMARY_TASK_RUNTIME_POLICY.to_string());
            sections.push(COMMON_ORCHESTRATION_POLICY.to_string());
        } else {
            sections.push(COMMON_ORCHESTRATION_POLICY.to_string());
            sections.push(SUBAGENT_COMMUNICATION_PROTOCOL.to_string());
            sections.push(TOOL_DISCOVERY_POLICY.to_string());
            sections.push(capabilities);
            sections.push(SUGGESTED_TASKS_POLICY.to_string());
            sections.push(SUBAGENT_LANGUAGE_POLICY.to_string());
        }
        if input.actor == PromptActor::Primary {
            sections.push(SUGGESTED_TASKS_POLICY.to_string());
        }
        sections.push(SUBAGENT_RESULT_QUALITY_POLICY.to_string());
        sections.push(render_result_contract());

        CompiledSubagentSystemPrompt {
            system_prompt: sections.join("\n\n"),
            diagnostics,
        }
    }

    fn compile_invocation(&self, input: &SubagentInvocation<'_>) -> CompiledSubagentInvocation {
        let mut history = if input.transfer_policy == ContextTransferPolicy::InheritStructured {
            filter_history(input.history, input.history_limit)
        } else {
            Vec::new()
        };
        remove_duplicate_current_message(&mut history, input.current_message);

        if let Some(payload) = input.payload {
            match serde_json::from_value::<EkoPromptPayload>(payload.clone()) {
                Ok(EkoPromptPayload::PlannedTask { task }) => {
                    return compile_planned_invocation(task, input, history);
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
    input: &SubagentInvocation<'_>,
    history: Vec<echo_agent::llm::types::Message>,
) -> CompiledSubagentInvocation {
    let mut diagnostics = PromptDiagnostics::default();
    let mut sections = Vec::new();
    append_dynamic_environment(&mut sections, &mut diagnostics);
    if let Some(goal) = input
        .context
        .user_goal
        .as_deref()
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
    {
        diagnostics.record("user-goal", "parent_context.parent_goal");
        sections.push(format!("[user_request]\n{goal}\n[/user_request]"));
    }
    if let Some(root) = input.context.workspace.as_deref() {
        diagnostics.record("workspace", "invocation.context.workspace");
        sections.push(format!(
            "[workspace]\n- root: {}\n[/workspace]",
            root.display()
        ));
    }
    append_capability_override(&mut sections, input, &mut diagnostics);
    diagnostics.record("task", "dispatch_request.task");
    let title = input
        .context
        .task_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|title| format!("Title: {title}\n\n"))
        .unwrap_or_default();
    sections.push(format!(
        "[task_context]\nAssigned Subagent: {}\n\n{title}Task:\n{}\n[/task_context]",
        input.agent_name,
        input.task.trim()
    ));
    append_context_lists(&mut sections, input, &mut diagnostics);
    if !input.context.constraints.is_empty() {
        diagnostics.record("constraints", "dispatch_request.constraints");
        sections.push(format!(
            "[constraints]\n{}\n[/constraints]",
            input.context.constraints.join("\n")
        ));
    }
    let mut messages = history;
    messages.push(compiled_current_message(
        input.current_message,
        &sections.join("\n\n"),
    ));
    CompiledSubagentInvocation {
        messages,
        diagnostics,
    }
}

fn compile_planned_invocation(
    task: PlannedTaskPrompt,
    input: &SubagentInvocation<'_>,
    history: Vec<echo_agent::llm::types::Message>,
) -> CompiledSubagentInvocation {
    let mut diagnostics = PromptDiagnostics::default();
    let mut sections = Vec::new();
    if let Some(root) = input.context.workspace.as_deref() {
        diagnostics.record("workspace", "task_runtime.working_dir");
        sections.push(format!(
            "[workspace]\n- root: {}\n[/workspace]",
            root.display()
        ));
    }
    append_dynamic_environment(&mut sections, &mut diagnostics);
    append_capability_override(&mut sections, input, &mut diagnostics);

    let mut context = String::from("[task_context]\n");
    diagnostics.record("domain", "task.domain_profile");
    context.push_str(&format!(
        "Domain profile: {} ({})\nExecution standard: {}\n\n",
        task.domain_key, task.domain_label, task.execution_guidance
    ));
    if let Some(goal) = input.context.user_goal.as_deref() {
        diagnostics.record("user-goal", "task_run.goal");
        context.push_str(&format!("User goal:\n{goal}\n\n"));
    }
    diagnostics.record("task", "plan_task");
    context.push_str(&format!(
        "Task: {}\n\n{}\n\n",
        input
            .context
            .task_title
            .as_deref()
            .unwrap_or(input.agent_name),
        input.task
    ));
    append_dependencies(&mut context, &task.dependency_summaries, &mut diagnostics);
    append_list(
        &mut context,
        if matches!(task.task_boundary, TaskBoundaryPrompt::ReadOnly) {
            "Read targets:"
        } else {
            "Declared write targets:"
        },
        &input.context.files,
        "files",
        "plan_task.files",
        &mut diagnostics,
    );
    append_list(
        &mut context,
        "Execution checks:",
        &input.context.execution_checks,
        "execution-checks",
        "plan_task.execution_checks",
        &mut diagnostics,
    );
    append_list(
        &mut context,
        "Acceptance criteria:",
        &input.context.acceptance_criteria,
        "acceptance-criteria",
        "plan_task.acceptance_criteria",
        &mut diagnostics,
    );
    append_list(
        &mut context,
        "Required artifacts:",
        &input.context.required_artifacts,
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

    if !input.context.constraints.is_empty() {
        diagnostics.record("constraints", "invocation.context.constraints");
        sections.push(format!(
            "[constraints]\n{}\n[/constraints]",
            input.context.constraints.join("\n")
        ));
    }

    let mut messages = history;
    messages.push(compiled_current_message(
        input.current_message,
        &sections.join("\n\n"),
    ));
    CompiledSubagentInvocation {
        messages,
        diagnostics,
    }
}

fn append_capability_override(
    sections: &mut Vec<String>,
    input: &SubagentInvocation<'_>,
    diagnostics: &mut PromptDiagnostics,
) {
    let Some(capabilities) = input.capability_override else {
        return;
    };
    diagnostics.record(
        "invocation-capabilities",
        "runtime.effective_tool_registration",
    );
    let mut section = String::from("[capability_override]\nAllowed tools:\n");
    if capabilities.visible_tools.is_empty() {
        section.push_str("- none\n");
    } else {
        for name in &capabilities.visible_tools {
            section.push_str(&format!("- {name}\n"));
        }
    }
    if !capabilities.disabled_tools.is_empty() {
        section.push_str("Disabled for this invocation:\n");
        for name in &capabilities.disabled_tools {
            section.push_str(&format!("- {name}\n"));
        }
    }
    section.push_str("[/capability_override]");
    sections.push(section);
}

fn append_context_lists(
    sections: &mut Vec<String>,
    input: &SubagentInvocation<'_>,
    diagnostics: &mut PromptDiagnostics,
) {
    let mut context = String::new();
    append_list(
        &mut context,
        "Files:",
        &input.context.files,
        "files",
        "invocation.context.files",
        diagnostics,
    );
    append_list(
        &mut context,
        "Execution checks:",
        &input.context.execution_checks,
        "execution-checks",
        "invocation.context.execution_checks",
        diagnostics,
    );
    append_list(
        &mut context,
        "Acceptance criteria:",
        &input.context.acceptance_criteria,
        "acceptance-criteria",
        "invocation.context.acceptance_criteria",
        diagnostics,
    );
    append_list(
        &mut context,
        "Required artifacts:",
        &input.context.required_artifacts,
        "required-artifacts",
        "invocation.context.required_artifacts",
        diagnostics,
    );
    if !context.trim().is_empty() {
        sections.push(format!(
            "[execution_context]\n{}[/execution_context]",
            context.trim_end()
        ));
    }
}

fn append_dynamic_environment(sections: &mut Vec<String>, diagnostics: &mut PromptDiagnostics) {
    diagnostics.record("date", "runtime.local_date");
    sections.push(format!(
        "[environment]\n- Date: {}\n[/environment]",
        chrono::Local::now().format("%Y-%m-%d")
    ));
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
    use echo_agent::llm::types::Message;
    use echo_agent::subagent::{
        ExecutionMode, SubagentContext, SubagentExecutionBoundary, SubagentTaskContext,
        ToolCapability, ToolCapabilitySnapshot,
    };

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
                actor: echo_agent::subagent::PromptActor::Subagent,
                name: &definition.name,
                description: &definition.description,
                role_prompt: &definition.system_prompt,
                capabilities: &ToolCapabilitySnapshot::default(),
                boundary: SubagentExecutionBoundary {
                    access: if definition.readonly {
                        SubagentAccessMode::ReadOnly
                    } else {
                        SubagentAccessMode::Write
                    },
                    isolation,
                    can_delegate: definition.can_delegate,
                },
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
            assert!(compiled.system_prompt.ends_with(&render_result_contract()));
        }
    }

    #[test]
    fn can_delegate_declaration_reaches_system_prompt_wording() {
        let compiler = EkoSubagentPromptCompiler;
        let isolation = "context";
        let enabled = compiler.compile_system(&SubagentSystemPromptInput {
            actor: echo_agent::subagent::PromptActor::Subagent,
            name: "delegate-test",
            description: "d",
            role_prompt: "# Role\nbody",
            capabilities: &ToolCapabilitySnapshot::default(),
            boundary: SubagentExecutionBoundary {
                access: SubagentAccessMode::Write,
                isolation,
                can_delegate: true,
            },
        });
        assert!(
            enabled.system_prompt.contains("delegation is allowed"),
            "can_delegate=true must render the allowed wording, got: {}",
            enabled.system_prompt
        );

        let disabled = compiler.compile_system(&SubagentSystemPromptInput {
            actor: echo_agent::subagent::PromptActor::Subagent,
            name: "delegate-test",
            description: "d",
            role_prompt: "# Role\nbody",
            capabilities: &ToolCapabilitySnapshot::default(),
            boundary: SubagentExecutionBoundary {
                access: SubagentAccessMode::Write,
                isolation,
                can_delegate: false,
            },
        });
        assert!(disabled.system_prompt.contains("delegation is disabled"));
    }

    #[test]
    fn system_prompt_uses_typed_capability_snapshot() {
        let compiler = EkoSubagentPromptCompiler;
        let capabilities = ToolCapabilitySnapshot {
            tools: vec![
                ToolCapability {
                    name: "read_file".to_string(),
                    description: "Read a file".to_string(),
                },
                ToolCapability {
                    name: "shell".to_string(),
                    description: "Execute a command".to_string(),
                },
            ],
            visible_tools: vec!["read_file".to_string()],
            disabled_tools: vec!["shell".to_string()],
        };
        let compiled = compiler.compile_system(&SubagentSystemPromptInput {
            actor: echo_agent::subagent::PromptActor::Subagent,
            name: "reader",
            description: "Reads evidence",
            role_prompt: "# Role\nInspect evidence.",
            capabilities: &capabilities,
            boundary: SubagentExecutionBoundary {
                access: SubagentAccessMode::ReadOnly,
                isolation: "worktree",
                can_delegate: false,
            },
        });

        assert!(compiled.system_prompt.contains("- read_file: Read a file"));
        assert!(
            !compiled
                .system_prompt
                .contains("- shell: Execute a command")
        );
        assert!(compiled.system_prompt.contains("Disabled tools:\n- shell"));
        assert!(compiled.system_prompt.contains("write scope: none"));
        assert!(compiled.system_prompt.contains("isolation: worktree"));
    }

    #[test]
    fn dispatch_constraints_render_in_direct_invocation() {
        let compiler = EkoSubagentPromptCompiler;
        let parent = parent_context();
        let context = SubagentTaskContext {
            user_goal: parent.parent_goal.clone(),
            constraints: vec![
                "只读，不修改文件".to_string(),
                "不要碰 src/legacy".to_string(),
            ],
            ..SubagentTaskContext::default()
        };
        let compiled = compiler.compile_invocation(&SubagentInvocation {
            agent_name: "explorer",
            task: "检查约束",
            mode: ExecutionMode::Sync,
            transfer_policy: ContextTransferPolicy::Fresh,
            history: &parent.messages,
            history_limit: None,
            current_message: None,
            context: &context,
            capability_override: None,
            payload: None,
        });
        assert_eq!(compiled.diagnostics.count("constraints"), 1);
        assert_eq!(compiled.diagnostics.count("date"), 1);
        assert!(compiled.task_input().contains("[constraints]"));
        assert_eq!(compiled.task_input().matches("- Date:").count(), 1);
        assert!(compiled.task_input().contains("只读，不修改文件"));
        assert!(compiled.task_input().contains("src/legacy"));

        let empty_context = SubagentTaskContext::default();
        let empty = compiler.compile_invocation(&SubagentInvocation {
            agent_name: "explorer",
            task: "检查约束",
            mode: ExecutionMode::Sync,
            transfer_policy: ContextTransferPolicy::Fresh,
            history: &[],
            history_limit: None,
            current_message: None,
            context: &empty_context,
            capability_override: None,
            payload: None,
        });
        assert_eq!(empty.diagnostics.count("constraints"), 0);
        assert!(!empty.task_input().contains("[constraints]"));
    }

    #[test]
    fn direct_planned_and_fork_invocations_have_structured_cardinality() -> Result<(), String> {
        let compiler = EkoSubagentPromptCompiler;
        let parent = parent_context();
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
        )
        .to_value()?;
        let planned_context = SubagentTaskContext {
            task_title: Some(task.title.clone()),
            user_goal: parent.parent_goal.clone(),
            workspace: Some(std::path::PathBuf::from("/workspace")),
            files: task.files.clone(),
            execution_checks: task.execution_checks.clone(),
            acceptance_criteria: task.acceptance_criteria.clone(),
            required_artifacts: task.required_artifacts.clone(),
            constraints: Vec::new(),
        };

        for definition in crate::subagent_loader::discover_subagents(None, None) {
            let direct_context = SubagentTaskContext {
                user_goal: parent.parent_goal.clone(),
                ..SubagentTaskContext::default()
            };
            let direct = compiler.compile_invocation(&SubagentInvocation {
                agent_name: &definition.name,
                task: "检查直接派发",
                mode: ExecutionMode::Sync,
                transfer_policy: ContextTransferPolicy::Fresh,
                history: &parent.messages,
                history_limit: None,
                current_message: None,
                context: &direct_context,
                capability_override: None,
                payload: None,
            });
            assert_eq!(direct.diagnostics.count("user-goal"), 1);
            assert_eq!(direct.diagnostics.count("task"), 1);
            assert_eq!(direct.diagnostics.count("date"), 1);
            assert!(direct.history().is_empty());

            let planned = compiler.compile_invocation(&SubagentInvocation {
                agent_name: &definition.name,
                task: &task.description,
                mode: ExecutionMode::Fork,
                transfer_policy: ContextTransferPolicy::Fresh,
                history: &parent.messages,
                history_limit: None,
                current_message: None,
                context: &planned_context,
                capability_override: None,
                payload: Some(&payload),
            });
            assert_eq!(planned.diagnostics.count("user-goal"), 1);
            assert_eq!(planned.diagnostics.count("task"), 1);
            assert_eq!(planned.diagnostics.count("dependencies"), 1);
            assert_eq!(planned.diagnostics.count("workspace"), 1);
            assert_eq!(planned.diagnostics.count("date"), 1);
            assert_eq!(planned.task_input().matches("[workspace]").count(), 1);
            assert!(planned.history().is_empty());

            let fork = compiler.compile_invocation(&SubagentInvocation {
                agent_name: &definition.name,
                task: "检查 fork",
                mode: ExecutionMode::Fork,
                transfer_policy: ContextTransferPolicy::InheritStructured,
                history: &parent.messages,
                history_limit: Some(2),
                current_message: None,
                context: &direct_context,
                capability_override: None,
                payload: None,
            });
            assert_eq!(fork.history().len(), 2);
            assert!(
                fork.history()
                    .iter()
                    .all(|message| message.tool_call_id.is_none())
            );

            let teammate = compiler.compile_invocation(&SubagentInvocation {
                agent_name: &definition.name,
                task: "检查 teammate",
                mode: ExecutionMode::Teammate,
                transfer_policy: ContextTransferPolicy::InheritStructured,
                history: &parent.messages,
                history_limit: Some(2),
                current_message: None,
                context: &direct_context,
                capability_override: None,
                payload: None,
            });
            assert_eq!(teammate.history().len(), 2);
        }
        Ok(())
    }

    #[test]
    fn primary_system_owns_stable_sections_and_invocation_stays_dynamic() -> Result<(), String> {
        let compiler = EkoSubagentPromptCompiler;
        let capabilities = ToolCapabilitySnapshot::default();
        let system = compiler.compile_system(&SubagentSystemPromptInput {
            actor: PromptActor::Primary,
            name: "primary",
            description: "primary task executor",
            role_prompt: "# Primary Role\nHelp the user.",
            capabilities: &capabilities,
            boundary: SubagentExecutionBoundary {
                access: SubagentAccessMode::Write,
                isolation: "runtime-selected",
                can_delegate: true,
            },
        });
        let task = PlanTask {
            title: "运行验证".to_string(),
            description: "执行测试".to_string(),
            ..PlanTask::default()
        };
        let payload = EkoPromptPayload::planned_task(&task, &[], false).to_value()?;
        let context = SubagentTaskContext {
            task_title: Some(task.title.clone()),
            user_goal: Some("完成验证".to_string()),
            ..SubagentTaskContext::default()
        };
        let compiled = compiler.compile_primary_invocation(&SubagentInvocation {
            agent_name: "primary",
            task: &task.description,
            mode: ExecutionMode::Sync,
            transfer_policy: ContextTransferPolicy::Fresh,
            history: &[],
            history_limit: None,
            current_message: None,
            context: &context,
            capability_override: None,
            payload: Some(&payload),
        });

        assert_eq!(compiled.diagnostics.count("user-goal"), 1);
        assert_eq!(
            system.system_prompt.matches("## Response Language").count(),
            1
        );
        assert_eq!(system.system_prompt.matches("## Result Quality").count(), 1);
        assert_eq!(
            system
                .system_prompt
                .lines()
                .filter(|line| *line == "## Result")
                .count(),
            1
        );
        assert!(!compiled.task_input().contains("## Response Language"));
        assert!(!compiled.task_input().contains("## Result Quality"));
        assert!(!compiled.task_input().contains("## Result"));
        Ok(())
    }
}
