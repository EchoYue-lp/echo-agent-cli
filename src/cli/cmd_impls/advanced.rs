//! Advanced slash commands — export, profiles, and diagnostics.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

// ── ExportCommand ─────────────────────────────────────────────────────

async fn cmd_export(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let fmt = args.first().copied().unwrap_or("json").to_string();
    let export_name = args.get(1).copied().unwrap_or("export").to_string();
    let handle = ctx.agent.clone();

    // Run export synchronously in the command handler (C3 fix: no fire-and-forget spawn).
    let fmt_clone = fmt.clone();
    let name_clone = export_name.clone();
    let output = handle.read_async(move |a| {
        let fmt = fmt_clone.clone();
        let name = name_clone.clone();
        Box::pin(async move {
            let ctx = a.context().lock().await;
            match fmt.as_str() {
                "json" => serde_json::to_string_pretty(&ctx.messages().iter().map(|m| {
                    serde_json::json!({"role": m.role.as_str(), "content": m.content.as_text().unwrap_or_default()})
                }).collect::<Vec<_>>()).unwrap_or_default(),
                "markdown" | "md" => {
                    let mut md = format!("# Session Export: {}\n\n", name);
                    for msg in ctx.messages() {
                        md.push_str(&format!("### {}\n\n", msg.role.as_str()));
                        if let Some(text) = msg.content.as_text_ref() {
                            md.push_str(text);
                            md.push_str("\n\n---\n\n");
                        }
                    }
                    md
                }
                _ => format!("Unknown format: {fmt}. Use 'json' or 'markdown'."),
            }
        })
    }).await;

    let export_dir = echo_agent_app_core::data_root::user_data_path("exports");
    let _ = std::fs::create_dir_all(&export_dir);

    let ext = if fmt == "markdown" || fmt == "md" {
        "md"
    } else {
        "json"
    };
    let path = export_dir.join(format!("{}.{}", export_name, ext));
    match std::fs::write(&path, &output) {
        Ok(_) => println!("Exported to: {}", path.display()),
        Err(e) => println!("Export failed: {e}"),
    }
    CommandOutcome::Continue
}
cmd!(
    ExportCommand,
    "export",
    CommandCategory::Sessions,
    "Export session to file",
    cmd_export
);

// ── ProfileCommand ────────────────────────────────────────────────────

async fn cmd_profile(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\n--- Profiles ---");
    println!("  Use /profile create|use|delete|show <name>");
    CommandOutcome::Continue
}
cmd!(
    ProfileCommand,
    "profile",
    ["prof"],
    CommandCategory::Profiles,
    "Manage configuration profiles",
    cmd_profile
);

// ── VerboseCommand ────────────────────────────────────────────────────

async fn cmd_verbose(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("Verbose mode toggled.");
    CommandOutcome::Continue
}
cmd!(
    VerboseCommand,
    "verbose",
    CommandCategory::Output,
    "Toggle verbose mode",
    cmd_verbose
);

// ── CronCommand ───────────────────────────────────────────────────────

async fn cmd_cron(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\n--- Scheduled Tasks ---");
    println!("  Use /cron add|remove|enable|disable|run");
    CommandOutcome::Continue
}
cmd!(
    CronCommand,
    "cron",
    CommandCategory::Advanced,
    "Manage scheduled tasks",
    cmd_cron
);

// ── DoctorCommand ─────────────────────────────────────────────────────

async fn cmd_doctor(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\n--- Configuration Diagnostics ---\n");

    // Check config
    let config_path = echo_agent_app_core::data_root::user_data_path("config.yaml");
    if config_path.exists() {
        println!("  [OK] Config file: {}", config_path.display());
    } else {
        println!("  [--] Config file not found: {}", config_path.display());
    }

    // Check git
    let git_ok = tokio::process::Command::new("git")
        .arg("--version")
        .output()
        .await;
    match git_ok {
        Ok(o) if o.status.success() => {
            let ver = String::from_utf8_lossy(&o.stdout).trim().to_string();
            println!("  [OK] Git: {ver}");
        }
        _ => println!("  [!!] Git not found"),
    }

    // Check agent context
    let handle = ctx.agent.clone();
    handle
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                let msg_count = ctx.messages().len();
                let tokens = ctx.token_estimate();
                println!("  [OK] Agent context: {msg_count} messages, ~{tokens} tokens");
                println!(
                    "  [OK] Plan mode: {}",
                    if a.is_plan_mode() { "ON" } else { "OFF" }
                );
            })
        })
        .await;

    // Check the same canonical conversation authority used by every surface.
    let conversation_store = match ctx.app_state.as_ref() {
        Some(app_state) => app_state.conversation_store().await,
        None => None,
    };
    match conversation_store {
        Some(store) => match store
            .list_conversations(echo_agent::memory::ConversationFilter {
                limit: Some(1),
                ..Default::default()
            })
            .await
        {
            Ok(_) => println!("  [OK] ConversationStore: accessible"),
            Err(error) => println!("  [!!] ConversationStore: {error}"),
        },
        None => println!("  [--] ConversationStore: unavailable"),
    }

    println!("\nDiagnostics complete.");
    CommandOutcome::Continue
}
cmd!(
    DoctorCommand,
    "doctor",
    ["doc"],
    CommandCategory::Advanced,
    "Run configuration diagnostics",
    cmd_doctor
);

// ── DelegateCommand ───────────────────────────────────────────────────

async fn cmd_delegate(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let task = args.join(" ");
    if task.is_empty() {
        println!("Usage: /delegate <task>");
    } else {
        println!("Delegating: {task}");
    }
    CommandOutcome::Continue
}
cmd!(
    DelegateCommand,
    "delegate",
    ["dl"],
    CommandCategory::Advanced,
    "Delegate task to sub-agent",
    cmd_delegate
);

// ── SearchCommand ─────────────────────────────────────────────────────

async fn cmd_search(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let query = args.join(" ");
    if query.is_empty() {
        println!("Usage: /search <query>");
        return CommandOutcome::Continue;
    }

    let query_lower = query.to_lowercase();
    let handle = ctx.agent.clone();
    handle
        .read_async(move |a| {
            let q = query_lower.clone();
            Box::pin(async move {
                let ctx = a.context().lock().await;
                let messages = ctx.messages();
                println!("\n--- Search: '{}' ({} messages) ---", q, messages.len());

                let mut found = 0;
                for (i, msg) in messages.iter().enumerate() {
                    if let Some(text) = msg.content.as_text_ref()
                        && text.to_lowercase().contains(&q)
                    {
                        let preview: String = text.chars().take(120).collect();
                        let role = msg.role.as_str();
                        println!("  [{i}] {role}: {preview}...");
                        found += 1;
                        if found >= 20 {
                            println!("  ... (showing first 20 matches)");
                            break;
                        }
                    }
                }
                if found == 0 {
                    println!("  No messages matching '{}'.", q);
                } else {
                    println!("\n  {found} match(es) found.");
                }
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    SearchCommand,
    "search",
    CommandCategory::Info,
    "Search session history",
    cmd_search
);

// ── InspectCommand ────────────────────────────────────────────────────

async fn cmd_inspect(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    ctx.agent
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                println!("\n--- Inspect ---");
                println!("  Messages: {}", ctx.messages().len());
                println!("  Tokens:   {}", ctx.token_estimate());
                println!("  Plan:     {}", a.is_plan_mode());
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    InspectCommand,
    "inspect",
    ["ins"],
    CommandCategory::Debug,
    "Detailed agent state",
    cmd_inspect
);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(ExportCommand));
    registry.register(Arc::new(ProfileCommand));
    registry.register(Arc::new(VerboseCommand));
    registry.register(Arc::new(CronCommand));
    registry.register(Arc::new(DoctorCommand));
    registry.register(Arc::new(DelegateCommand));
    registry.register(Arc::new(SearchCommand));
    registry.register(Arc::new(InspectCommand));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_json_session_commands_are_not_registered() {
        let mut registry = crate::cli::command::CommandRegistry::new();
        crate::cli::cmd_impls::session::register_all(&mut registry);
        crate::cli::cmd_impls::context::register_all(&mut registry);
        register_all(&mut registry);

        assert_eq!(
            registry.get("save").map(|command| command.name()),
            Some("checkpoint")
        );
        assert!(registry.get("load").is_none());
        assert_eq!(
            registry.get("sessions").map(|command| command.name()),
            Some("sessions")
        );
        assert!(registry.get("theme").is_none());
        assert!(registry.get("output").is_none());
    }
}
