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
    Reset,
    History,
    Stats,
    Status,
    New,
    Compact,

    // -- Context --
    Mode,
    Model,
    Think,
    System,
    Memory,
    Remember,
    Forget,

    // -- Coding --
    Plan,
    Tasks,
    Test,
    CodeReview,
    Diff,

    // -- Git --
    Git,

    // -- Pipeline --
    Pipeline,

    // -- Security --
    Permission,

    // -- Scheduling --
    Cron,
    AutoMemory,

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
            Self::Reset => "Reset conversation history",
            Self::History => "Show session history",
            Self::Stats => "Show session statistics",
            Self::Status => "Show agent status",
            Self::New => "Start a new session",
            Self::Compact => "Compress context window",

            Self::Mode => "Switch agent mode (general/coding/research/data/writing)",
            Self::Model => "Switch or show current model",
            Self::Think => "Toggle reasoning/thinking display",
            Self::System => "Show or set system prompt",
            Self::Memory => "Show memory contents",
            Self::Remember => "Save a fact to memory",
            Self::Forget => "Remove a fact from memory",

            Self::Plan => "Enter plan mode (read-only)",
            Self::Tasks => "Show active tasks",
            Self::Test => "Run tests",
            Self::CodeReview => "Request a code review",
            Self::Diff => "Show git or file diff",

            Self::Git => "Run a git command",

            Self::Pipeline => "Manage pipelines",

            Self::Permission => "Show/set permission mode",

            Self::Cron => "Manage scheduled tasks",
            Self::AutoMemory => "Toggle auto-memory",

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
            Self::Reset
            | Self::History
            | Self::Stats
            | Self::Status
            | Self::New
            | Self::Compact => Category::Session,
            Self::Mode
            | Self::Model
            | Self::Think
            | Self::System
            | Self::Memory
            | Self::Remember
            | Self::Forget => Category::Context,
            Self::Plan | Self::Tasks | Self::Test | Self::CodeReview | Self::Diff => {
                Category::Coding
            }
            Self::Git => Category::Git,
            Self::Pipeline => Category::Pipeline,
            Self::Permission => Category::Security,
            Self::Cron | Self::AutoMemory => Category::Scheduling,
            Self::Tools | Self::Cost | Self::Help => Category::Info,
            Self::Quit | Self::Exit => Category::Exit,
        }
    }

    /// Example usage string (arguments portion).
    pub fn usage(self) -> &'static str {
        match self {
            Self::Mode => "[general|coding|research|data|writing]",
            Self::Model => "[model-name]",
            Self::System => "[prompt text]",
            Self::Remember => "<fact>",
            Self::Forget => "<fact>",
            Self::Diff => "[file-path]",
            Self::Git => "<git-args>",
            Self::Pipeline => "[list|run <name>]",
            Self::Permission => "[ask|auto|deny]",
            Self::Cron => "[list|add|remove]",
            Self::Test => "[test-name]",
            Self::Plan => "",
            Self::CodeReview => "[file-or-dir]",
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
            if let Some(last) = groups.last_mut() {
                if last.0 == cat {
                    last.1.push(cmd);
                    continue;
                }
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
