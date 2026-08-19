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

async fn cmd_remember(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let content = args.join(" ");
    if content.trim().is_empty() {
        println!("Usage: /remember <fact>");
        return CommandOutcome::Continue;
    }
    let Some(integration) = ctx.review_integration.as_ref() else {
        println!("Layered memory is not configured for this agent.");
        return CommandOutcome::Continue;
    };
    let memory_lease = match integration.lease_generation() {
        Ok(lease) => lease,
        Err(error) => {
            println!("Cannot save memory while the workspace is switching: {error}");
            return CommandOutcome::Continue;
        }
    };
    let layer_manager = match memory_lease.create_layer_manager() {
        Ok(manager) => Arc::new(manager),
        Err(error) => {
            println!("Failed to initialize layered memory: {error}");
            return CommandOutcome::Continue;
        }
    };
    let key = uuid::Uuid::new_v4().to_string();
    let meta = echo_agent::memory::MemoryMeta::new(
        echo_agent::memory::MemoryType::ProjectFact,
        echo_agent::memory::MemorySource::ExplicitSave,
        "explicit",
    );
    match layer_manager.write_memory(&key, content.trim(), meta).await {
        Ok(promotion) => {
            if promotion.is_some() {
                let root = ctx.agent.read(|agent| agent.working_dir()).await;
                ctx.agent
                    .write_async(|agent| {
                        Box::pin(async move {
                            echo_agent_app_core::unified_memory::refresh_hot_memory_projection(
                                agent,
                                root.as_deref(),
                            )
                            .await;
                        })
                    })
                    .await;
            }
            println!("Memory saved with key: {key}");
        }
        Err(error) => println!("Failed to save memory: {error}"),
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

async fn cmd_forget(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let query = args.join(" ");
    if query.trim().is_empty() {
        println!("Usage: /forget <key-or-query>");
        return CommandOutcome::Continue;
    }
    let Some(integration) = ctx.review_integration.as_ref() else {
        println!("Layered memory is not configured for this agent.");
        return CommandOutcome::Continue;
    };
    let memory_lease = match integration.lease_generation() {
        Ok(lease) => lease,
        Err(error) => {
            println!("Cannot remove memory while the workspace is switching: {error}");
            return CommandOutcome::Continue;
        }
    };
    let layer_manager = match memory_lease.create_layer_manager() {
        Ok(manager) => Arc::new(manager),
        Err(error) => {
            println!("Failed to initialize layered memory: {error}");
            return CommandOutcome::Continue;
        }
    };
    let key = if layer_manager.locate(query.trim()).await.is_some() {
        Some(query.trim().to_string())
    } else {
        match layer_manager.search_layered(query.trim(), 20).await {
            Ok(matches) if matches.len() == 1 => {
                matches.into_iter().next().map(|(_, entry)| entry.key)
            }
            Ok(matches) if matches.len() > 1 => {
                let keys = matches
                    .iter()
                    .map(|(_, entry)| entry.key.chars().take(8).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("Multiple memories match; use a full key or prefix: {keys}");
                None
            }
            Ok(_) => None,
            Err(error) => {
                println!("Failed to search memory: {error}");
                None
            }
        }
    };
    if let Some(key) = key {
        let layer = layer_manager.locate(&key).await.map(|(layer, _)| layer);
        match layer_manager.delete_memory(&key).await {
            Ok(true) => {
                if layer == Some(echo_agent::evolution::MemoryLayer::Hot) {
                    let root = ctx.agent.read(|agent| agent.working_dir()).await;
                    ctx.agent
                        .write_async(|agent| {
                            Box::pin(async move {
                                echo_agent_app_core::unified_memory::refresh_hot_memory_projection(
                                    agent,
                                    root.as_deref(),
                                )
                                .await;
                            })
                        })
                        .await;
                }
                println!("Removed memory: {key}");
            }
            Ok(false) => println!("No matching memory found."),
            Err(error) => println!("Failed to remove memory: {error}"),
        }
    } else {
        println!("No unambiguous matching memory found.");
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

            // User-level: ~/.eko/user.md
            let user_path = echo_agent::paths::user_data_path("user.md");
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
            let char_count = content.chars().count();
            let preview = if char_count > 500 {
                format!("{}...", content.chars().take(500).collect::<String>())
            } else {
                content.clone()
            };
            for line in preview.lines() {
                println!("    {}", line);
            }
            if char_count > 500 {
                println!("    ({char_count} chars total, truncated)");
            }
            println!();
        }
        _ => {
            println!("  ── {} ── (empty)", label);
            println!();
        }
    }
}

async fn cmd_attach(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let value = args.join(" ");
    if value.trim().is_empty() {
        let staged = ctx.staged_attachments.lock().await;
        if staged.is_empty() {
            println!("No attachments staged. Usage: /attach <path>");
        } else {
            println!("Staged attachments:");
            for attachment in staged.iter() {
                println!("  {} ({})", attachment.name, attachment.mime_type);
            }
        }
        return CommandOutcome::Continue;
    }
    if value.trim().eq_ignore_ascii_case("clear") {
        let attachments = {
            let mut staged = ctx.staged_attachments.lock().await;
            std::mem::take(&mut *staged)
        };
        match echo_agent_app_core::attachments::discard_staged_attachment_refs(&attachments) {
            Ok(()) => println!("Cleared staged attachments."),
            Err(error) => println!("Cleared attachment refs, but staging cleanup failed: {error}"),
        }
        return CommandOutcome::Continue;
    }
    let expanded = shellexpand::tilde(value.trim()).into_owned();
    let path = std::path::PathBuf::from(expanded);
    let workspace_root = find_project_root();
    match echo_agent_app_core::attachments::stage_local_attachment(&path, workspace_root.as_deref())
    {
        Ok(attachment) => {
            let name = attachment.name.clone();
            let mime = attachment.mime_type.clone();
            ctx.staged_attachments.lock().await.push(attachment);
            println!("Staged attachment: {name} ({mime})");
        }
        Err(error) => println!("Failed to stage attachment: {error}"),
    }
    CommandOutcome::Continue
}
cmd!(
    AttachCommand,
    "attach",
    CommandCategory::Context,
    "Stage a file for the next chat turn",
    cmd_attach
);

async fn cmd_interaction_mode(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    use echo_agent_app_core::tasks::task_runtime::InteractionMode;

    let Some(value) = args.first() else {
        let current = ctx.interaction_mode.read().await;
        println!(
            "Current interaction mode: {}. Usage: /mode chat|task|auto",
            current.as_str()
        );
        return CommandOutcome::Continue;
    };
    let next = match value.to_ascii_lowercase().as_str() {
        "chat" => InteractionMode::Chat,
        "task" => InteractionMode::Task,
        "auto" => InteractionMode::Auto,
        _ => {
            println!("Usage: /mode chat|task|auto");
            return CommandOutcome::Continue;
        }
    };
    *ctx.interaction_mode.write().await = next;
    println!("Interaction mode set to {}.", next.as_str());
    CommandOutcome::Continue
}
cmd!(
    InteractionModeCommand,
    "mode",
    CommandCategory::Context,
    "Set Chat, Task, or Auto interaction mode",
    cmd_interaction_mode
);

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

    let Some(integration) = ctx.review_integration.as_ref() else {
        println!("⚠️  Review integration is not configured.");
        return CommandOutcome::Continue;
    };
    let memory_generation = match integration.lease_generation() {
        Ok(generation) => generation,
        Err(error) => {
            println!("⚠️  Reflection unavailable during workspace transition: {error}");
            return CommandOutcome::Continue;
        }
    };

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

    // Write to the `.eko` root pinned before the LLM call.
    let memory_dir = memory_generation.echo_agent_dir().join("memory");
    if let Err(error) = std::fs::create_dir_all(&memory_dir) {
        println!("⚠️  Failed to create reflection memory directory: {error}");
        return CommandOutcome::Continue;
    }
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
            match file.write_all(entry.as_bytes()) {
                Ok(()) => println!("✅ Reflection saved to {}", memory_file.display()),
                Err(error) => println!("⚠️  Failed to write reflection: {error}"),
            }
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
        AutoMemoryConfig, extract_observations, format_observations_for_memory, queue_observations,
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
            let Some(integration) = ctx.review_integration.as_ref() else {
                println!("Review integration is not configured.");
                return CommandOutcome::Continue;
            };
            let evidence_lease = match integration.lease_generation() {
                Ok(lease) => lease,
                Err(error) => {
                    println!(
                        "Cannot queue memory candidates while the workspace is switching: {error}"
                    );
                    return CommandOutcome::Continue;
                }
            };
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

            let store = evidence_lease.evidence_store();
            match queue_observations(&store, &observations, &messages) {
                Ok(candidates) => println!(
                    "Extracted {} observation(s); {} candidate(s) are in Review Inbox.",
                    count,
                    candidates.len()
                ),
                Err(e) => println!("Auto-memory candidate creation failed: {e}"),
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
            println!("  extract — extract observations into Review Inbox");
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
    "Extract observations into the Review Inbox",
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
    registry.register(Arc::new(AttachCommand));
    registry.register(Arc::new(InteractionModeCommand));
    registry.register(Arc::new(ReflectCommand));
    registry.register(Arc::new(AutoMemoryCommand));
}
