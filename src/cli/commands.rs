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
}

impl CommandHandler {
    pub fn new(agent: AgentHandle) -> Self {
        Self {
            agent,
            current_mode: "general".to_string(),
            coding_loop: None,
            registry: None,
            task_service: None,
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

// ── Price estimation (data-driven, updated for latest models) ─

const PRICE_TABLE: &[(&str, f64, f64, &str)] = &[
    // Anthropic Claude
    ("claude-opus-4-6", 0.015, 0.075, "USD"),
    ("claude-opus-4-5", 0.015, 0.075, "USD"),
    ("claude-opus-4", 0.015, 0.075, "USD"),
    ("claude-opus", 0.015, 0.075, "USD"),
    ("claude-sonnet-4-6", 0.003, 0.015, "USD"),
    ("claude-sonnet-4-5", 0.003, 0.015, "USD"),
    ("claude-sonnet-4", 0.003, 0.015, "USD"),
    ("claude-sonnet", 0.003, 0.015, "USD"),
    ("claude-haiku-4-5", 0.001, 0.005, "USD"),
    ("claude-haiku", 0.001, 0.005, "USD"),
    // OpenAI
    ("gpt-4o", 0.0025, 0.01, "USD"),
    ("gpt-4.1", 0.002, 0.008, "USD"),
    ("gpt-4.5", 0.075, 0.15, "USD"),
    ("gpt-4", 0.03, 0.06, "USD"),
    ("gpt-4-turbo", 0.01, 0.03, "USD"),
    ("gpt-3.5", 0.0005, 0.0015, "USD"),
    ("o3", 0.01, 0.04, "USD"),
    ("o4-mini", 0.0011, 0.0044, "USD"),
    // Qwen (CNY)
    ("qwen-max", 0.02, 0.06, "CNY"),
    ("qwen-plus", 0.004, 0.012, "CNY"),
    ("qwen-turbo", 0.002, 0.006, "CNY"),
    ("qwen3", 0.004, 0.012, "CNY"),
    // DeepSeek (CNY)
    ("deepseek-chat", 0.001, 0.002, "CNY"),
    ("deepseek-v3", 0.001, 0.002, "CNY"),
    ("deepseek-reasoner", 0.004, 0.016, "CNY"),
    ("deepseek-r1", 0.004, 0.016, "CNY"),
    // Zhipu GLM (CNY)
    ("glm", 0.005, 0.005, "CNY"),
    ("chatglm", 0.005, 0.005, "CNY"),
];

pub(crate) fn estimate_price(model: &str) -> (f64, f64, &'static str) {
    let m = model.to_lowercase();
    for (pattern, in_price, out_price, currency) in PRICE_TABLE {
        if m.contains(pattern) {
            return (*in_price, *out_price, currency);
        }
    }
    // Default: mid-tier estimate
    (0.004, 0.012, "USD")
}
