//! Skills & MCP management slash commands.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

pub async fn execute_skill_update_command(
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    args: &[&str],
) -> Option<String> {
    let subcommand = args.first().copied()?;
    match subcommand {
        "check-updates" | "check" => {
            let target = args.get(1).copied().or(Some("all"));
            let hub = crate::skills_hub::SkillsHub::new();
            Some(match crate::skills_hub::check_updates(&hub, target).await {
                Ok(statuses) if statuses.is_empty() => "No skills found.".to_string(),
                Ok(statuses) => statuses
                    .into_iter()
                    .map(|status| {
                        format!("[{:?}] {} - {}", status.state, status.name, status.message)
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(error) => format!("Skill update check failed: {error}"),
            })
        }
        "sync" => {
            let force = args.contains(&"--force");
            let target = args
                .iter()
                .skip(1)
                .copied()
                .find(|value| *value != "--force")
                .or(Some("all"));
            let mut hub = crate::skills_hub::SkillsHub::new();
            Some(
                match crate::skills_hub::sync_skills(&mut hub, target, force).await {
                    Ok(results) => {
                        let summary = if results.is_empty() {
                            "No skills found.".to_string()
                        } else {
                            results
                                .iter()
                                .map(|result| {
                                    format!(
                                        "[{}] {} - {}",
                                        if !result.success {
                                            "failed"
                                        } else if result.updated {
                                            "updated"
                                        } else {
                                            "unchanged"
                                        },
                                        result.name,
                                        result.message
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        };
                        let root = hub.root().to_path_buf();
                        match agent
                            .write_async(|value| {
                                Box::pin(async move { value.load_skills_from_dir(root).await })
                            })
                            .await
                        {
                            Ok(_) => summary,
                            Err(error) => {
                                format!(
                                    "{summary}\nSkills synced, but runtime reload failed: {error}"
                                )
                            }
                        }
                    }
                    Err(error) => format!("Skill sync failed: {error}"),
                },
            )
        }
        _ => None,
    }
}

// ── SkillsCommand ──────────────────────────────────────────────────────

async fn cmd_skills(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("");
    let rest = args.get(1..).map(|s| s.join(" ")).unwrap_or_default();
    let hub = crate::skills_hub::SkillsHub::new();

    match sub {
        "list" | "ls" | "" => {
            ctx.agent
                .read_async(|a| {
                    Box::pin(async move {
                        let names = a.skill_names();
                        println!("\n--- Loaded Skills ({}) ---", names.len());
                        for name in &names {
                            println!("  * {name}");
                        }
                        if names.is_empty() {
                            println!("  No skills loaded. Use /skills refresh to scan.");
                        }

                        // Also show Skills Hub entries
                        let hub = crate::skills_hub::SkillsHub::new();
                        let hub_entries = hub.list();
                        let unloaded: Vec<_> = hub_entries
                            .iter()
                            .filter(|e| !names.iter().any(|n| n == &e.name))
                            .collect();
                        if !unloaded.is_empty() {
                            println!("\n--- Hub Available ({} unloaded) ---", unloaded.len());
                            for e in unloaded {
                                let desc = if e.description.is_empty() {
                                    String::new()
                                } else {
                                    format!(" — {}", e.description)
                                };
                                println!("  o {}{}", e.name, desc);
                            }
                        }
                    })
                })
                .await;
        }
        "search" | "find" => {
            if rest.is_empty() {
                println!("Usage: /skills search <keyword>");
                return CommandOutcome::Continue;
            }
            let results = hub.search(&rest);
            println!(
                "\n--- Skill search: \"{}\" ({} results) ---",
                rest,
                results.len()
            );
            if results.is_empty() {
                println!("  No matches.");
            } else {
                for e in &results {
                    let status = if e.loaded { "[loaded]" } else { "[available]" };
                    println!("  {status} {} — {}", e.name, e.description);
                }
            }
        }
        "install" => {
            if rest.is_empty() {
                println!("Usage: /skills install <local-path|git-url>");
                return CommandOutcome::Continue;
            }
            let mut hub = hub;
            let result = if rest.starts_with("http://")
                || rest.starts_with("https://")
                || rest.ends_with(".git")
            {
                crate::skills_hub::install::install_from_git(&rest, None, &mut hub).await
            } else {
                let path = std::path::PathBuf::from(&rest);
                crate::skills_hub::install::install_from_local(&path, &mut hub)
            };
            match result {
                Ok(r) => println!("\n  Installed: {} (path: {})", r.name, r.path.display()),
                Err(e) => println!("\n  Install failed: {e}"),
            }
        }
        "uninstall" | "remove" | "rm" => {
            if rest.is_empty() {
                println!("Usage: /skills uninstall <name>");
                return CommandOutcome::Continue;
            }
            let mut hub = hub;
            match crate::skills_hub::install::uninstall(&rest, &mut hub) {
                Ok(()) => println!("Uninstalled: {rest}"),
                Err(e) => println!("Uninstall failed: {e}"),
            }
        }
        "info" => {
            let name = if rest.is_empty() {
                args.get(1).copied().unwrap_or("")
            } else {
                &rest
            };
            if name.is_empty() {
                println!("Usage: /skills info <name>");
                return CommandOutcome::Continue;
            }
            match hub.get(name) {
                Some(e) => {
                    println!("\n--- Skill: {} ---", e.name);
                    println!("  Description: {}", e.description);
                    println!("  Path:        {}", e.path.display());
                    if let Some(v) = &e.version {
                        println!("  Version:     {v}");
                    }
                    if let Some(a) = &e.author {
                        println!("  Author:      {a}");
                    }
                    println!(
                        "  Status:      {}",
                        if e.loaded { "loaded" } else { "not loaded" }
                    );
                }
                None => println!("Skill '{name}' not found in Hub."),
            }
        }
        "refresh" => {
            let mut hub = hub;
            hub.refresh();
            println!("Skills Hub refreshed ({} entries).", hub.list().len());
        }
        "check-updates" | "check" | "sync" => {
            if let Some(output) = execute_skill_update_command(&ctx.agent, args).await {
                println!("{output}");
            }
        }
        _ => {
            println!(
                "Usage: /skills [list|search|install|uninstall|info|refresh|check-updates|sync] [args]"
            );
        }
    }
    CommandOutcome::Continue
}
cmd!(
    SkillsCommand,
    "skills",
    ["sk", "skill"],
    CommandCategory::Info,
    "List and manage skills, including explicit upstream checks and sync",
    cmd_skills
);

// ── McpCommand ─────────────────────────────────────────────────────────

async fn cmd_mcp(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("");
    match sub {
        "list" | "ls" | "" => {
            ctx.agent
                .read_async(|a| {
                    Box::pin(async move {
                        let servers = a.mcp_server_names();
                        println!("\n--- MCP Servers ({}) ---", servers.len());
                        for name in &servers {
                            println!("  * {name}");
                        }
                        if servers.is_empty() {
                            println!("  No MCP servers connected.");
                        }
                    })
                })
                .await;
        }
        "connect" => {
            let name = args.get(1).copied().unwrap_or("");
            if name.is_empty() {
                println!("Usage: /mcp connect <name>");
            } else {
                println!("Connecting to MCP server: {name}");
            }
        }
        "disconnect" => {
            let name = args.get(1).copied().unwrap_or("");
            if name.is_empty() {
                println!("Usage: /mcp disconnect <name>");
            } else {
                println!("Disconnecting: {name}");
            }
        }
        _ => {
            println!("Usage: /mcp [list|connect|disconnect] [args]");
        }
    }
    CommandOutcome::Continue
}
cmd!(
    McpCommand,
    "mcp",
    ["m"],
    CommandCategory::Info,
    "Manage MCP server connections",
    cmd_mcp
);

// ── Register ───────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(SkillsCommand));
    registry.register(Arc::new(McpCommand));
}
