//! Slash command trait and registry — modular command extension.
//!
//! Instead of a giant `handle_command()` match, commands implement
//! the [`SlashCommand`] trait and register with [`CommandRegistry`].
//!
//! # Adding a new command
//!
//! ```rust,ignore
//! struct MyCommand;
//!
//! #[async_trait]
//! impl SlashCommand for MyCommand {
//!     fn name(&self) -> &'static str { "mycmd" }
//!     fn aliases(&self) -> &'static [&'static str] { &["mc"] }
//!     fn description(&self) -> &'static str { "Does something useful" }
//!     fn category(&self) -> CommandCategory { CommandCategory::Coding }
//!
//!     async fn run(&self, ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
//!         println!("Running mycmd with args: {:?}", args);
//!         CommandOutcome::Continue
//!     }
//! }
//! ```

use crate::agent_handle::AgentHandle;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// ── CommandOutcome ──────────────────────────────────────────────────

/// What the REPL should do after a command executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Continue the REPL loop.
    Continue,
    /// Exit the REPL.
    Exit,
    /// Execute a chat message with the agent.
    Chat(String),
}

// ── CommandCategory ──────────────────────────────────────────────────

/// Logical grouping for help display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandCategory {
    Session,
    Context,
    Config,
    Coding,
    Info,
    Debug,
    Advanced,
    Output,
    Profiles,
    Sessions,
    Help,
}

impl CommandCategory {
    pub fn display_name(&self) -> &str {
        match self {
            Self::Session => "Session",
            Self::Context => "Context",
            Self::Config => "Config",
            Self::Coding => "Coding",
            Self::Info => "Info",
            Self::Debug => "Debug",
            Self::Advanced => "Advanced",
            Self::Output => "Output",
            Self::Profiles => "Profiles",
            Self::Sessions => "Sessions",
            Self::Help => "Help",
        }
    }
}

// ── CommandContext ───────────────────────────────────────────────────

/// Shared context available to all command handlers.
pub struct CommandContext {
    /// Agent handle for read/write access.
    pub agent: AgentHandle,
    /// Current mode (e.g. "general", "coding").
    pub current_mode: String,
    /// Optional coding loop for coding-mode commands.
    pub coding_loop: Option<Arc<tokio::sync::Mutex<crate::project::coding_loop::CodingLoop>>>,
    /// Command registry (for /help to list commands dynamically).
    pub registry: Option<Arc<CommandRegistry>>,
    /// Background task service for submitting long-running tasks.
    pub task_service: Option<Arc<echo_agent_app_core::tasks::BackgroundTaskService>>,
}

impl CommandContext {
    pub fn new(agent: AgentHandle) -> Self {
        Self {
            agent,
            current_mode: "general".into(),
            coding_loop: None,
            registry: None,
            task_service: None,
        }
    }

    pub fn with_mode(mut self, mode: &str) -> Self {
        self.current_mode = mode.to_string();
        self
    }

    pub fn with_coding_loop(
        mut self,
        cl: Arc<tokio::sync::Mutex<crate::project::coding_loop::CodingLoop>>,
    ) -> Self {
        self.coding_loop = Some(cl);
        self
    }

    pub fn with_registry(mut self, registry: Arc<CommandRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn with_task_service(
        mut self,
        service: Arc<echo_agent_app_core::tasks::BackgroundTaskService>,
    ) -> Self {
        self.task_service = Some(service);
        self
    }

    pub fn is_coding_mode(&self) -> bool {
        self.current_mode == "coding"
    }
}

// ── SubCommandDef ───────────────────────────────────────────────────

/// A subcommand definition for commands that group related operations.
pub struct SubCommandDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
}

// ── SlashCommand trait ───────────────────────────────────────────────

/// A slash command that can be registered in the command registry.
///
/// Implement this trait to add new commands. Register with [`CommandRegistry::register`].
pub trait SlashCommand: Send + Sync {
    /// Primary command name (without the leading slash).
    fn name(&self) -> &'static str;

    /// Alternative names for this command.
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// One-line description for /help output.
    fn description(&self) -> &'static str;

    /// Category for /help grouping.
    fn category(&self) -> CommandCategory;

    /// Subcommand definitions. Empty by default (flat command).
    fn subcommands(&self) -> Vec<SubCommandDef> {
        vec![]
    }

    /// Execute the command. Returns what the REPL should do next.
    fn run<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: &'a [&'a str],
    ) -> Pin<Box<dyn Future<Output = CommandOutcome> + Send + 'a>>;
}

// ── Convenience macro ─────────────────────────────────────────────────

/// Macro to define a SlashCommand struct + impl with minimal boilerplate.
///
/// ```rust,ignore
/// cmd!(MyCmd, "mycmd", ["mc"], CommandCategory::Info, "Does a thing", my_handler);
/// ```
///
/// The handler must be an async fn: `async fn handler(&CommandContext, &[&str]) -> CommandOutcome`.
macro_rules! cmd {
    // With aliases
    ($name:ident, $cmd_name:literal, [$($alias:literal),*], $cat:expr, $desc:literal, $body:expr) => {
        pub struct $name;
        impl $crate::cli::command::SlashCommand for $name {
            fn name(&self) -> &'static str { $cmd_name }
            fn aliases(&self) -> &'static [&'static str] { &[$($alias),*] }
            fn description(&self) -> &'static str { $desc }
            fn category(&self) -> $crate::cli::command::CommandCategory { $cat }
            fn run<'a>(&'a self, ctx: &'a $crate::cli::command::CommandContext, args: &'a [&'a str]) -> std::pin::Pin<Box<dyn std::future::Future<Output = $crate::cli::command::CommandOutcome> + Send + 'a>> {
                Box::pin(async move { $body(ctx, args).await })
            }
        }
    };
    // Without aliases
    ($name:ident, $cmd_name:literal, $cat:expr, $desc:literal, $body:expr) => {
        pub struct $name;
        impl $crate::cli::command::SlashCommand for $name {
            fn name(&self) -> &'static str { $cmd_name }
            fn description(&self) -> &'static str { $desc }
            fn category(&self) -> $crate::cli::command::CommandCategory { $cat }
            fn run<'a>(&'a self, ctx: &'a $crate::cli::command::CommandContext, args: &'a [&'a str]) -> std::pin::Pin<Box<dyn std::future::Future<Output = $crate::cli::command::CommandOutcome> + Send + 'a>> {
                Box::pin(async move { $body(ctx, args).await })
            }
        }
    };
}
pub(crate) use cmd;

// ── CommandRegistry ──────────────────────────────────────────────────

/// Registry of all slash commands.
///
/// Commands are looked up by name or alias. New commands can be added
/// via `register()` without modifying the dispatch logic.
pub struct CommandRegistry {
    commands: Vec<Arc<dyn SlashCommand>>,
    /// name/alias → index into commands
    by_name: HashMap<String, usize>,
}

impl CommandRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    /// Register a command.
    pub fn register(&mut self, cmd: Arc<dyn SlashCommand>) {
        let idx = self.commands.len();
        self.by_name.insert(cmd.name().to_string(), idx);
        for alias in cmd.aliases() {
            self.by_name.insert(alias.to_string(), idx);
        }
        self.commands.push(cmd);
    }

    /// Look up a command by name or alias. Returns None if not found.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn SlashCommand>> {
        self.by_name.get(name).map(|&idx| &self.commands[idx])
    }

    /// Dispatch a command by name. Returns None if the command doesn't exist.
    ///
    /// For commands with subcommands, the first arg is treated as the subcommand name.
    /// If no matching subcommand is found, the command receives all args and handles
    /// the error/help display itself.
    pub async fn dispatch(
        &self,
        name: &str,
        ctx: &CommandContext,
        args: &[&str],
    ) -> Option<CommandOutcome> {
        let cmd = self.get(name)?;
        let subs = cmd.subcommands();

        if !subs.is_empty() {
            if let Some(sub_name) = args.first() {
                let mut matched = false;
                for sub in &subs {
                    if sub.name == *sub_name || sub.aliases.contains(sub_name) {
                        matched = true;
                        break;
                    }
                }
                if matched {
                    return Some(cmd.run(ctx, args).await);
                }
                // No subcommand matched — pass through for command to handle help
            }
        }

        Some(cmd.run(ctx, args).await)
    }

    /// List all commands grouped by category (for /help).
    pub fn by_category(&self) -> HashMap<CommandCategory, Vec<&Arc<dyn SlashCommand>>> {
        let mut map: HashMap<CommandCategory, Vec<&Arc<dyn SlashCommand>>> = HashMap::new();
        for cmd in &self.commands {
            map.entry(cmd.category()).or_default().push(cmd);
        }
        map
    }

    /// Number of registered commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}
