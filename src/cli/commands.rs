//! Command handler module.
//!
//! The [`CommandHandler`] dispatches slash commands via the trait-based
//! [`CommandRegistry`](crate::cli::command::CommandRegistry). Individual
//! commands live in [`crate::cli::cmd_impls`].

use std::sync::Arc;

use crate::agent_handle::AgentHandle;
use crate::cli::command::CommandContext;

/// Result of command processing.
pub enum CommandResult {
    /// Continue the REPL loop.
    Continue,
    /// Exit the REPL.
    Exit,
    /// Execute a chat message.
    Chat(String),
}

/// Command handler — owns agent, mode, and optional coding loop + registry.
pub struct CommandHandler {
    agent: AgentHandle,
    current_mode: String,
    coding_loop: Option<Arc<tokio::sync::Mutex<crate::project::coding_loop::CodingLoop>>>,
    registry: Option<Arc<crate::cli::command::CommandRegistry>>,
    task_service: Option<Arc<echo_agent_app_core::tasks::BackgroundTaskService>>,
    scheduler: Option<Arc<echo_agent_app_core::scheduler::SchedulerRunner>>,
    plugin_runtime: Option<Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>>,
    prompt_assembly: Option<echo_agent_app_core::project::prompt::PromptAssembly>,
    review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    interaction_mode:
        Arc<tokio::sync::RwLock<echo_agent_app_core::tasks::task_runtime::InteractionMode>>,
    staged_attachments:
        Arc<tokio::sync::Mutex<Vec<echo_agent_app_core::attachments::AttachmentRef>>>,
    app_state: Option<Arc<echo_agent_app_core::state::AppState>>,
}

impl CommandHandler {
    pub fn new(agent: AgentHandle) -> Self {
        Self {
            agent,
            current_mode: "general".to_string(),
            coding_loop: None,
            registry: None,
            task_service: None,
            scheduler: None,
            plugin_runtime: None,
            prompt_assembly: None,
            review_integration: None,
            interaction_mode: Arc::new(tokio::sync::RwLock::new(
                echo_agent_app_core::tasks::task_runtime::InteractionMode::Auto,
            )),
            staged_attachments: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            app_state: None,
        }
    }

    pub fn with_mode(mut self, mode: &str) -> Self {
        self.current_mode = mode.to_string();
        self
    }

    /// Attach a CodingLoop for coding mode commands.
    pub fn with_coding_loop(
        mut self,
        cl: Arc<tokio::sync::Mutex<crate::project::coding_loop::CodingLoop>>,
    ) -> Self {
        self.coding_loop = Some(cl);
        self
    }

    /// Attach a CommandRegistry for trait-based command dispatch.
    pub fn with_registry(mut self, registry: Arc<crate::cli::command::CommandRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Attach a BackgroundTaskService for submitting long-running tasks.
    pub fn with_task_service(
        mut self,
        service: Arc<echo_agent_app_core::tasks::BackgroundTaskService>,
    ) -> Self {
        self.task_service = Some(service);
        self
    }

    /// Attach an optional BackgroundTaskService.
    pub fn with_task_service_opt(
        mut self,
        service: Option<Arc<echo_agent_app_core::tasks::BackgroundTaskService>>,
    ) -> Self {
        self.task_service = service;
        self
    }

    /// Attach an optional SchedulerRunner for cron task management.
    pub fn with_scheduler_opt(
        mut self,
        scheduler: Option<Arc<echo_agent_app_core::scheduler::SchedulerRunner>>,
    ) -> Self {
        self.scheduler = scheduler;
        self
    }

    pub fn with_plugin_runtime(
        mut self,
        runtime: Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>,
    ) -> Self {
        self.plugin_runtime = Some(runtime);
        self
    }

    pub fn with_plugin_runtime_opt(
        mut self,
        runtime: Option<Arc<echo_agent_app_core::plugin_runtime::PluginRuntimeService>>,
    ) -> Self {
        self.plugin_runtime = runtime;
        self
    }

    pub fn with_prompt_assembly(
        mut self,
        prompt_assembly: Option<echo_agent_app_core::project::prompt::PromptAssembly>,
    ) -> Self {
        self.prompt_assembly = prompt_assembly;
        self
    }

    pub fn with_review_integration(
        mut self,
        review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    ) -> Self {
        self.review_integration = review_integration;
        self
    }

    pub fn with_app_state_opt(
        mut self,
        app_state: Option<Arc<echo_agent_app_core::state::AppState>>,
    ) -> Self {
        self.app_state = app_state;
        self
    }

    pub fn with_interaction_mode(
        mut self,
        mode: Arc<tokio::sync::RwLock<echo_agent_app_core::tasks::task_runtime::InteractionMode>>,
    ) -> Self {
        self.interaction_mode = mode;
        self
    }

    pub fn with_staged_attachments(
        mut self,
        attachments: Arc<tokio::sync::Mutex<Vec<echo_agent_app_core::attachments::AttachmentRef>>>,
    ) -> Self {
        self.staged_attachments = attachments;
        self
    }

    /// Process user input.
    pub async fn handle(&self, input: &str) -> CommandResult {
        let input = input.trim();

        if input.is_empty() {
            return CommandResult::Continue;
        }

        if input.starts_with('/') {
            self.handle_command(input).await
        } else {
            CommandResult::Chat(input.to_string())
        }
    }

    /// Dispatch a slash command via the CommandRegistry.
    async fn handle_command(&self, input: &str) -> CommandResult {
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.is_empty() {
            return CommandResult::Continue;
        }

        let cmd = parts[0];
        let args = &parts[1..];

        // Strip leading slash(es) for registry lookup
        let cmd_name = cmd.trim_start_matches('/');

        // Try registry dispatch
        if let Some(ref registry) = self.registry {
            let ctx = CommandContext {
                agent: self.agent.clone(),
                current_mode: self.current_mode.clone(),
                coding_loop: self.coding_loop.clone(),
                registry: Some(registry.clone()),
                task_service: self.task_service.clone(),
                scheduler: self.scheduler.clone(),
                plugin_runtime: self.plugin_runtime.clone(),
                prompt_assembly: self.prompt_assembly.clone(),
                review_integration: self.review_integration.clone(),
                interaction_mode: self.interaction_mode.clone(),
                staged_attachments: self.staged_attachments.clone(),
                app_state: self.app_state.clone(),
            };

            if let Some(outcome) = registry.dispatch(cmd_name, &ctx, args).await {
                match outcome {
                    crate::cli::command::CommandOutcome::Continue => {
                        return CommandResult::Continue;
                    }
                    crate::cli::command::CommandOutcome::Exit => return CommandResult::Exit,
                    crate::cli::command::CommandOutcome::Chat(msg) => {
                        return CommandResult::Chat(msg);
                    }
                }
            }
        }

        // Unknown command
        println!("\n  Unknown command: {}", cmd);
        println!("  Type /help for available commands");
        CommandResult::Continue
    }
}
