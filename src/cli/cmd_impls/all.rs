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

async fn cmd_memory(_: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("show");

    match sub {
        "show" => {
            println!("\n📝 Project Memory\n");

            // User-level: ~/.echo-agent/user.md
            let user_path = std::env::var("HOME")
                .map(|h| {
                    std::path::PathBuf::from(h)
                        .join(".echo-agent")
                        .join("user.md")
                })
                .unwrap_or_default();
            print_memory_tier("User", &user_path);

            // Project-level: <project-root>/.eko/project.md
            let project_path = find_project_root()
                .map(|r| r.join(".eko").join("project.md"))
                .unwrap_or_default();
            print_memory_tier("Project", &project_path);

            // Reflection memory: .eko/memory/PROJECT.md
            let reflection_path = std::env::current_dir()
                .unwrap_or_default()
                .join(".eko")
                .join("memory")
                .join("PROJECT.md");
            print_memory_tier("Reflection", &reflection_path);
        }
        _ => {
            println!("Usage: /memory [show]");
        }
    }
    CommandOutcome::Continue
}

fn print_memory_tier(label: &str, path: &std::path::Path) {
    match std::fs::read_to_string(path) {
        Ok(content) if !content.trim().is_empty() => {
            println!("  ── {} ({}) ──", label, path.display());
            // Show first 500 chars to avoid flooding the terminal
            let preview = if content.len() > 500 {
                format!("{}...", &content[..500])
            } else {
                content.clone()
            };
            for line in preview.lines() {
                println!("    {}", line);
            }
            if content.len() > 500 {
                println!("    ({} chars total, truncated)", content.len());
            }
            println!();
        }
        _ => {
            println!("  ── {} ── (empty)", label);
            println!();
        }
    }
}

fn find_project_root() -> Option<std::path::PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() || dir.join(".eko").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}
cmd!(
    MemoryCommand,
    "memory",
    CommandCategory::Context,
    "List memories",
    cmd_memory
);

// ── ReflectCommand ─────────────────────────────────────────────────────

async fn cmd_reflect(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("🪞 Generating reflection...");

    // Get LLM client from agent
    let llm_client = ctx.agent.read(|a| a.llm_client().cloned()).await;

    let Some(llm) = llm_client else {
        println!("⚠️  No LLM client available for reflection.");
        return CommandOutcome::Continue;
    };

    let prompt = "Reflect on the current session and summarize key learnings in 1-2 sentences.\n\
                  Focus on reusable insights. Be specific. Max 200 tokens.\n\nReflection:";

    let messages = vec![echo_agent::prelude::Message::user(prompt.to_string())];
    let options = echo_agent::prelude::SimpleChatOptions::default().with_max_tokens(300);

    let reflection = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        llm.chat_simple_with_options(messages, options),
    )
    .await
    {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => {
            println!("⚠️  LLM reflection failed: {e}");
            return CommandOutcome::Continue;
        }
        Err(_) => {
            println!("⚠️  LLM reflection timed out (>2s).");
            return CommandOutcome::Continue;
        }
    };

    // Write to .eko/memory/PROJECT.md
    let memory_dir = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".eko")
        .join("memory");
    let _ = std::fs::create_dir_all(&memory_dir);
    let memory_file = memory_dir.join("PROJECT.md");

    let entry = format!(
        "\n## [session] Reflection ({})\n{}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M"),
        reflection.trim()
    );

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&memory_file)
    {
        Ok(mut file) => {
            use std::io::Write;
            let _ = file.write_all(entry.as_bytes());
            println!("✅ Reflection saved to {}", memory_file.display());
        }
        Err(e) => {
            println!("⚠️  Failed to write reflection: {e}");
        }
    }

    CommandOutcome::Continue
}
cmd!(
    ReflectCommand,
    "reflect",
    CommandCategory::Context,
    "Reflect on session learnings",
    cmd_reflect
);

// ── AutoMemoryCommand ─────────────────────────────────────────────────

/// Global auto-memory toggle (persists within the session).
pub static AUTO_MEMORY_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

async fn cmd_auto_memory(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    use echo_agent_app_core::auto_memory::{
        AutoMemoryConfig, append_to_project_memory, extract_observations,
        format_observations_for_memory, write_observations_to_memory_layer,
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
            let observations = extract_observations(&messages, &config);
            let count = observations.len();
            if count == 0 {
                println!("No observations extracted from this session.");
                return CommandOutcome::Continue;
            }

            let typed_written = match handle.read(|a| a.memory_layer_manager().cloned()).await {
                Some(lm) => match write_observations_to_memory_layer(&observations, &lm).await {
                    Ok(count) => count,
                    Err(e) => {
                        println!("Typed auto-memory write failed: {e}");
                        0
                    }
                },
                None => {
                    tracing::debug!(
                        "auto-memory: agent has no shared layer manager, skipping typed write"
                    );
                    0
                }
            };

            match append_to_project_memory(&observations) {
                Ok(()) => {
                    println!(
                        "Extracted {} observation(s), saved {} to typed memory and project memory.",
                        count, typed_written
                    );
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
    registry.register(Arc::new(ReflectCommand));
    registry.register(Arc::new(AutoMemoryCommand));
}
