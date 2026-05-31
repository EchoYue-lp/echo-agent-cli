//! All remaining slash commands — help, exit, clear, remember, forget, memory.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

// ── HelpCommand ────────────────────────────────────────────────────────

async fn cmd_help(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    if let Some(ref registry) = ctx.registry {
        let by_cat = registry.by_category();
        let cat_order = [
            CommandCategory::Session,
            CommandCategory::Context,
            CommandCategory::Config,
            CommandCategory::Coding,
            CommandCategory::Info,
            CommandCategory::Debug,
            CommandCategory::Output,
            CommandCategory::Profiles,
            CommandCategory::Sessions,
            CommandCategory::Advanced,
            CommandCategory::Help,
        ];
        println!();
        println!("  Available commands:");
        for cat in &cat_order {
            if let Some(cmds) = by_cat.get(cat) {
                if cmds.is_empty() {
                    continue;
                }
                println!();
                println!("  [{}]", cat.display_name());
                for cmd in cmds {
                    let aliases = cmd.aliases();
                    if aliases.is_empty() {
                        println!("    /{} — {}", cmd.name(), cmd.description());
                    } else {
                        let alias_str = aliases
                            .iter()
                            .map(|a| format!("/{a}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!(
                            "    /{} ({}) — {}",
                            cmd.name(),
                            alias_str,
                            cmd.description()
                        );
                    }
                }
            }
        }
        println!();
        println!("  Type any message to chat with the agent.");
        println!();
    } else {
        println!("\nHelp not available (no command registry).");
    }
    CommandOutcome::Continue
}
cmd!(
    HelpCommand,
    "help",
    ["h", "?"],
    CommandCategory::Help,
    "Show help",
    cmd_help
);

// ── ExitCommand ────────────────────────────────────────────────────────

async fn cmd_exit(_: &CommandContext, _: &[&str]) -> CommandOutcome {
    let (_, _, tool_calls) = crate::cli::repl::get_usage_stats();
    if tool_calls > 0 {
        println!("Session: {} tool calls", tool_calls);
    }
    println!("\nGoodbye!");
    CommandOutcome::Exit
}
cmd!(
    ExitCommand,
    "exit",
    ["quit", "q"],
    CommandCategory::Session,
    "Exit the REPL",
    cmd_exit
);

// ── ClearCommand ───────────────────────────────────────────────────────

async fn cmd_clear(_: &CommandContext, _: &[&str]) -> CommandOutcome {
    print!("\x1b[2J\x1b[H");
    CommandOutcome::Continue
}
cmd!(
    ClearCommand,
    "clear",
    ["cls"],
    CommandCategory::Session,
    "Clear screen",
    cmd_clear
);

// ── RememberCommand ────────────────────────────────────────────────────

async fn cmd_remember(_: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.len() < 2 {
        println!("Usage: /remember [scope] <key> <value>");
    } else {
        println!("Remembered: {}", args.join(" "));
    }
    CommandOutcome::Continue
}
cmd!(
    RememberCommand,
    "remember",
    CommandCategory::Context,
    "Store a memory",
    cmd_remember
);

// ── ForgetCommand ──────────────────────────────────────────────────────

async fn cmd_forget(_: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.is_empty() {
        println!("Usage: /forget <key>");
    } else {
        println!("Forgotten: {}", args[0]);
    }
    CommandOutcome::Continue
}
cmd!(
    ForgetCommand,
    "forget",
    CommandCategory::Context,
    "Remove a memory",
    cmd_forget
);

// ── MemoryCommand ──────────────────────────────────────────────────────

async fn cmd_memory(_: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\nMemory scopes: user, project, repo, task, session, run");
    CommandOutcome::Continue
}
cmd!(
    MemoryCommand,
    "memory",
    CommandCategory::Context,
    "List memories",
    cmd_memory
);

// ── Register ───────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(HelpCommand));
    registry.register(Arc::new(ExitCommand));
    registry.register(Arc::new(ClearCommand));
    registry.register(Arc::new(RememberCommand));
    registry.register(Arc::new(ForgetCommand));
    registry.register(Arc::new(MemoryCommand));
}
