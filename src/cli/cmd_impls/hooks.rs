//! Hooks management slash commands.
//!
//! `/hooks list` — list registered hook sources and rule counts
//! `/hooks reload` — reload hooks from config files (~/.echo-agent/hooks.yaml, .echo-agent/hooks.yaml)
//! `/hooks test <event>` — test if hooks are registered for a given event

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

async fn cmd_hooks(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("list");

    match sub {
        "list" | "ls" | "" => {
            ctx.agent
                .read_async(|a| {
                    Box::pin(async move {
                        let registry = a.hook_registry().read().await;
                        let sources = registry.list_sources();
                        println!("\n--- Registered Hooks ({}) ---", sources.len());
                        if sources.is_empty() {
                            println!("  No hooks registered.");
                            println!(
                                "  Configure hooks in ~/.echo-agent/hooks.yaml or echo-agent.yaml"
                            );
                        } else {
                            for (name, count) in &sources {
                                println!("  * {} ({} rules)", name, count);
                            }
                        }
                    })
                })
                .await;
        }
        "reload" => {
            // Load hooks from YAML files
            let load_result = echo_agent_app_core::hooks_config::load_hooks_files();
            if load_result.definition.is_empty() {
                println!("No hooks found in config files.");
                println!("  Searched: ~/.echo-agent/hooks.yaml, .echo-agent/hooks.yaml");
            } else {
                let rule_count: usize =
                    load_result.definition.rules.values().map(|v| v.len()).sum();
                println!("Loaded {} rules from:", rule_count);
                for path in &load_result.loaded_from {
                    println!("  - {}", path.display());
                }

                // Register into agent's hook registry
                let hooks_def = load_result.definition;
                ctx.agent
                    .write_async(|a| {
                        Box::pin(async move {
                            let mut registry = a.hook_registry().write().await;
                            registry.clear_user_hooks();
                            registry.register_user_hooks(hooks_def);
                        })
                    })
                    .await;
                println!("Hooks registered successfully.");
            }
        }
        "test" => {
            let event_name = args.get(1).copied().unwrap_or("").to_string();
            if event_name.is_empty() {
                println!("Usage: /hooks test <event>");
                println!("  Events: PreToolUse, PostToolUse, SessionStart, SessionEnd, Stop, ...");
                return CommandOutcome::Continue;
            }

            // Try to parse the event name
            let event = match event_name.as_str() {
                "PreToolUse" => Some(echo_agent::skills::hooks::HookEvent::PreToolUse),
                "PostToolUse" => Some(echo_agent::skills::hooks::HookEvent::PostToolUse),
                "PostToolUseFailure" => {
                    Some(echo_agent::skills::hooks::HookEvent::PostToolUseFailure)
                }
                "SessionStart" => Some(echo_agent::skills::hooks::HookEvent::SessionStart),
                "SessionEnd" => Some(echo_agent::skills::hooks::HookEvent::SessionEnd),
                "Stop" => Some(echo_agent::skills::hooks::HookEvent::Stop),
                "UserPromptSubmit" => Some(echo_agent::skills::hooks::HookEvent::UserPromptSubmit),
                "ConfigChange" => Some(echo_agent::skills::hooks::HookEvent::ConfigChange),
                _ => None,
            };

            match event {
                Some(evt) => {
                    ctx.agent
                        .read_async(|a| {
                            Box::pin(async move {
                                let registry = a.hook_registry().read().await;
                                let has = registry.has_hooks_for(evt);
                                println!(
                                    "Hooks for {}: {}",
                                    event_name,
                                    if has {
                                        "YES (hooks registered)"
                                    } else {
                                        "NO (no hooks)"
                                    }
                                );
                            })
                        })
                        .await;
                }
                None => {
                    println!("Unknown event: '{}'", event_name);
                    println!(
                        "  Valid events: PreToolUse, PostToolUse, PostToolUseFailure, SessionStart, SessionEnd, Stop, UserPromptSubmit, ConfigChange"
                    );
                }
            }
        }
        _ => {
            println!("Usage: /hooks [list|reload|test <event>]");
        }
    }

    CommandOutcome::Continue
}

cmd!(
    HooksCommand,
    "hooks",
    ["hk"],
    CommandCategory::Config,
    "Manage hooks (list/reload/test)",
    cmd_hooks
);

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(HooksCommand));
}
