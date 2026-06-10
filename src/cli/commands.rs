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
