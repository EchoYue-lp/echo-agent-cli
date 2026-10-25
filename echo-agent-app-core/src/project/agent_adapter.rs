//! Thin preparation adapter at the existing fallible invocation boundary.
//!
//! Fork isolation has already supplied the actual cwd here. The original
//! ReactAgent and invocation still own execution, cancellation and stream EOF.

use std::path::PathBuf;
use std::sync::Arc;

use echo_agent::agent::{
    Agent, AgentEvent, AgentInvocationContext, AgentSteerError, AgentSteerReceipt,
    CancellationToken,
};
use echo_agent::error::{ReactError, Result};
use echo_agent::llm::types::Message;
use echo_agent::sandbox::SandboxExecutor;
use echo_agent::tools::filesystem::FileSystem;
use futures::{future::BoxFuture, stream::BoxStream};

struct ProjectAgentAdapter {
    handle: crate::agent_handle::AgentHandle,
    delegate: Box<dyn Agent>,
    files: Arc<dyn FileSystem>,
    executor: Arc<dyn SandboxExecutor>,
    local_state_dir: Option<PathBuf>,
    default_cwd: Option<PathBuf>,
}

/// Adapt the fresh default/Plugin instances admitted by their existing
/// factories. This is not a preparation lock for concurrently reused static
/// Agents, and must not replace a primary Agent's product projector.
pub(crate) async fn boxed_project_agent(
    handle: crate::agent_handle::AgentHandle,
    files: Arc<dyn FileSystem>,
    executor: Arc<dyn SandboxExecutor>,
    local_state_dir: Option<PathBuf>,
    default_cwd: Option<PathBuf>,
) -> Box<dyn Agent> {
    let delegate = handle.to_boxed_agent().await;
    Box::new(ProjectAgentAdapter {
        handle,
        delegate,
        files,
        executor,
        local_state_dir,
        default_cwd,
    })
}

impl ProjectAgentAdapter {
    async fn prepare(&self, invocation: &AgentInvocationContext) -> Result<()> {
        let cwd = invocation
            .working_dir
            .clone()
            .or_else(|| self.default_cwd.clone());
        let state = self.local_state_dir.clone();
        let files = self.files.clone();
        let executor = self.executor.clone();
        // Only this invocation's physical preparation borrows these guards.
        // They are never retained in the shared adapter or a later invocation.
        let guards = invocation.resource_guards.clone();
        self.handle
            .write_async(|agent| {
                Box::pin(async move {
                    let prepared = crate::infra::prepare_dynamic_project_context(
                        cwd.as_deref(),
                        state.as_deref(),
                        files,
                        executor,
                        guards,
                        agent.config().get_token_limit(),
                    )
                    .await
                    .map_err(ReactError::Other)?;
                    prepared.publish_for_fresh_subagent(agent).await;
                    Ok(())
                })
            })
            .await
    }
}

impl Agent for ProjectAgentAdapter {
    fn name(&self) -> &str {
        self.delegate.name()
    }
    fn model_name(&self) -> &str {
        self.delegate.model_name()
    }
    fn system_prompt(&self) -> &str {
        self.delegate.system_prompt()
    }
    fn tool_names(&self) -> Vec<String> {
        self.delegate.tool_names()
    }
    fn tool_definitions(&self) -> Vec<echo_agent::llm::ToolDefinition> {
        self.delegate.tool_definitions()
    }
    fn disabled_tool_names(&self) -> std::collections::HashSet<String> {
        self.delegate.disabled_tool_names()
    }
    fn tool_visibility_policy(&self) -> echo_agent::agent::ToolVisibilityPolicy {
        self.delegate.tool_visibility_policy()
    }
    fn working_dir(&self) -> Option<PathBuf> {
        self.delegate.working_dir()
    }
    fn skill_names(&self) -> Vec<String> {
        self.delegate.skill_names()
    }
    fn mcp_server_names(&self) -> Vec<String> {
        self.delegate.mcp_server_names()
    }
    fn close(&self) -> BoxFuture<'_, Result<()>> {
        self.delegate.close()
    }
    fn execute<'a>(&'a self, task: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            self.prepare(&AgentInvocationContext::default()).await?;
            self.delegate.execute(task).await
        })
    }
    fn execute_stream<'a>(
        &'a self,
        task: &'a str,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
        self.execute_stream_with_invocation_context(
            task,
            CancellationToken::new(),
            AgentInvocationContext::default(),
        )
    }
    fn execute_stream_with_cancel<'a>(
        &'a self,
        task: &'a str,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
        self.execute_stream_with_invocation_context(task, cancel, AgentInvocationContext::default())
    }
    fn execute_stream_with_invocation_context<'a>(
        &'a self,
        task: &'a str,
        cancel: CancellationToken,
        invocation: AgentInvocationContext,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
        Box::pin(async move {
            self.prepare(&invocation).await?;
            self.delegate
                .execute_stream_with_invocation_context(task, cancel, invocation)
                .await
        })
    }
    fn execute_stream_message_with_cancel<'a>(
        &'a self,
        message: Message,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
        self.execute_stream_message_with_invocation_context(
            message,
            cancel,
            AgentInvocationContext::default(),
        )
    }
    fn execute_stream_message_with_invocation_context<'a>(
        &'a self,
        message: Message,
        cancel: CancellationToken,
        invocation: AgentInvocationContext,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
        Box::pin(async move {
            self.prepare(&invocation).await?;
            self.delegate
                .execute_stream_message_with_invocation_context(message, cancel, invocation)
                .await
        })
    }
    fn chat<'a>(&'a self, message: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            self.prepare(&AgentInvocationContext::default()).await?;
            self.delegate.chat(message).await
        })
    }
    fn chat_stream<'a>(
        &'a self,
        message: &'a str,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
        Box::pin(async move {
            self.prepare(&AgentInvocationContext::default()).await?;
            self.delegate.chat_stream(message).await
        })
    }
    fn chat_stream_with_cancel<'a>(
        &'a self,
        message: &'a str,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
        Box::pin(async move {
            self.prepare(&AgentInvocationContext::default()).await?;
            self.delegate.chat_stream_with_cancel(message, cancel).await
        })
    }
    fn chat_stream_message_with_cancel<'a>(
        &'a self,
        message: Message,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
        self.chat_stream_message_with_invocation_context(
            message,
            cancel,
            AgentInvocationContext::default(),
        )
    }
    fn chat_stream_message_with_invocation_context<'a>(
        &'a self,
        message: Message,
        cancel: CancellationToken,
        invocation: AgentInvocationContext,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
        Box::pin(async move {
            self.prepare(&invocation).await?;
            self.delegate
                .chat_stream_message_with_invocation_context(message, cancel, invocation)
                .await
        })
    }
    fn reset(&self) -> BoxFuture<'_, ()> {
        self.delegate.reset()
    }
    fn current_run_id(&self) -> Option<String> {
        self.delegate.current_run_id()
    }
    fn token_usage_summary(&self) -> echo_agent::tokenizer::UsageSummary {
        self.delegate.token_usage_summary()
    }
    fn steer_input(
        &self,
        turn: Option<&str>,
        message: Message,
    ) -> std::result::Result<String, AgentSteerError> {
        self.delegate.steer_input(turn, message)
    }
    fn steer_input_tracked(
        &self,
        turn: Option<&str>,
        message: Message,
    ) -> std::result::Result<AgentSteerReceipt, AgentSteerError> {
        self.delegate.steer_input_tracked(turn, message)
    }
    fn set_external_context(&self, context: &echo_agent::tools::ExternalRunContext) {
        self.delegate.set_external_context(context);
    }
    fn clear_external_context(&self) {
        self.delegate.clear_external_context();
    }
    fn set_working_dir(&self, path: Option<PathBuf>) {
        self.delegate.set_working_dir(path);
    }
    fn clear_working_dir(&self) {
        self.delegate.clear_working_dir();
    }
    fn register_tool(&self, tool: Box<dyn echo_agent::tools::Tool>) {
        self.delegate.register_tool(tool);
    }
    fn remove_tool(&self, name: &str) -> bool {
        self.delegate.remove_tool(name)
    }
    fn messages(&self) -> Vec<Message> {
        self.delegate.messages()
    }
    fn set_system_prompt(&self, prompt: &str) {
        self.delegate.set_system_prompt(prompt);
    }
    fn delegate_to<'a>(&'a self, target: &'a str, task: &'a str) -> BoxFuture<'a, Result<String>> {
        self.delegate.delegate_to(target, task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingAgent(Arc<AtomicUsize>);
    impl Agent for CountingAgent {
        fn name(&self) -> &str {
            "counter"
        }
        fn model_name(&self) -> &str {
            "unused"
        }
        fn system_prompt(&self) -> &str {
            "role"
        }
        fn execute<'a>(&'a self, _task: &'a str) -> BoxFuture<'a, Result<String>> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(String::new())
            })
        }
        fn execute_stream<'a>(
            &'a self,
            _task: &'a str,
        ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(futures::stream::empty().boxed())
            })
        }
    }

    fn fixture(
        host: &std::path::Path,
        state: PathBuf,
    ) -> anyhow::Result<(ProjectAgentAdapter, Arc<AtomicUsize>)> {
        let handle = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("unused")
                .system_prompt("role")
                .working_dir(host.to_path_buf())
                .auto_project_rules(false)
                .build()?,
        );
        let calls = Arc::new(AtomicUsize::new(0));
        Ok((
            ProjectAgentAdapter {
                handle,
                delegate: Box::new(CountingAgent(calls.clone())),
                files: Arc::new(echo_agent::tools::files::LocalFileSystem),
                executor: Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox()),
                local_state_dir: Some(state),
                default_cwd: Some(host.to_path_buf()),
            },
            calls,
        ))
    }

    #[tokio::test]
    async fn prepares_actual_checkout_rules_before_delegating() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let host = temp.path().join("host");
        let checkout = temp.path().join("checkout");
        for root in [&host, &checkout] {
            std::fs::create_dir(root)?;
            std::fs::write(root.join("Cargo.toml"), "marker")?;
        }
        std::fs::write(host.join("AGENTS.md"), "LOCAL_HOST_SENTINEL")?;
        std::fs::write(checkout.join("AGENTS.md"), "TARGET_CHECKOUT_SENTINEL")?;
        let (adapter, calls) = fixture(&host, temp.path().join("local-state"))?;
        let invocation = AgentInvocationContext {
            working_dir: Some(checkout),
            ..Default::default()
        };
        let events = adapter
            .execute_stream_with_invocation_context("read", CancellationToken::new(), invocation)
            .await?
            .collect::<Vec<_>>()
            .await;
        assert!(events.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let context = adapter.handle.read(|agent| agent.context().clone()).await;
        let text = context
            .lock()
            .await
            .messages()
            .iter()
            .filter_map(|message| message.content.as_text_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("TARGET_CHECKOUT_SENTINEL"));
        assert!(!text.contains("LOCAL_HOST_SENTINEL"));
        Ok(())
    }

    #[tokio::test]
    async fn actual_checkout_rule_io_failure_stops_before_delegate() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let checkout = temp.path().join("checkout");
        std::fs::create_dir(&checkout)?;
        std::fs::write(checkout.join("Cargo.toml"), "marker")?;
        std::fs::write(checkout.join("AGENTS.md"), [0xff])?;
        let (adapter, calls) = fixture(temp.path(), temp.path().join("state"))?;
        let result = adapter
            .execute_stream_message_with_invocation_context(
                Message::user("read".to_string()),
                CancellationToken::new(),
                AgentInvocationContext {
                    working_dir: Some(checkout),
                    ..Default::default()
                },
            )
            .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(
            result
                .err()
                .map(|error| error
                    .to_string()
                    .contains("Project context preparation failed"))
                .unwrap_or(false)
        );
        Ok(())
    }

    #[tokio::test]
    async fn real_cold_execute_and_checkpoint_restore_keep_actual_checkout_rules()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let host = temp.path().join("host");
        let first = temp.path().join("first-checkout");
        let second = temp.path().join("second-checkout");
        for root in [&host, &first, &second] {
            std::fs::create_dir(root)?;
            std::fs::write(root.join("Cargo.toml"), "marker")?;
        }
        std::fs::write(host.join("AGENTS.md"), "WRONG_HOST_RULE")?;
        std::fs::write(first.join("AGENTS.md"), "FIRST_CHECKOUT_RULE")?;
        std::fs::write(second.join("AGENTS.md"), "SECOND_CHECKOUT_RULE")?;
        let client = Arc::new(
            echo_agent::testing::MockLlmClient::new().with_responses(["first done", "second done"]),
        );
        let states: Arc<dyn echo_agent::state::RuntimeStateStore> = Arc::new(
            echo_agent::state::FileRuntimeStateStore::new(temp.path().join("checkpoints"))?,
        );
        let handle = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("mock-model")
                .system_prompt("ROLE_BASELINE")
                .auto_project_rules(false)
                .working_dir(host)
                .llm_client(client.clone())
                .state_store(states.clone())
                .conversation_id("project-rule-checkpoint")
                .react_checkpoint_interval(1)
                .max_iterations(1)
                .build()?,
        );
        let adapter = boxed_project_agent(
            handle,
            Arc::new(echo_agent::tools::files::LocalFileSystem),
            Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox()),
            Some(temp.path().join("local-state")),
            None,
        )
        .await;
        let binding = crate::domain_plugin::test_binding(false).map_err(anyhow::Error::msg)?;
        let run_id = uuid::Uuid::new_v4().to_string();
        let store = Arc::new(crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()?);
        store.create_run_for_active_workspace_with_domain(
            &run_id,
            "project-rule-checkpoint",
            "domain-root",
            crate::tasks::task_runtime::DomainProfile::General,
            "inspect the bound project",
            "task",
            crate::tasks::task_runtime::AttendedMode::Attended,
            crate::tasks::task_runtime::TaskRunExecutionProfile::orchestrated(
                crate::tasks::task_runtime::RunPlanPolicy::AllowDirect,
            ),
            Some(&binding),
        )?;
        let projection =
            crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
                .retain_for_invocation(&run_id, store)
                .map_err(anyhow::Error::msg)?;
        for (cwd, runtime) in [
            (
                first,
                Some(echo_agent::tools::ExternalRunContext {
                    run_id: Some(run_id),
                    ..Default::default()
                }),
            ),
            (second, None),
        ] {
            let events = adapter
                .execute_stream_with_invocation_context(
                    "finish",
                    CancellationToken::new(),
                    AgentInvocationContext {
                        working_dir: Some(cwd),
                        runtime,
                        ..Default::default()
                    },
                )
                .await?
                .collect::<Vec<_>>()
                .await;
            for event in events {
                event?;
            }
            assert!(
                states
                    .get_checkpoint("project-rule-checkpoint")
                    .await?
                    .is_some()
            );
        }
        let calls = client.all_calls();
        assert_eq!(calls.len(), 2);
        let first = calls
            .first()
            .ok_or_else(|| anyhow::anyhow!("first model request absent"))?
            .iter()
            .filter_map(|message| message.content.as_text_ref())
            .collect::<Vec<_>>()
            .join("\n");
        let second = calls
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("resumed model request absent"))?
            .iter()
            .filter_map(|message| message.content.as_text_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(first.matches("FIRST_CHECKOUT_RULE").count(), 1);
        assert!(first.contains(&binding.snapshot.method));
        assert!(!first.contains("WRONG_HOST_RULE"));
        assert_eq!(second.matches("SECOND_CHECKOUT_RULE").count(), 1);
        assert!(!second.contains("FIRST_CHECKOUT_RULE"));
        assert!(!second.contains("WRONG_HOST_RULE"));
        assert!(
            !second.contains(crate::tasks::task_runtime::compact_context::DOMAIN_METHOD_MARKER)
        );
        assert!(!second.contains(&binding.snapshot.method));
        assert_eq!(second.matches("ROLE_BASELINE").count(), 1);
        drop(projection);
        Ok(())
    }
}
