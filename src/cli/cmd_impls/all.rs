//! All remaining slash commands — help, exit, clear, remember, forget, memory.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

async fn current_memory_control(
    ctx: &CommandContext,
) -> Result<
    (
        crate::cli::command::ScopedReviewControl,
        Arc<echo_agent::evolution::MemoryLayerManager>,
    ),
    String,
> {
    let control = ctx.current_review_control().await?;
    let manager = control
        .generation
        .layer_manager()
        .map_err(|error| error.to_string())?;
    Ok((control, manager))
}

async fn current_control_messages(
    control: &crate::cli::command::ScopedReviewControl,
) -> Vec<(String, String)> {
    control
        .runtime
        .primary_agent()
        .read_async(|agent| {
            Box::pin(async move {
                let context = agent.context().lock().await;
                context
                    .messages()
                    .iter()
                    .map(|message| {
                        (
                            message.role.as_str().to_string(),
                            message.content.as_text().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
        })
        .await
}

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
    let (control, layer_manager) = match current_memory_control(ctx).await {
        Ok(control) => control,
        Err(error) => {
            println!("Cannot save memory: {error}");
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
        Ok(_) => {
            let projection = control.generation.settle_hot_memory_projection().await;
            println!("Memory saved with key: {key}");
            if let Some(error) = projection.error {
                println!("Memory projection remains pending: {error}");
            }
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
    let (control, layer_manager) = match current_memory_control(ctx).await {
        Ok(control) => control,
        Err(error) => {
            println!("Cannot remove memory: {error}");
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
        match layer_manager.delete_memory(&key).await {
            Ok(true) => {
                let projection = control.generation.settle_hot_memory_projection().await;
                println!("Removed memory: {key}");
                if let Some(error) = projection.error {
                    println!("Memory projection remains pending: {error}");
                }
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

async fn cmd_memory(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("show");
    let control = match ctx.current_review_control().await {
        Ok(control) => control,
        Err(error) => {
            println!("Memory unavailable: {error}");
            return CommandOutcome::Continue;
        }
    };

    match sub {
        "show" => {
            let user_path = echo_agent_app_core::api::data_root::user_data_path("user.md");
            let project_path = control
                .runtime
                .execution_scope()
                .root()
                .join(".eko")
                .join("project.md");
            if let Err(error) = tokio::task::spawn_blocking(move || {
                println!("\n📝 Project Memory\n");
                print_memory_tier("User", &user_path);
                print_memory_tier("Project", &project_path);
            })
            .await
            {
                println!("Failed to join memory read: {error}");
            }
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
        match echo_agent_app_core::api::attachments::discard_staged_attachment_refs(&attachments) {
            Ok(()) => println!("Cleared staged attachments."),
            Err(error) => println!("Cleared attachment refs, but staging cleanup failed: {error}"),
        }
        return CommandOutcome::Continue;
    }
    let expanded = shellexpand::tilde(value.trim()).into_owned();
    let path = std::path::PathBuf::from(expanded);
    let state = match ctx.app_state.as_ref() {
        Some(state) => state,
        None => {
            println!("Workspace attachment control is unavailable.");
            return CommandOutcome::Continue;
        }
    };
    let runtime = match state.current_control_runtime().await {
        Ok(runtime) => runtime,
        Err(error) => {
            println!("Cannot stage attachment: {error}");
            return CommandOutcome::Continue;
        }
    };
    let root = runtime.execution_scope().root().to_path_buf();
    let staged = tokio::task::spawn_blocking(move || {
        echo_agent_app_core::api::attachments::stage_local_attachment(&path, Some(&root))
    })
    .await;
    match staged {
        Ok(Ok(attachment)) => {
            let name = attachment.name.clone();
            let mime = attachment.mime_type.clone();
            ctx.staged_attachments.lock().await.push(attachment);
            println!("Staged attachment: {name} ({mime})");
        }
        Ok(Err(error)) => println!("Failed to stage attachment: {error}"),
        Err(error) => println!("Failed to join attachment staging: {error}"),
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

cmd!(
    MemoryCommand,
    "memory",
    CommandCategory::Context,
    "List memories",
    cmd_memory
);

// ── ReflectCommand ─────────────────────────────────────────────────────

fn render_reflection_receipt(
    receipt: &echo_agent_app_core::api::reflection::ReflectionReceipt,
) -> String {
    receipt.display_message()
}

fn validate_reflection_args(
    args: &[&str],
) -> Result<(), echo_agent_app_core::api::reflection::ReflectionCommandParseError> {
    let input = if args.is_empty() {
        "/reflect".to_string()
    } else {
        format!("/reflect {}", args.join(" "))
    };
    echo_agent_app_core::api::reflection::ReflectionCommand::parse(&input).map(|_| ())
}

async fn cmd_reflect(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Err(error) = validate_reflection_args(args) {
        println!("{error}");
        return CommandOutcome::Continue;
    }
    let Some(state) = ctx.app_state.as_ref() else {
        println!("Reflection unavailable: application state is unavailable");
        return CommandOutcome::Continue;
    };
    let runtime = match state.current_control_runtime().await {
        Ok(runtime) => runtime,
        Err(error) => {
            println!("Reflection unavailable: {error}");
            return CommandOutcome::Continue;
        }
    };
    match echo_agent_app_core::api::reflection::reflect_session(
        &runtime,
        &ctx.agent,
        ctx.conversation_id.as_deref(),
    )
    .await
    {
        Ok(receipt) => println!("{}", render_reflection_receipt(&receipt)),
        Err(error) => println!("Reflection failed: {error}"),
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
    use echo_agent_app_core::api::auto_memory::{
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
            let control = match ctx.current_review_control().await {
                Ok(control) => control,
                Err(error) => {
                    println!("Cannot queue memory candidates: {error}");
                    return CommandOutcome::Continue;
                }
            };
            let messages = current_control_messages(&control).await;

            let config = AutoMemoryConfig::default();
            let observations = extract_observations(&messages, &config);
            let count = observations.len();
            if count == 0 {
                println!("No observations extracted from this session.");
                return CommandOutcome::Continue;
            }

            let store = control.generation.evidence_store();
            match queue_observations(&store, &observations, &messages) {
                Ok(candidates) => {
                    let projection = if candidates.is_empty() {
                        None
                    } else {
                        Some(control.generation.settle_hot_memory_projection().await)
                    };
                    println!(
                        "Extracted {} observation(s); {} candidate(s) are in Review Inbox.",
                        count,
                        candidates.len()
                    );
                    if let Some(error) = projection.and_then(|receipt| receipt.error) {
                        println!("Memory projection remains pending: {error}");
                    }
                }
                Err(e) => println!("Auto-memory candidate creation failed: {e}"),
            }
        }
        "show" => {
            let control = match ctx.current_review_control().await {
                Ok(control) => control,
                Err(error) => {
                    println!("Auto-memory preview unavailable: {error}");
                    return CommandOutcome::Continue;
                }
            };
            let messages = current_control_messages(&control).await;

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
    registry.register(Arc::new(ReflectCommand));
    registry.register(Arc::new(AutoMemoryCommand));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_reflection_adapter_projects_shared_receipt() {
        assert!(validate_reflection_args(&[]).is_ok());
        assert!(validate_reflection_args(&["extra"]).is_err());
        let receipt = echo_agent_app_core::api::reflection::reflection_receipt_fixture();
        let rendered = render_reflection_receipt(&receipt);
        assert!(rendered.contains(&receipt.key));
        assert!(rendered.contains(&receipt.content_summary));
    }
}
