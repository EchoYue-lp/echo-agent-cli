//! Context management slash commands — project, think, reasoning, model, system, compress, compact, context, refresh.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use std::sync::Arc;

fn parse_llm_protocol(value: &str) -> Option<echo_agent::llm::LlmApiProtocol> {
    match value.trim().to_ascii_lowercase().as_str() {
        "chat" | "chat_completions" | "chat-completions" => {
            Some(echo_agent::llm::LlmApiProtocol::ChatCompletions)
        }
        "responses" => Some(echo_agent::llm::LlmApiProtocol::Responses),
        "anthropic" | "messages" => Some(echo_agent::llm::LlmApiProtocol::Anthropic),
        _ => None,
    }
}

// ── ThinkCommand ──────────────────────────────────────────────────────

async fn cmd_think(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let available = if let Some(app_state) = ctx.app_state.as_ref() {
        let config = app_state.config.app_config.read().await;
        let runtime = echo_agent_app_core::model_config::resolve_runtime_model(&config, None);
        echo_agent_app_core::model_config::thinking_level_specs(runtime.thinking_profile)
    } else {
        Vec::new()
    };
    let Some(level) = args.first().map(|value| value.trim().to_ascii_lowercase()) else {
        let choices = if available.is_empty() {
            "auto".to_string()
        } else {
            format!("auto, {}", available.join(", "))
        };
        println!("Thinking levels for the active model: {choices}");
        return CommandOutcome::Continue;
    };
    if level != "auto" && !available.iter().any(|candidate| candidate == &level) {
        println!("Thinking level '{level}' is not available for the active model");
        return CommandOutcome::Continue;
    }
    let thinking = match echo_agent::llm::ThinkingConfig::parse_spec(&level) {
        Ok(thinking) => thinking,
        Err(error) => {
            println!("Invalid thinking level: {error}");
            return CommandOutcome::Continue;
        }
    };
    ctx.agent.write(|agent| agent.set_thinking(thinking)).await;
    println!("Thinking level: {level}");
    CommandOutcome::Continue
}
cmd!(
    ThinkCommand,
    "think",
    CommandCategory::Context,
    "Show or set the active model's thinking level",
    cmd_think
);

// ── ReasoningCommand ──────────────────────────────────────────────────

async fn cmd_reasoning(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    cmd_think(ctx, args).await
}
cmd!(
    ReasoningCommand,
    "reasoning",
    CommandCategory::Context,
    "Alias of /think",
    cmd_reasoning
);

// ── ModelCommand ──────────────────────────────────────────────────────

async fn cmd_model(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let Some(app_state) = ctx.app_state.as_ref() else {
        println!("Model configuration is unavailable in this runtime.");
        return CommandOutcome::Continue;
    };
    match args.first().copied().unwrap_or("list") {
        "list" => {
            let config = app_state.config.app_config.read().await;
            for model in echo_agent_app_core::model_config::configured_model_views(&config) {
                let active = if model.is_default { "*" } else { " " };
                println!(
                    "{active} {}  {}  {:?}  {:?}",
                    model.id, model.model, model.api_protocol, model.input_modalities
                );
            }
        }
        "use" => {
            let Some(selector) = args.get(1) else {
                println!("Usage: /model use <model-id|model-name>");
                return CommandOutcome::Continue;
            };
            match app_state.set_default_model_owned(*selector).await {
                Ok(receipt) => println!("Active model: {}", receipt.model_id),
                Err(error) => println!("{error}"),
            }
        }
        "delete" => {
            let Some(model_id) = args.get(1) else {
                println!("Usage: /model delete <model-id>");
                return CommandOutcome::Continue;
            };
            match app_state.delete_configured_model_owned(*model_id).await {
                Ok(_) => println!("Deleted model: {model_id}"),
                Err(error) => println!("{error}"),
            }
        }
        "test" => {
            let Some(selector) = args.get(1) else {
                println!("Usage: /model test <model-id|model-name>");
                return CommandOutcome::Continue;
            };
            let config = app_state.config.app_config.read().await.clone();
            let runtime = match echo_agent_app_core::model_config::resolve_runtime_model_selector(
                &config,
                Some(selector),
            ) {
                Ok(runtime) => runtime,
                Err(error) => {
                    println!("{error}");
                    return CommandOutcome::Continue;
                }
            };
            match echo_agent_app_core::infra::test_runtime_llm_connection(&runtime).await {
                Ok(result) => println!(
                    "Connection succeeded: {} ({})",
                    result.model, result.response
                ),
                Err(error) => println!("Connection failed: {error}"),
            }
        }
        "add" => {
            let (Some(provider), Some(model), Some(protocol)) = (
                args.get(1),
                args.get(2),
                args.get(3).and_then(|v| parse_llm_protocol(v)),
            ) else {
                println!(
                    "Usage: /model add <provider-id> <model> <chat|responses|anthropic> [image] [audio] [video] [default]"
                );
                return CommandOutcome::Continue;
            };
            let flags = args.get(4..).unwrap_or(&[]);
            let mut input_modalities = echo_agent::llm::ModelInputModality::text_only();
            if flags.contains(&"image") {
                input_modalities.push(echo_agent::llm::ModelInputModality::Image);
            }
            if flags.contains(&"audio") {
                input_modalities.push(echo_agent::llm::ModelInputModality::Audio);
            }
            if flags.contains(&"video") {
                input_modalities.push(echo_agent::llm::ModelInputModality::Video);
            }
            let mutation = echo_agent_app_core::state::ConfiguredModelMutation {
                model: echo_agent::config::ConfiguredModel {
                    provider: (*provider).to_string(),
                    model: (*model).to_string(),
                    api_protocol: protocol,
                    input_modalities,
                    ..Default::default()
                },
                set_default: flags.contains(&"default"),
            };
            match app_state.upsert_configured_model_owned(mutation).await {
                Ok(receipt) => println!("Saved model: {}", receipt.model_id),
                Err(error) => println!("{error}"),
            }
        }
        selector => match app_state.set_default_model_owned(selector).await {
            Ok(receipt) => println!("Active model: {}", receipt.model_id),
            Err(error) => println!("{error}"),
        },
    }
    CommandOutcome::Continue
}
cmd!(
    ModelCommand,
    "model",
    CommandCategory::Config,
    "List, add, test, select, or delete models",
    cmd_model
);

async fn cmd_provider(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let Some(app_state) = ctx.app_state.as_ref() else {
        println!("Provider configuration is unavailable in this runtime.");
        return CommandOutcome::Continue;
    };
    match args.first().copied().unwrap_or("list") {
        "list" => {
            let config = app_state.config.app_config.read().await;
            for provider in echo_agent_app_core::model_config::configured_provider_views(&config) {
                println!(
                    "{}  {}  {}  {:?}  {} models",
                    provider.id,
                    provider.name,
                    provider.base_url,
                    provider.default_api_protocol,
                    provider.model_count
                );
            }
        }
        "delete" => {
            let Some(provider_id) = args.get(1) else {
                println!("Usage: /provider delete <provider-id>");
                return CommandOutcome::Continue;
            };
            match app_state.delete_model_provider_owned(*provider_id).await {
                Ok(_) => println!("Deleted provider: {provider_id}"),
                Err(error) => println!("{error}"),
            }
        }
        "add" | "update" => {
            let (Some(id), Some(base_url), Some(protocol)) = (
                args.get(1),
                args.get(2),
                args.get(3).and_then(|v| parse_llm_protocol(v)),
            ) else {
                println!(
                    "Usage: /provider add <id> <base-url> <chat|responses|anthropic> [api-key-env] [requires-key]"
                );
                return CommandOutcome::Continue;
            };
            let api_key_env = args
                .get(4)
                .filter(|value| !value.trim().is_empty() && **value != "-")
                .map(|value| (*value).to_string());
            let requires_api_key = args.get(5).is_some_and(|value| *value == "requires-key");
            let mutation = echo_agent_app_core::state::ModelProviderMutation {
                id: (*id).to_string(),
                provider: echo_agent::config::ModelProviderConfig {
                    name: (*id).to_string(),
                    api_key_env,
                    base_url: Some((*base_url).to_string()),
                    default_api_protocol: Some(protocol),
                    requires_api_key,
                    ..Default::default()
                },
                preserve_auth_token: true,
            };
            match app_state.upsert_model_provider_owned(mutation).await {
                Ok(receipt) => println!("Saved provider: {}", receipt.model_id),
                Err(error) => println!("{error}"),
            }
        }
        _ => println!("Usage: /provider [list|add|update|delete]"),
    }
    CommandOutcome::Continue
}
cmd!(
    ProviderCommand,
    "provider",
    CommandCategory::Config,
    "List, add, update, or delete model providers",
    cmd_provider
);

// ── SystemCommand ─────────────────────────────────────────────────────

async fn cmd_system(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.is_empty() {
        ctx.agent
            .read_async(|a| {
                Box::pin(async move {
                    let ctx = a.context().lock().await;
                    if let Some(first) = ctx.messages().first() {
                        println!(
                            "\n--- System Prompt ---\n{}",
                            first.content.as_text().unwrap_or_default()
                        );
                    }
                })
            })
            .await;
    } else {
        let prompt = args.join(" ");
        ctx.agent
            .write_async(|a| Box::pin(async move { a.set_system_prompt(prompt).await }))
            .await;
        println!("System prompt updated.");
    }
    CommandOutcome::Continue
}
cmd!(
    SystemCommand,
    "system",
    ["sys"],
    CommandCategory::Config,
    "View or set system prompt",
    cmd_system
);

// ── CompressCommand ───────────────────────────────────────────────────

async fn run_manual_compression(
    ctx: &CommandContext,
    args: &[&str],
    keep_messages: usize,
    label: &str,
) -> CommandOutcome {
    let (Some(app_state), Some(conversation_id)) =
        (ctx.app_state.as_ref(), ctx.conversation_id.as_ref())
    else {
        println!("Manual compression requires an active persisted conversation.");
        return CommandOutcome::Continue;
    };
    let focus = if args.is_empty() {
        None
    } else {
        Some(args.join(" "))
    };
    match app_state
        .compress_conversation_owned(
            echo_agent_app_core::manual_compression::ManualCompressionRequest {
                conversation_id: conversation_id.clone(),
                surface: echo_agent_app_core::foreground_turn::ForegroundTurnSurface::Cli,
                focus,
                keep_messages,
            },
        )
        .await
    {
        Ok(receipt) => {
            println!(
                "{label}: {} -> {} msgs ({} tokens -> {})",
                receipt.messages_before,
                receipt.messages_after,
                receipt.tokens_before,
                receipt.tokens_after
            );
            if let Some(checkpoint) = receipt.checkpoint {
                println!(
                    "  Checkpoint: {} | Strategy: {} | Evicted: {} | Protected: {} | Tool fixes: {} | Duration: {}ms",
                    checkpoint.checkpoint_id,
                    checkpoint.strategy,
                    checkpoint.evicted_count,
                    checkpoint.protected_count,
                    checkpoint.tool_pair_fixes.len(),
                    checkpoint.compression_duration_ms
                );
                if let Some(focus) = checkpoint.focus_instructions {
                    println!("  Focus: {focus}");
                }
            }
        }
        Err(error) => println!("{label} failed: {error}"),
    }
    CommandOutcome::Continue
}

async fn cmd_compress(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    run_manual_compression(ctx, args, 6, "Compressed").await
}
// NOTE: No /cp alias — /cp belongs to /compact only
cmd!(
    CompressCommand,
    "compress",
    CommandCategory::Context,
    "Force context compression",
    cmd_compress
);

// ── CompactCommand ────────────────────────────────────────────────────

async fn cmd_compact(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    run_manual_compression(ctx, args, 12, "Compact").await
}
cmd!(
    CompactCommand,
    "compact",
    ["cp"],
    CommandCategory::Context,
    "Lightweight context compaction",
    cmd_compact
);

// ── ContextCommand ────────────────────────────────────────────────────

async fn cmd_context(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    ctx.agent
        .read_async(|a| {
            Box::pin(async move {
                let ctx = a.context().lock().await;
                println!("\n--- Context ---");
                // Show detailed token breakdown
                let breakdown = ctx.token_breakdown(None);
                println!("{}", breakdown.format_bar());
                println!(
                    "  Plan mode: {}  Iterations: {}",
                    a.is_plan_mode(),
                    a.max_iterations()
                );
            })
        })
        .await;
    CommandOutcome::Continue
}
cmd!(
    ContextCommand,
    "context",
    CommandCategory::Context,
    "Show context state",
    cmd_context
);

// ── CheckpointCommand ─────────────────────────────────────────────────

async fn cmd_checkpoint(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    let result = ctx
        .agent
        .read_async(|a| Box::pin(async move { a.force_checkpoint().await }))
        .await;
    match result {
        Ok(()) => println!("Checkpoint saved."),
        Err(error) => eprintln!("Failed to save checkpoint: {error}"),
    }
    CommandOutcome::Continue
}
cmd!(
    CheckpointCommand,
    "checkpoint",
    ["save"],
    CommandCategory::Context,
    "Force-save a runtime checkpoint (messages + plan + skills)",
    cmd_checkpoint
);

// ── RefreshCommand ────────────────────────────────────────────────────

async fn cmd_refresh(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("Project context refreshed.");
    CommandOutcome::Continue
}
cmd!(
    RefreshCommand,
    "refresh",
    CommandCategory::Context,
    "Rescan project files",
    cmd_refresh
);

// ── ProjectCommand ────────────────────────────────────────────────────

async fn cmd_project(_ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("\nProject context loaded from current directory.");
    CommandOutcome::Continue
}
cmd!(
    ProjectCommand,
    "project",
    ["proj"],
    CommandCategory::Context,
    "View/load project context",
    cmd_project
);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(ThinkCommand));
    registry.register(Arc::new(ReasoningCommand));
    registry.register(Arc::new(ModelCommand));
    registry.register(Arc::new(ProviderCommand));
    registry.register(Arc::new(SystemCommand));
    registry.register(Arc::new(CompressCommand));
    registry.register(Arc::new(CompactCommand));
    registry.register(Arc::new(ContextCommand));
    registry.register(Arc::new(CheckpointCommand));
    registry.register(Arc::new(RefreshCommand));
    registry.register(Arc::new(ProjectCommand));
}
