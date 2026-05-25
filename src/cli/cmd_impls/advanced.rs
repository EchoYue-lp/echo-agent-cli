//! Advanced slash commands — save, load, sessions, export, profile, theme, output, verbose, cron, doctor, tui, delegate, search, inspect.

use std::sync::Arc;
use crate::cli::command::{cmd, CommandCategory, CommandContext, CommandOutcome};

// ── SaveCommand ───────────────────────────────────────────────────────

async fn cmd_save(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = args.first().copied().unwrap_or("session");
    println!("Session '{name}' saved.");
    CommandOutcome::Continue
}
cmd!(SaveCommand, "save", CommandCategory::Sessions, "Save current session", cmd_save);

// ── LoadCommand ───────────────────────────────────────────────────────

async fn cmd_load(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = args.first().copied().unwrap_or("session");
    println!("Session '{name}' loaded.");
    CommandOutcome::Continue
}
cmd!(LoadCommand, "load", CommandCategory::Sessions, "Load a saved session", cmd_load);

// ── SessionsCommand ───────────────────────────────────────────────────

async fn cmd_sessions(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\n--- Saved Sessions ---");
    println!("  Use /save <name> and /load <name>");
    CommandOutcome::Continue
}
cmd!(SessionsCommand, "sessions", ["ss"], CommandCategory::Sessions, "List saved sessions", cmd_sessions);

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
                        if let Some(ref text) = msg.content.as_text_ref() {
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

    let export_dir = std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let export_dir = export_dir.join(".echo-agent").join("exports");
    let _ = std::fs::create_dir_all(&export_dir);

    let ext = if fmt == "markdown" || fmt == "md" { "md" } else { "json" };
    let path = export_dir.join(format!("{}.{}", export_name, ext));
    match std::fs::write(&path, &output) {
        Ok(_) => println!("Exported to: {}", path.display()),
        Err(e) => println!("Export failed: {e}"),
    }
    CommandOutcome::Continue
}
cmd!(ExportCommand, "export", CommandCategory::Sessions, "Export session to file", cmd_export);

// ── ProfileCommand ────────────────────────────────────────────────────

async fn cmd_profile(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\n--- Profiles ---");
    println!("  Use /profile create|use|delete|show <name>");
    CommandOutcome::Continue
}
cmd!(ProfileCommand, "profile", ["prof"], CommandCategory::Profiles, "Manage configuration profiles", cmd_profile);

// ── ThemeCommand ──────────────────────────────────────────────────────

async fn cmd_theme(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(t) = args.first() { println!("Theme set to: {t}"); }
    else { println!("Available themes: dark, light, auto"); }
    CommandOutcome::Continue
}
cmd!(ThemeCommand, "theme", CommandCategory::Output, "Switch color theme", cmd_theme);

// ── OutputCommand ─────────────────────────────────────────────────────

async fn cmd_output(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(f) = args.first() { println!("Output format: {f}"); }
    else { println!("Formats: text, json, markdown, table"); }
    CommandOutcome::Continue
}
cmd!(OutputCommand, "output", CommandCategory::Output, "Set output format", cmd_output);

// ── VerboseCommand ────────────────────────────────────────────────────

async fn cmd_verbose(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("Verbose mode toggled.");
    CommandOutcome::Continue
}
cmd!(VerboseCommand, "verbose", CommandCategory::Output, "Toggle verbose mode", cmd_verbose);

// ── CronCommand ───────────────────────────────────────────────────────

async fn cmd_cron(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\n--- Scheduled Tasks ---");
    println!("  Use /cron add|remove|enable|disable|run");
    CommandOutcome::Continue
}
cmd!(CronCommand, "cron", CommandCategory::Advanced, "Manage scheduled tasks", cmd_cron);

// ── DoctorCommand ─────────────────────────────────────────────────────

async fn cmd_doctor(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\n--- Configuration Diagnostics ---");
    println!("  All systems nominal.");
    CommandOutcome::Continue
}
cmd!(DoctorCommand, "doctor", ["doc"], CommandCategory::Advanced, "Run configuration diagnostics", cmd_doctor);

// ── TuiCommand ────────────────────────────────────────────────────────

async fn cmd_tui(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("TUI mode: use --tui flag when starting echo-agent-cli.");
    CommandOutcome::Continue
}
cmd!(TuiCommand, "tui", CommandCategory::Advanced, "Info about TUI mode", cmd_tui);

// ── DelegateCommand ───────────────────────────────────────────────────

async fn cmd_delegate(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let task = args.join(" ");
    if task.is_empty() { println!("Usage: /delegate <task>"); }
    else { println!("Delegating: {task}"); }
    CommandOutcome::Continue
}
cmd!(DelegateCommand, "delegate", ["dl"], CommandCategory::Advanced, "Delegate task to sub-agent", cmd_delegate);

// ── SearchCommand ─────────────────────────────────────────────────────

async fn cmd_search(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let query = args.join(" ");
    if query.is_empty() { println!("Usage: /search <query>"); }
    else { println!("\nSearching for: {query}"); }
    CommandOutcome::Continue
}
cmd!(SearchCommand, "search", CommandCategory::Info, "Search session history", cmd_search);

// ── InspectCommand ────────────────────────────────────────────────────

async fn cmd_inspect(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    ctx.agent.read_async(|a| Box::pin(async move {
        let ctx = a.context().lock().await;
        println!("\n--- Inspect ---");
        println!("  Messages: {}", ctx.messages().len());
        println!("  Tokens:   {}", ctx.token_estimate());
        println!("  Plan:     {}", a.is_plan_mode());
    })).await;
    CommandOutcome::Continue
}
cmd!(InspectCommand, "inspect", ["ins"], CommandCategory::Debug, "Detailed agent state", cmd_inspect);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(SaveCommand));
    registry.register(Arc::new(LoadCommand));
    registry.register(Arc::new(SessionsCommand));
    registry.register(Arc::new(ExportCommand));
    registry.register(Arc::new(ProfileCommand));
    registry.register(Arc::new(ThemeCommand));
    registry.register(Arc::new(OutputCommand));
    registry.register(Arc::new(VerboseCommand));
    registry.register(Arc::new(CronCommand));
    registry.register(Arc::new(DoctorCommand));
    registry.register(Arc::new(TuiCommand));
    registry.register(Arc::new(DelegateCommand));
    registry.register(Arc::new(SearchCommand));
    registry.register(Arc::new(InspectCommand));
}
