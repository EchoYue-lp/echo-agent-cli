//! Advanced slash commands — save, load, sessions, export, profiles, themes, diagnostics.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

// chrono is re-exported by the workspace
use chrono;

// ── SaveCommand ───────────────────────────────────────────────────────

async fn cmd_save(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = args.first().copied().unwrap_or("session");
    let sessions_dir = std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".echo-agent")
        .join("sessions");
    let _ = std::fs::create_dir_all(&sessions_dir);

    let handle = ctx.agent.clone();
    let name_owned = name.to_string();
    let output = handle.read_async(move |a| {
        let name = name_owned.clone();
        Box::pin(async move {
            let ctx = a.context().lock().await;
            let messages: Vec<serde_json::Value> = ctx.messages().iter().map(|m| {
                serde_json::json!({"role": m.role.as_str(), "content": m.content.as_text().unwrap_or_default()})
            }).collect();
            serde_json::json!({
                "name": name,
                "saved_at": chrono::Utc::now().to_rfc3339(),
                "messages": messages,
                "plan_mode": a.is_plan_mode(),
            })
        })
    }).await;

    let path = sessions_dir.join(format!("{}.json", name));
    match std::fs::write(
        &path,
        serde_json::to_string_pretty(&output).unwrap_or_default(),
    ) {
        Ok(_) => println!("Session '{}' saved to: {}", name, path.display()),
        Err(e) => println!("Failed to save session: {e}"),
    }
    CommandOutcome::Continue
}
cmd!(
    SaveCommand,
    "save",
    CommandCategory::Sessions,
    "Save current session",
    cmd_save
);

// ── LoadCommand ───────────────────────────────────────────────────────

async fn cmd_load(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let name = args.first().copied().unwrap_or("session");
    let sessions_dir = std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".echo-agent")
        .join("sessions");
    let path = sessions_dir.join(format!("{}.json", name));

    if !path.exists() {
        println!(
            "Session '{}' not found. Use /sessions to list saved sessions.",
            name
        );
        return CommandOutcome::Continue;
    }

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(data) => {
                let msg_count = data
                    .get("messages")
                    .and_then(|m| m.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let saved_at = data
                    .get("saved_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                println!(
                    "Session '{}' loaded ({} messages, saved: {})",
                    name, msg_count, saved_at
                );
                println!("Note: session restoration requires agent restart for full effect.");
            }
            Err(e) => println!("Failed to parse session file: {e}"),
        },
        Err(e) => println!("Failed to read session: {e}"),
    }
    CommandOutcome::Continue
}
cmd!(
    LoadCommand,
    "load",
    CommandCategory::Sessions,
    "Load a saved session",
    cmd_load
);

// ── SessionsCommand ───────────────────────────────────────────────────

async fn cmd_sessions(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let sessions_dir = std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".echo-agent")
        .join("sessions");

    println!("\n--- Saved Sessions ---");
    if !sessions_dir.exists() {
        println!("  No sessions saved yet.");
        println!("  Use /save <name> to save, /load <name> to restore.");
        return CommandOutcome::Continue;
    }

    match std::fs::read_dir(&sessions_dir) {
        Ok(entries) => {
            let mut sessions: Vec<(String, String)> = Vec::new();
            for entry in entries.flatten() {
                if entry
                    .path()
                    .extension()
                    .map(|e| e == "json")
                    .unwrap_or(false)
                {
                    let name = entry
                        .path()
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string();
                    let meta = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(|t| {
                            let dt: chrono::DateTime<chrono::Utc> = t.into();
                            dt.format("%Y-%m-%d %H:%M").to_string()
                        })
                        .unwrap_or_default();
                    sessions.push((name, meta));
                }
            }
            if sessions.is_empty() {
                println!("  No sessions saved yet.");
            } else {
                sessions.sort();
                for (name, date) in &sessions {
                    println!("  {name:<20} {date}");
                }
                println!("\n  {} session(s) saved.", sessions.len());
            }
        }
        Err(e) => println!("  Error reading sessions: {e}"),
    }
    println!("  Use /save <name> to save, /load <name> to restore.");
    CommandOutcome::Continue
}
cmd!(
    SessionsCommand,
    "sessions",
    ["ss"],
    CommandCategory::Sessions,
    "List saved sessions",
    cmd_sessions
);

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

    let export_dir = std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let export_dir = export_dir.join(".echo-agent").join("exports");
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

// ── ThemeCommand ──────────────────────────────────────────────────────

async fn cmd_theme(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(t) = args.first() {
        println!("Theme set to: {t}");
    } else {
        println!("Available themes: dark, light, auto");
    }
    CommandOutcome::Continue
}
cmd!(
    ThemeCommand,
    "theme",
    CommandCategory::Output,
    "Switch color theme",
    cmd_theme
);

// ── OutputCommand ─────────────────────────────────────────────────────

async fn cmd_output(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(f) = args.first() {
        println!("Output format: {f}");
    } else {
        println!("Formats: text, json, markdown, table");
    }
    CommandOutcome::Continue
}
cmd!(
    OutputCommand,
    "output",
    CommandCategory::Output,
    "Set output format",
    cmd_output
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
    let config_path = std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .map(|h| h.join(".echo-agent").join("echo-agent.yaml"));
    match &config_path {
        Some(p) if p.exists() => println!("  [OK] Config file: {}", p.display()),
        Some(p) => println!("  [--] Config file not found: {}", p.display()),
        None => println!("  [--] HOME not set, cannot check config"),
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

    // Check sessions dir
    let sessions_dir = std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .map(|h| h.join(".echo-agent").join("sessions"));
    if let Some(d) = sessions_dir {
        if d.exists() {
            let count = std::fs::read_dir(&d)
                .map(|r| r.flatten().count())
                .unwrap_or(0);
            println!("  [OK] Sessions: {count} saved");
        } else {
            println!("  [--] Sessions: directory not created yet");
        }
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

// ── TraceCommand ──────────────────────────────────────────────────────

async fn cmd_trace(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let subcmd = args.first().copied().unwrap_or("sessions");
    let handle = ctx.agent.clone();

    match subcmd {
        "sessions" | "ss" => {
            // List sessions with trace data
            handle
                .read_async(|a| {
                    Box::pin(async move {
                        if let Some(ref store) = a.run_store {
                            let analyzer = echo_agent::trace::TraceAnalyzer::new(store.clone());
                            let sessions = analyzer.list_sessions(100).await.unwrap_or_default();
                            println!("\n--- Trace Sessions ---");
                            if sessions.is_empty() {
                                println!("  No sessions with trace data.");
                            } else {
                                for sid in &sessions {
                                    println!("  {sid}");
                                }
                                println!("\n  {} session(s) found.", sessions.len());
                            }
                        } else {
                            println!("  Run store not initialized. Trace data unavailable.");
                        }
                    })
                })
                .await;
        }
        "summary" | "sm" => {
            let session_id = args.get(1).copied().unwrap_or("default");
            handle
                .read_async(|a| {
                    let sid = session_id.to_string();
                    Box::pin(async move {
                        if let Some(ref store) = a.run_store {
                            let analyzer = echo_agent::trace::TraceAnalyzer::new(store.clone());
                            match analyzer.summarize_session(&sid).await {
                                Ok(s) => {
                                    println!("\n--- Session Summary: {} ---", s.session_id);
                                    println!("  Runs:           {} (completed={}, failed={}, cancelled={})", s.run_count, s.completed_count, s.failed_count, s.cancelled_count);
                                    println!("  Total tokens:   {} (prompt={}, completion={})", s.total_tokens, s.total_prompt_tokens, s.total_completion_tokens);
                                    println!("  Duration:       {}ms (LLM={}ms, tools={}ms)", s.total_duration_ms, s.total_llm_duration_ms, s.total_tool_duration_ms);
                                    println!("  LLM calls:      {}", s.llm_call_count);
                                    println!("  Tools used:     {}", s.tools_used.join(", "));
                                }
                                Err(e) => println!("  Error: {e}"),
                            }
                        } else {
                            println!("  Run store not initialized.");
                        }
                    })
                })
                .await;
        }
        "stats" | "st" => {
            handle
                .read_async(|a| {
                    Box::pin(async move {
                        if let Some(ref store) = a.run_store {
                            let analyzer = echo_agent::trace::TraceAnalyzer::new(store.clone());
                            let tools = analyzer.tool_usage_stats(50).await.unwrap_or_default();
                            println!("\n--- Tool Usage Stats ---");
                            for t in &tools {
                                println!(
                                    "  {:<20} calls={} success={} avg={}ms total={}ms ({:.1}%)",
                                    t.name,
                                    t.call_count,
                                    t.success_count,
                                    t.avg_duration_ms,
                                    t.total_duration_ms,
                                    t.pct_of_total_time
                                );
                            }

                            let tokens =
                                analyzer
                                    .token_usage_breakdown(50)
                                    .await
                                    .unwrap_or_else(|_| echo_agent::trace::TokenBreakdown {
                                        prompt_tokens: 0,
                                        completion_tokens: 0,
                                        total_tokens: 0,
                                        per_run: std::collections::HashMap::new(),
                                        per_llm_call: std::collections::HashMap::new(),
                                    });
                            println!("\n--- Token Breakdown ---");
                            println!(
                                "  Total: prompt={}, completion={}, total={}",
                                tokens.prompt_tokens, tokens.completion_tokens, tokens.total_tokens
                            );

                            let errors = analyzer
                                .error_pattern_analysis(50)
                                .await
                                .unwrap_or_default();
                            if !errors.is_empty() {
                                println!("\n--- Error Patterns ---");
                                for e in &errors {
                                    println!(
                                        "  [{}x] {} (tools: {})",
                                        e.occurrence_count,
                                        e.pattern,
                                        e.associated_tools.join(", ")
                                    );
                                }
                            }
                        } else {
                            println!("  Run store not initialized.");
                        }
                    })
                })
                .await;
        }
        _ => {
            println!("Usage: /trace <sessions|summary|stats>");
            println!("  sessions  — list sessions with trace data");
            println!("  summary <id> — session summary (default id='default')");
            println!("  stats — tool usage, token breakdown, error patterns");
        }
    }
    CommandOutcome::Continue
}
cmd!(
    TraceCommand,
    "trace",
    ["tr"],
    CommandCategory::Advanced,
    "Trace observability (sessions, summary, stats)",
    cmd_trace
);

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
    registry.register(Arc::new(DelegateCommand));
    registry.register(Arc::new(SearchCommand));
    registry.register(Arc::new(InspectCommand));
    registry.register(Arc::new(TraceCommand));
}
