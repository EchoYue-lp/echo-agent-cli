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
    OpenArtifact,
    Workspace,

    // -- Context --
    Model,
    Provider,
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
    Plugins,

    // -- Coding --
    Plan,
    Mode,
    Tasks,
    Steer,
    TaskCancel,
    TaskPause,
    TaskResume,
    TaskBudget,
    TaskGoal,
    TaskRequirements,
    TaskRequirementSkip,
    SubagentMessage,
    SubagentFollowup,
    SubagentInterrupt,
    TaskRecovery,
    TaskRetry,
    TaskSkip,
    Test,
    CodeReview,
    Diff,
    Preview,
    Edit,
    Browser,
    Analysis,
    Terminal,
    Lsp,

    // -- Git --
    Git,
    Worktrees,

    // -- Pipeline --
    Pipeline,
    Workflow,

    // -- Security --
    Permission,

    // -- Scheduling --
    Cron,
    AutoMemory,
    RunReview,
    EvidenceInbox,
    MemoryReview,
    SkillCandidates,

    // -- Info --
    Tools,
    Cost,
    Trace,
    PromptDiagnostics,
    EvolutionDashboard,
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
            Self::OpenArtifact => "Open the latest or specified tool-output artifact",
            Self::Workspace => "Manage workspaces",

            Self::Model => "List, add, test, select, or delete models",
            Self::Provider => "List, add, update, or delete model providers",
            Self::Think => "Show or set the active model's thinking level",
            Self::System => "Show or set system prompt",
            Self::Memory => "Show memory contents",
            Self::Remember => "Save a fact to memory",
            Self::Forget => "Remove a fact from memory",
            Self::Attach => "Attach a file to the next message (/attach <path>)",
            Self::Skills => "List and manage skills",
            Self::Mcp => "List, load, or disconnect MCP servers",
            Self::Hooks => "List, reload, or test hooks",
            Self::Plugins => "Manage live plugins",

            Self::Plan => "Enter plan mode (read-only)",
            Self::Mode => "Switch interaction mode (auto/chat/task)",
            Self::Tasks => "Show active tasks",
            Self::Steer => "Inject guidance into the active turn or queue it",
            Self::TaskCancel => "Cancel the current or specified task run",
            Self::TaskPause => "Pause the current or specified task run",
            Self::TaskResume => "Resume the current or specified task run",
            Self::TaskBudget => "Set token and time budgets for a task run",
            Self::TaskGoal => "Update the paused task run Goal with optimistic concurrency",
            Self::TaskRequirements => "Show the Goal Requirement/Evidence completion gate",
            Self::TaskRequirementSkip => "Confirm a Skip for one exact Goal requirement",
            Self::SubagentMessage => "Send guidance to one exact active Subagent attempt",
            Self::SubagentFollowup => "Queue guidance for one exact future Subagent attempt",
            Self::SubagentInterrupt => "Interrupt one exact Subagent attempt",
            Self::TaskRecovery => "Show unresolved recovery barriers",
            Self::TaskRetry => "Confirm retry for an indeterminate task",
            Self::TaskSkip => "Skip an indeterminate task",
            Self::Test => "Run tests",
            Self::CodeReview => "Request a code review",
            Self::Diff => "Show git or file diff",
            Self::Preview => "Preview a workspace text file",
            Self::Edit => "Edit a workspace file in $VISUAL/$EDITOR",
            Self::Browser => "Show or switch the browser backend",
            Self::Analysis => "Create, inspect, and run file-backed analyses",
            Self::Terminal => "Manage and attach to interactive terminal sessions",
            Self::Lsp => "Inspect and manage workspace language servers",

            Self::Git => "Run a git command",
            Self::Worktrees => "Review and clean retained EKO worktrees",

            Self::Pipeline => "Manage pipelines",
            Self::Workflow => "Manage and execute durable Graph workflows",

            Self::Permission => "Show/set permission mode",

            Self::Cron => "Manage scheduled tasks",
            Self::AutoMemory => "Toggle auto-memory",
            Self::RunReview => "Propose evidence-linked memory candidates from the last run",
            Self::EvidenceInbox => "Review evidence-backed memory candidates",
            Self::MemoryReview => "Review and clean up accumulated memories",
            Self::SkillCandidates => "List skill candidates and drafts",

            Self::Tools => "List available tools",
            Self::Cost => "Show token cost summary",
            Self::Trace => "Show durable run diagnostics",
            Self::PromptDiagnostics => "Show prompt and protected-context diagnostics",
            Self::EvolutionDashboard => "Show on-demand evolution diagnostics",
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
            | Self::Copy
            | Self::OpenArtifact
            | Self::Workspace => Category::Session,
            Self::Model
            | Self::Provider
            | Self::Think
            | Self::System
            | Self::Memory
            | Self::Remember
            | Self::Forget
            | Self::Attach
            | Self::Skills
            | Self::Mcp
            | Self::Hooks
            | Self::Plugins => Category::Context,
            Self::Plan
            | Self::Mode
            | Self::Tasks
            | Self::Steer
            | Self::TaskCancel
            | Self::TaskPause
            | Self::TaskResume
            | Self::TaskBudget
            | Self::TaskGoal
            | Self::TaskRequirements
            | Self::TaskRequirementSkip
            | Self::SubagentMessage
            | Self::SubagentFollowup
            | Self::SubagentInterrupt
            | Self::TaskRecovery
            | Self::TaskRetry
            | Self::TaskSkip
            | Self::Test
            | Self::CodeReview
            | Self::Diff
            | Self::Preview
            | Self::Edit
            | Self::Browser
            | Self::Analysis
            | Self::Terminal
            | Self::Lsp => Category::Coding,
            Self::Git | Self::Worktrees => Category::Git,
            Self::Pipeline | Self::Workflow => Category::Pipeline,
            Self::Permission => Category::Security,
            Self::Cron
            | Self::AutoMemory
            | Self::RunReview
            | Self::EvidenceInbox
            | Self::MemoryReview
            | Self::SkillCandidates => Category::Scheduling,
            Self::Tools
            | Self::Cost
            | Self::Trace
            | Self::PromptDiagnostics
            | Self::EvolutionDashboard
            | Self::Help => Category::Info,
            Self::Quit | Self::Exit => Category::Exit,
        }
    }

    /// Example usage string (arguments portion).
    pub fn usage(self) -> &'static str {
        match self {
            Self::Model => {
                "[list|use <model>|test <model>|add <provider> <model> <protocol> [image] [audio] [video] [default]|delete <id>]"
            }
            Self::Provider => {
                "[list|add <id> <base-url> <protocol> [api-key-env] [requires-key]|update ...|delete <id>]"
            }
            Self::System => "[prompt text]",
            Self::Remember => "<fact>",
            Self::Forget => "<fact>",
            Self::Diff => "[file-path]",
            Self::Preview | Self::Edit => "<file-path>",
            Self::Browser => "[status|managed|chrome]",
            Self::Analysis => "[list|create <python|r> <title>|show <id>|run <id>]",
            Self::Terminal => {
                "[list|create <id> [cwd] [rows] [cols]|attach <id>|write <id> <data>|resize <id> <rows> <cols>|close <id>]"
            }
            Self::Lsp => "[list|status|start <language>|stop <language>|restart <language>]",
            Self::Git => "<git-args>",
            Self::Worktrees => "[list|cleanup|merge <run-id>|discard <run-id>]",
            Self::Pipeline => "[list|run <name>]",
            Self::Workflow => {
                "[list|show <id>|create <name> <definition|@path>|delete <id>|run <id> [json-input]]"
            }
            Self::Permission => "[ask|auto|deny]",
            Self::Cron => "[list|create|delete|pause|resume|run|reload]",
            Self::Test => "[test-name]",
            Self::Plan => "",
            Self::Mode => "[auto|chat|task]",
            Self::TaskCancel | Self::TaskPause | Self::TaskResume => "[run-id]",
            Self::TaskBudget => "<tokens|none> <seconds|none> [run-id]",
            Self::TaskGoal => "<expected-revision> [run-id] --reason <reason> --goal <new-goal>",
            Self::TaskRequirements => "[run-id]",
            Self::TaskRequirementSkip => {
                "<expected-goal-revision> <requirement-id> [run-id] --reason <reason>"
            }
            Self::SubagentMessage => {
                "<run-id> <task-id> <execution-id> <plan-revision> <attempt> <command-id> <instruction>"
            }
            Self::SubagentFollowup => {
                "<run-id> <task-id> <execution-id> <plan-revision> <attempt> <command-id> <instruction>"
            }
            Self::SubagentInterrupt => {
                "<run-id> <task-id> <execution-id> <plan-revision> <attempt> <command-id>"
            }
            Self::TaskRecovery => "[run-id]",
            Self::TaskRetry | Self::TaskSkip => "<task-id> [run-id]",
            Self::Steer => "<instruction>",
            Self::Sessions => "[query]",
            Self::Resume => "<conversation-id>",
            Self::Fork => "[title]",
            Self::Rename => "<title>",
            Self::DeleteSession => "<conversation-id>",
            Self::OpenArtifact => "[call-id|path]",
            Self::Workspace => "[new|list|switch|exit|link|migrate|info] [args]",
            Self::CodeReview => "[file-or-dir]",
            Self::Attach => "<file-path>",
            Self::Skills => "[list|search|install|uninstall|info|refresh] [args]",
            Self::Mcp => "[list|load <config>|disconnect <name>]",
            Self::Hooks => "[list|reload|test <event> [matcher]]",
            Self::Plugins => {
                "[list|install|uninstall|enable|disable|info|reload|config|themes|theme|styles|style|init|validate]"
            }
            Self::EvidenceInbox => {
                "[pending|expired|undoable|show|edit|accept|reject|undo] [candidate-id] [content]"
            }
            Self::Trace => "[run-id]",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_commands_are_first_class_coding_actions() -> Result<(), String> {
        let retry = "task-retry"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        let skip = "task-skip"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        let recovery = "task-recovery"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;

        assert_eq!(retry.category(), Category::Coding);
        assert_eq!(retry.usage(), "<task-id> [run-id]");
        assert_eq!(skip.category(), Category::Coding);
        assert_eq!(recovery.usage(), "[run-id]");
        Ok(())
    }

    #[test]
    fn task_budget_is_a_first_class_coding_action() -> Result<(), String> {
        let budget = "task-budget"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        assert_eq!(budget.category(), Category::Coding);
        assert_eq!(budget.usage(), "<tokens|none> <seconds|none> [run-id]");
        Ok(())
    }

    #[test]
    fn task_goal_is_a_first_class_coding_action() -> Result<(), String> {
        let goal = "task-goal"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        assert_eq!(goal.category(), Category::Coding);
        assert_eq!(
            goal.usage(),
            "<expected-revision> [run-id] --reason <reason> --goal <new-goal>"
        );
        Ok(())
    }

    #[test]
    fn trace_is_a_first_class_info_command() -> Result<(), String> {
        let trace = "trace"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        assert_eq!(trace.category(), Category::Info);
        assert_eq!(trace.usage(), "[run-id]");
        assert!(SlashCommand::complete("/tr").contains(&trace));
        Ok(())
    }

    #[test]
    fn worktrees_is_a_first_class_git_command() -> Result<(), String> {
        let worktrees = "worktrees"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        assert_eq!(worktrees.category(), Category::Git);
        assert_eq!(
            worktrees.usage(),
            "[list|cleanup|merge <run-id>|discard <run-id>]"
        );
        assert!(SlashCommand::complete("/work").contains(&worktrees));
        Ok(())
    }

    #[test]
    fn provider_and_model_commands_expose_dynamic_configuration() -> Result<(), String> {
        let provider = "provider"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        let model = "model"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;

        assert_eq!(provider.category(), Category::Context);
        assert!(provider.usage().contains("add <id> <base-url>"));
        assert!(model.usage().contains("[image] [audio] [video]"));
        Ok(())
    }

    #[test]
    fn developer_commands_are_first_class_coding_actions() -> Result<(), String> {
        let terminal = "terminal"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        let lsp = "lsp"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        assert_eq!(terminal.category(), Category::Coding);
        assert!(terminal.usage().contains("attach <id>"));
        assert_eq!(lsp.category(), Category::Coding);
        assert!(lsp.usage().contains("restart <language>"));
        Ok(())
    }

    #[test]
    fn workflow_is_a_first_class_pipeline_action() -> Result<(), String> {
        let workflow = "workflow"
            .parse::<SlashCommand>()
            .map_err(|error| error.to_string())?;
        assert_eq!(workflow.category(), Category::Pipeline);
        assert!(workflow.usage().contains("create <name>"));
        assert!(workflow.usage().contains("run <id>"));
        Ok(())
    }
}
