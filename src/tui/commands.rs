//! Slash commands for the TUI command palette.
//!
//! Enum-driven with strum for iteration, string conversion, and parsing.

use strum_macros::{AsRefStr, EnumIter, EnumString, IntoStaticStr};

/// Category groupings for the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Session,
    Context,
    Coding,
    Git,
    Pipeline,
    Security,
    Scheduling,
    Info,
    Exit,
}

impl Category {
    /// Display label with icon for the palette grouping.
    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Context => "Context",
            Self::Coding => "Coding",
            Self::Git => "Git",
            Self::Pipeline => "Pipeline",
            Self::Security => "Security",
            Self::Scheduling => "Scheduling",
            Self::Info => "Info",
            Self::Exit => "Exit",
        }
    }

    /// Icon character for the palette.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Session => "[S]",
            Self::Context => "[C]",
            Self::Coding => "[>]",
            Self::Git => "[G]",
            Self::Pipeline => "[P]",
            Self::Security => "[!]",
            Self::Scheduling => "[@]",
            Self::Info => "[i]",
            Self::Exit => "[x]",
        }
    }
}

/// All slash commands available in the TUI.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, EnumIter, AsRefStr, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SlashCommand {
    // -- Session --
    Clear,
    History,
    Stats,
    Status,
    New,
    Sessions,
    Resume,
    Fork,
    Rename,
    DeleteSession,
    Compact,
    Copy,

    // -- Context --
    Model,
    Think,
    System,
    Memory,
    Remember,
    Forget,
    /// Attach a file (image/document) to the next message (B5.3 multimodal).
    Attach,
    Skills,
    Mcp,
    Hooks,

    // -- Coding --
    Plan,
    Mode,
    Tasks,
    Steer,
    TaskCancel,
    TaskPause,
    TaskResume,
    Test,
    CodeReview,
    Diff,
    Preview,
    Edit,
    Browser,

    // -- Git --
    Git,

    // -- Pipeline --
    Pipeline,

    // -- Security --
    Permission,

    // -- Scheduling --
    Cron,
    AutoMemory,
    MemoryReview,
    SkillCandidates,

    // -- Info --
    Tools,
    Cost,
    Help,

    // -- Exit --
    Quit,
    Exit,
}

impl SlashCommand {
    /// Short human-readable description.
    pub fn description(self) -> &'static str {
        match self {
            Self::Clear => "Clear conversation and start fresh",
            Self::History => "Show session history",
            Self::Stats => "Show session statistics",
            Self::Status => "Show agent status",
            Self::New => "Start a new session",
            Self::Sessions => "List or search persisted conversations",
            Self::Resume => "Resume a persisted conversation",
            Self::Fork => "Fork the current conversation",
            Self::Rename => "Rename the current conversation",
            Self::DeleteSession => "Delete a persisted conversation",
            Self::Compact => "Compress context window",
            Self::Copy => "Copy the last response to clipboard (or Ctrl+Y)",

            Self::Model => "Switch or show current model",
            Self::Think => "Toggle reasoning/thinking display",
            Self::System => "Show or set system prompt",
            Self::Memory => "Show memory contents",
            Self::Remember => "Save a fact to memory",
            Self::Forget => "Remove a fact from memory",
            Self::Attach => "Attach a file to the next message (/attach <path>)",
            Self::Skills => "List and manage skills",
            Self::Mcp => "List, load, or disconnect MCP servers",
            Self::Hooks => "List, reload, or test hooks",

            Self::Plan => "Enter plan mode (read-only)",
            Self::Mode => "Switch interaction mode (auto/chat/task)",
            Self::Tasks => "Show active tasks",
            Self::Steer => "Inject guidance into the active turn or queue it",
            Self::TaskCancel => "Cancel the current or specified task run",
            Self::TaskPause => "Pause the current or specified task run",
            Self::TaskResume => "Resume the current or specified task run",
            Self::Test => "Run tests",
            Self::CodeReview => "Request a code review",
            Self::Diff => "Show git or file diff",
            Self::Preview => "Preview a workspace text file",
            Self::Edit => "Edit a workspace file in $VISUAL/$EDITOR",
            Self::Browser => "Show or switch the browser backend",

            Self::Git => "Run a git command",

            Self::Pipeline => "Manage pipelines",

            Self::Permission => "Show/set permission mode",

            Self::Cron => "Manage scheduled tasks",
            Self::AutoMemory => "Toggle auto-memory",
            Self::MemoryReview => "Review and clean up accumulated memories",
            Self::SkillCandidates => "List skill candidates and drafts",

            Self::Tools => "List available tools",
            Self::Cost => "Show token cost summary",
            Self::Help => "Show help",

            Self::Quit => "Quit the TUI",
            Self::Exit => "Quit the TUI",
        }
    }

    /// Which category this command belongs to.
    pub fn category(self) -> Category {
        match self {
            Self::Clear
            | Self::History
            | Self::Stats
            | Self::Status
            | Self::New
            | Self::Sessions
            | Self::Resume
            | Self::Fork
            | Self::Rename
            | Self::DeleteSession
            | Self::Compact
            | Self::Copy => Category::Session,
            Self::Model
            | Self::Think
            | Self::System
            | Self::Memory
            | Self::Remember
            | Self::Forget
            | Self::Attach
            | Self::Skills
            | Self::Mcp
            | Self::Hooks => Category::Context,
            Self::Plan
            | Self::Mode
            | Self::Tasks
            | Self::Steer
            | Self::TaskCancel
            | Self::TaskPause
            | Self::TaskResume
            | Self::Test
            | Self::CodeReview
            | Self::Diff
            | Self::Preview
            | Self::Edit
            | Self::Browser => Category::Coding,
            Self::Git => Category::Git,
            Self::Pipeline => Category::Pipeline,
            Self::Permission => Category::Security,
            Self::Cron | Self::AutoMemory | Self::MemoryReview | Self::SkillCandidates => {
                Category::Scheduling
            }
            Self::Tools | Self::Cost | Self::Help => Category::Info,
            Self::Quit | Self::Exit => Category::Exit,
        }
    }

    /// Example usage string (arguments portion).
    pub fn usage(self) -> &'static str {
        match self {
            Self::Model => "[model-name]",
            Self::System => "[prompt text]",
            Self::Remember => "<fact>",
            Self::Forget => "<fact>",
            Self::Diff => "[file-path]",
            Self::Preview | Self::Edit => "<file-path>",
            Self::Browser => "[status|managed|chrome]",
            Self::Git => "<git-args>",
            Self::Pipeline => "[list|run <name>]",
            Self::Permission => "[ask|auto|deny]",
            Self::Cron => "[list|add|remove]",
            Self::Test => "[test-name]",
            Self::Plan => "",
            Self::Mode => "[auto|chat|task]",
            Self::TaskCancel | Self::TaskPause | Self::TaskResume => "[run-id]",
            Self::Steer => "<instruction>",
            Self::Sessions => "[query]",
            Self::Resume => "<conversation-id>",
            Self::Fork => "[title]",
            Self::Rename => "<title>",
            Self::DeleteSession => "<conversation-id>",
            Self::CodeReview => "[file-or-dir]",
            Self::Attach => "<file-path>",
            Self::Skills => "[list|search|install|uninstall|info|refresh] [args]",
            Self::Mcp => "[list|load <config>|disconnect <name>]",
            Self::Hooks => "[list|reload|test <event>]",
            _ => "",
        }
    }

    /// Return the slash form: `/command-name`.
    pub fn slash_name(self) -> String {
        format!("/{}", self.as_ref())
    }

    /// All commands grouped by category, in declaration order.
    pub fn grouped() -> Vec<(Category, Vec<SlashCommand>)> {
        use strum::IntoEnumIterator;
        let mut groups: Vec<(Category, Vec<SlashCommand>)> = Vec::new();
        for cmd in SlashCommand::iter() {
            let cat = cmd.category();
            if let Some(last) = groups.last_mut()
                && last.0 == cat
            {
                last.1.push(cmd);
                continue;
            }
            groups.push((cat, vec![cmd]));
        }
        groups
    }

    /// Filter commands whose slash-name starts with `query` (case-insensitive).
    pub fn complete(query: &str) -> Vec<SlashCommand> {
        use strum::IntoEnumIterator;
        let q = query.to_lowercase();
        SlashCommand::iter()
            .filter(|c| c.slash_name().starts_with(&q))
            .collect()
    }
}
