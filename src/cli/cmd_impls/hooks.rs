//! Hooks management slash commands.
//!
//! `/hooks list` — list registered hook sources and rule counts
//! `/hooks reload` — reload hooks from config files (~/.eko/hooks.yaml, .eko/hooks.yaml)
//! `/hooks test <event> [matcher]` — dry-run matching without executing actions

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
                            println!("  Configure hooks in ~/.eko/hooks.yaml or echo-agent.yaml");
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
            // P0-1: 从磁盘重读**所有** user hook 来源(echo-agent.yaml 内嵌
            // + ~/.eko/hooks.yaml + .eko/hooks.yaml),合并成单个 definition
            // 后一次性 register。旧实现只 reload 文件 hooks、不重读
            // echo-agent.yaml,导致 reload 后内嵌 hooks 永久丢失。
            let load_result =
                echo_agent_app_core::hook_config_loader::HookConfigLoader::load_merged_from_disk();
            if !load_result.errors.is_empty() {
                println!("Hook reload aborted; existing hooks are unchanged:");
                for error in &load_result.errors {
                    println!("  - {error}");
                }
                return CommandOutcome::Continue;
            }
            let rule_count: usize = load_result.definition.rules.values().map(Vec::len).sum();
            let is_empty = load_result.definition.is_empty();
            let hooks_def = load_result.definition;
            ctx.agent
                .write_async(|a| {
                    Box::pin(async move {
                        let mut registry = a.hook_registry().write().await;
                        registry.clear_user_hooks();
                        if !hooks_def.is_empty() {
                            registry.register_user_hooks(hooks_def);
                        }
                    })
                })
                .await;
            if is_empty {
                println!("No hooks found in config sources.");
                println!("  Checked: echo-agent.yaml, ~/.eko/hooks.yaml, .eko/hooks.yaml");
            } else {
                println!("Loaded {} rules from:", rule_count);
                println!("  - echo-agent.yaml (inline)");
                for path in &load_result.loaded_from {
                    println!("  - {}", path.display());
                }
                println!("Hooks registered successfully.");
            }
        }
        "test" => {
            let event_name = args.get(1).copied().unwrap_or("").to_string();
            let matcher = args.get(2).copied().unwrap_or("*").to_string();
            if event_name.is_empty() {
                println!("Usage: /hooks test <event> [matcher]");
                println!("  Events: PreToolUse, PostToolUse, SessionStart, SessionEnd, Stop, ...");
                return CommandOutcome::Continue;
            }

            let event = echo_agent::skills::hooks::HookEvent::from_name(&event_name);

            match event {
                Some(evt) => {
                    ctx.agent
                        .read_async(|a| {
                            Box::pin(async move {
                                let registry = a.hook_registry().read().await;
                                let context = echo_agent::skills::hooks::HookContext::for_dry_run(
                                    evt, &matcher,
                                );
                                let result = registry.dry_run(&context);
                                println!("Dry-run {} matcher '{}':", event_name, matcher);
                                if result.matches.is_empty() {
                                    println!("  no matching actions");
                                } else {
                                    for item in result.matches {
                                        println!(
                                            "  {} · matcher={} · action={}",
                                            item.source, item.matcher, item.action
                                        );
                                    }
                                }
                            })
                        })
                        .await;
                }
                None => {
                    println!("Unknown event: '{}'", event_name);
                    println!("  Use the canonical PascalCase hook event name.");
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
