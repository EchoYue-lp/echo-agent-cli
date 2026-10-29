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
    // 准备投影与实际执行共用同一个 cwd：显式隔离目录优先，其次为原
    // Agent 配置，再用产品工厂的项目根兜底；不修改共享 Agent 的配置。
    fn resolve_working_dir(&self, explicit: Option<PathBuf>) -> Option<PathBuf> {
        explicit
            // 直接调用原 dyn Agent；Box 自身的 trait 默认 getter 会丢失配置目录。
            .or_else(|| self.delegate.as_ref().working_dir())
            .or_else(|| self.default_cwd.clone())
    }

    fn qualify_invocation(&self, mut invocation: AgentInvocationContext) -> AgentInvocationContext {
        invocation.working_dir = self.resolve_working_dir(invocation.working_dir);
        invocation
    }

    async fn prepare(&self, invocation: &AgentInvocationContext) -> Result<()> {
        let cwd = self.resolve_working_dir(invocation.working_dir.clone());
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
        self.resolve_working_dir(None)
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
            let invocation = self.qualify_invocation(invocation);
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
            let invocation = self.qualify_invocation(invocation);
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
            let invocation = self.qualify_invocation(invocation);
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

    #[derive(Debug, PartialEq)]
    struct ObservedReadonlyInvocation {
        entry: &'static str,
        working_dir: Option<PathBuf>,
        tool_access: Option<echo_agent::tools::ToolAccess>,
        allowed_tools: Option<std::collections::HashSet<String>>,
        run_id: Option<String>,
        policy: Option<echo_agent::tasks::NestedDelegationPolicy>,
        history: Vec<String>,
    }

    struct InvocationObservingAgent {
        working_dir: Option<PathBuf>,
        observed: Arc<std::sync::Mutex<Vec<ObservedReadonlyInvocation>>>,
    }

    impl InvocationObservingAgent {
        fn record(&self, entry: &'static str, invocation: AgentInvocationContext) -> Result<()> {
            let history = invocation
                .history
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter_map(|message| message.content.as_text_ref().map(str::to_string))
                .collect();
            self.observed
                .lock()
                .map_err(|error| ReactError::Other(error.to_string()))?
                .push(ObservedReadonlyInvocation {
                    entry,
                    working_dir: invocation.working_dir,
                    tool_access: invocation.tool_access,
                    allowed_tools: invocation.allowed_tools,
                    run_id: invocation
                        .runtime
                        .as_ref()
                        .and_then(|value| value.run_id.clone()),
                    policy: invocation.runtime.and_then(|value| value.delegation_policy),
                    history,
                });
            Ok(())
        }
    }

    impl Agent for InvocationObservingAgent {
        fn working_dir(&self) -> Option<PathBuf> {
            self.working_dir.clone()
        }
        fn name(&self) -> &str {
            "invocation-observer"
        }
        fn model_name(&self) -> &str {
            "unused"
        }
        fn system_prompt(&self) -> &str {
            "field-level forwarding probe"
        }
        fn execute<'a>(&'a self, _task: &'a str) -> BoxFuture<'a, Result<String>> {
            Box::pin(async { Err(ReactError::Other("unexpected legacy entry".to_string())) })
        }
        fn execute_stream<'a>(
            &'a self,
            _task: &'a str,
        ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
            Box::pin(async {
                Err(ReactError::Other(
                    "unexpected legacy stream entry".to_string(),
                ))
            })
        }
        fn execute_stream_with_invocation_context<'a>(
            &'a self,
            _task: &'a str,
            _cancel: CancellationToken,
            invocation: AgentInvocationContext,
        ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
            let observed = self.record("text", invocation);
            Box::pin(async move {
                observed?;
                Ok(futures::stream::empty().boxed())
            })
        }
        fn execute_stream_message_with_invocation_context<'a>(
            &'a self,
            _message: Message,
            _cancel: CancellationToken,
            invocation: AgentInvocationContext,
        ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
            let observed = self.record("execute-message", invocation);
            Box::pin(async move {
                observed?;
                Ok(futures::stream::empty().boxed())
            })
        }
        fn chat_stream_message_with_invocation_context<'a>(
            &'a self,
            _message: Message,
            _cancel: CancellationToken,
            invocation: AgentInvocationContext,
        ) -> BoxFuture<'a, Result<BoxStream<'a, Result<AgentEvent>>>> {
            let observed = self.record("chat-message", invocation);
            Box::pin(async move {
                observed?;
                Ok(futures::stream::empty().boxed())
            })
        }
    }

    #[tokio::test]
    async fn project_adapter_forwards_readonly_scope_at_all_invocation_entries()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let initialized = std::process::Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "--quiet"])
            .arg(temp.path())
            .output()?;
        anyhow::ensure!(
            initialized.status.success(),
            "test project initialization failed"
        );
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname=\"adapter-fixture\"\nversion=\"0.0.0\"\n",
        )?;
        let (mut adapter, _) = fixture(temp.path(), temp.path().join("state"))?;
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        adapter.delegate = Box::new(InvocationObservingAgent {
            working_dir: None,
            observed: observed.clone(),
        });
        let policy = echo_agent::tasks::NestedDelegationPolicy {
            can_spawn_subagents: true,
            delegate_depth: 1,
            max_delegate_depth: 3,
        };
        let invocation = AgentInvocationContext {
            tool_access: Some(echo_agent::tools::ToolAccess::ReadOnly),
            allowed_tools: Some(["read_file".to_string()].into_iter().collect()),
            history: Some(vec![Message::user("retained history".to_string())]),
            working_dir: Some(temp.path().to_path_buf()),
            runtime: Some(echo_agent::tools::ExternalRunContext {
                run_id: Some("adapter-readonly-run".to_string()),
                delegation_policy: Some(policy),
                ..Default::default()
            }),
            ..Default::default()
        };
        let first = adapter
            .execute_stream_with_invocation_context(
                "probe",
                CancellationToken::new(),
                invocation.clone(),
            )
            .await?
            .collect::<Vec<_>>()
            .await;
        let second = adapter
            .execute_stream_message_with_invocation_context(
                Message::user("probe".to_string()),
                CancellationToken::new(),
                invocation.clone(),
            )
            .await?
            .collect::<Vec<_>>()
            .await;
        let third = adapter
            .chat_stream_message_with_invocation_context(
                Message::user("probe".to_string()),
                CancellationToken::new(),
                invocation,
            )
            .await?
            .collect::<Vec<_>>()
            .await;
        assert!(first.is_empty() && second.is_empty() && third.is_empty());
        let actual = observed
            .lock()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let expected = ["text", "execute-message", "chat-message"]
            .into_iter()
            .map(|entry| ObservedReadonlyInvocation {
                entry,
                working_dir: Some(temp.path().to_path_buf()),
                tool_access: Some(echo_agent::tools::ToolAccess::ReadOnly),
                allowed_tools: Some(["read_file".to_string()].into_iter().collect()),
                run_id: Some("adapter-readonly-run".to_string()),
                policy: Some(policy),
                history: vec!["retained history".to_string()],
            })
            .collect::<Vec<_>>();
        assert_eq!(*actual, expected);
        Ok(())
    }

    #[tokio::test]
    async fn project_adapter_resolves_actual_cwd_at_all_typed_entries() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let fallback = temp.path().join("fallback");
        let configured = temp.path().join("configured");
        let explicit = temp.path().join("explicit");
        for path in [&fallback, &configured, &explicit] {
            std::fs::create_dir(path)?;
            std::fs::write(path.join("Cargo.toml"), "marker")?;
        }
        for (requested, delegated, expected) in [
            (None, None, fallback.clone()),
            (None, Some(configured.clone()), configured.clone()),
            (
                Some(explicit.clone()),
                Some(configured.clone()),
                explicit.clone(),
            ),
        ] {
            let (mut adapter, _) = fixture(&fallback, temp.path().join("state"))?;
            let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
            adapter.delegate = Box::new(InvocationObservingAgent {
                working_dir: delegated.clone(),
                observed: observed.clone(),
            });
            assert_eq!(
                adapter.working_dir(),
                delegated.or_else(|| Some(fallback.clone()))
            );
            let invocation = AgentInvocationContext {
                working_dir: requested,
                ..Default::default()
            };
            let first = adapter
                .execute_stream_with_invocation_context(
                    "probe",
                    CancellationToken::new(),
                    invocation.clone(),
                )
                .await?
                .collect::<Vec<_>>()
                .await;
            let second = adapter
                .execute_stream_message_with_invocation_context(
                    Message::user("probe".to_string()),
                    CancellationToken::new(),
                    invocation.clone(),
                )
                .await?
                .collect::<Vec<_>>()
                .await;
            let third = adapter
                .chat_stream_message_with_invocation_context(
                    Message::user("probe".to_string()),
                    CancellationToken::new(),
                    invocation,
                )
                .await?
                .collect::<Vec<_>>()
                .await;
            assert!(first.is_empty() && second.is_empty() && third.is_empty());
            let actual = observed
                .lock()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            assert_eq!(actual.len(), 3);
            for (observation, entry) in
                actual
                    .iter()
                    .zip(["text", "execute-message", "chat-message"])
            {
                assert_eq!(observation.entry, entry);
                assert_eq!(observation.working_dir, Some(expected.clone()));
            }
        }
        Ok(())
    }
}
