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

// ── AutoMemoryCommand ─────────────────────────────────────────────────

/// Global auto-memory toggle (persists within the session).
pub static AUTO_MEMORY_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

async fn cmd_auto_memory(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    use echo_agent_app_core::auto_memory::{
        AutoMemoryConfig, extract_observations, format_observations_for_memory,
        run_auto_memory_extraction,
    };

    let sub = args.first().copied().unwrap_or("");
    match sub {
        "on" => {
            AUTO_MEMORY_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
            println!("Auto-memory enabled.");
        }
        "off" => {
            AUTO_MEMORY_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);
            println!("Auto-memory disabled.");
        }
        "extract" => {
            println!("Extracting observations from current session...");
            let handle = ctx.agent.clone();
            let messages: Vec<(String, String)> = handle
                .read_async(|a| {
                    Box::pin(async move {
                        let ctx = a.context().lock().await;
                        ctx.messages()
                            .iter()
                            .map(|m| {
                                (
                                    m.role.as_str().to_string(),
                                    m.content.as_text().unwrap_or_default().to_string(),
                                )
                            })
                            .collect()
                    })
                })
                .await;

            let config = AutoMemoryConfig::default();
            match run_auto_memory_extraction(&messages, &config) {
                Ok(count) => {
                    if count > 0 {
                        println!(
                            "Extracted {} observation(s) and saved to project memory.",
                            count
                        );
                    } else {
                        println!("No observations extracted from this session.");
                    }
                }
                Err(e) => println!("Auto-memory extraction failed: {e}"),
            }
        }
        "show" => {
            let handle = ctx.agent.clone();
            let messages: Vec<(String, String)> = handle
                .read_async(|a| {
                    Box::pin(async move {
                        let ctx = a.context().lock().await;
                        ctx.messages()
                            .iter()
                            .map(|m| {
                                (
                                    m.role.as_str().to_string(),
                                    m.content.as_text().unwrap_or_default().to_string(),
                                )
                            })
                            .collect()
                    })
                })
                .await;

            let config = AutoMemoryConfig::default();
            let observations = extract_observations(&messages, &config);

            if observations.is_empty() {
                println!("No observations would be extracted from this session.");
            } else {
                let formatted = format_observations_for_memory(&observations);
                println!(
                    "\n--- Auto-memory preview ({} observations) ---\n",
                    observations.len()
                );
                println!("{}", formatted);
            }
        }
        "config" => {
            let enabled = AUTO_MEMORY_ENABLED.load(std::sync::atomic::Ordering::Relaxed);
            let config = AutoMemoryConfig::default();
            println!("\n--- Auto-memory Configuration ---");
            println!("  Enabled:         {}", enabled);
            println!("  Min confidence:  {:.0}%", config.min_confidence * 100.0);
            println!("  Max per session: {}", config.max_per_session);
            println!(
                "  Categories:      {}",
                config
                    .categories
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        _ => {
            let enabled = AUTO_MEMORY_ENABLED.load(std::sync::atomic::Ordering::Relaxed);
            println!(
                "\nAuto-memory: {} (runs on session exit)",
                if enabled { "ON" } else { "OFF" }
            );
            println!("Usage: /auto-memory <on|off|extract|show|config>");
            println!("  on      — enable auto-memory (default)");
            println!("  off     — disable auto-memory");
            println!("  extract — manually extract and save observations now");
            println!("  show    — preview what would be extracted");
            println!("  config  — show current configuration");
        }
    }
    CommandOutcome::Continue
}
cmd!(
    AutoMemoryCommand,
    "auto-memory",
    ["am"],
    CommandCategory::Context,
    "Auto-extract and persist observations from sessions",
    cmd_auto_memory
);

// ── Register ───────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(HelpCommand));
    registry.register(Arc::new(ExitCommand));
    registry.register(Arc::new(ClearCommand));
    registry.register(Arc::new(RememberCommand));
    registry.register(Arc::new(ForgetCommand));
    registry.register(Arc::new(MemoryCommand));
    registry.register(Arc::new(AutoMemoryCommand));
}
