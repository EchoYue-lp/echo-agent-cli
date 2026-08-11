//! Plugin management slash commands.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent::plugin::{InstallSource, PluginScope};
use std::sync::Arc;

// ── PluginsCommand ───────────────────────────────────────────────────

async fn cmd_plugins(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("list");
    let rest: &[&str] = args.get(1..).unwrap_or(&[]);

    match sub {
        "list" | "ls" | "" => {
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };

            let plugins = runtime.list().await;
            if plugins.is_empty() {
                println!("\n--- No plugins installed ---");
                println!("Use /plugins install <path|git-url> to add plugins.");
                return CommandOutcome::Continue;
            }

            println!("\n--- Installed Plugins ({}) ---", plugins.len());
            for entry in plugins {
                let status = if entry.enabled { "enabled" } else { "disabled" };
                let caps = entry.manifest.inferred_capabilities();
                let cap_str = caps
                    .iter()
                    .map(|c| c.display_name())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "  * {} v{} [{}] — {}",
                    entry.manifest.name, entry.manifest.version, status, entry.manifest.description
                );
                if !cap_str.is_empty() {
                    println!("    Capabilities: {cap_str}");
                }
                println!("    Scope: {}, Path: {}", entry.scope, entry.root.display());
            }
        }

        "install" => {
            let Some(source_str) = rest.first().copied() else {
                println!("Usage: /plugins install <path|git-url> [--scope user|project|local]");
                return CommandOutcome::Continue;
            };
            let scope = rest
                .windows(2)
                .find(|window| window.first() == Some(&"--scope"))
                .and_then(|window| window.get(1))
                .and_then(|value| PluginScope::from_arg(value))
                .unwrap_or(PluginScope::User);

            let source = InstallSource::parse(source_str);
            println!("Installing plugin from {source} (scope: {scope})...");

            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };
            match runtime.install(&source, scope).await {
                Ok((id, summary)) => {
                    println!("Plugin '{id}' installed successfully.");
                    let mut enabled = false;
                    if let Some(entry) = runtime.get(&id).await {
                        enabled = entry.enabled;
                        let caps = entry.manifest.inferred_capabilities();
                        println!("  Version: {}", entry.manifest.version);
                        println!("  Description: {}", entry.manifest.description);
                        if !caps.is_empty() {
                            println!(
                                "  Capabilities: {}",
                                caps.iter()
                                    .map(|c| c.display_name())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                    }
                    if !enabled {
                        println!("Plugin is disabled by its manifest default.");
                    } else if summary.errors.is_empty() {
                        println!("Plugin components are active in the current session.");
                    } else {
                        println!("Plugin installed, but some components failed to activate:");
                        for error in summary.errors {
                            println!("  - {error}");
                        }
                    }
                }
                Err(e) => println!("Install failed: {e}"),
            }
        }

        "uninstall" | "remove" => {
            let Some(name) = rest.first().copied() else {
                println!("Usage: /plugins uninstall <name> [--keep-data]");
                return CommandOutcome::Continue;
            };
            let keep_data = rest.contains(&"--keep-data");

            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };

            println!("Uninstalling plugin '{name}'...");
            match runtime.uninstall(name, keep_data).await {
                Ok(summary) => {
                    println!("Plugin '{name}' uninstalled.");
                    if keep_data {
                        println!("  (Data directory preserved)");
                    }
                    for error in summary.errors {
                        println!("  Remaining plugin wiring error: {error}");
                    }
                }
                Err(e) => println!("Uninstall failed: {e}"),
            }
        }

        "enable" => {
            let Some(name) = rest.first().copied() else {
                println!("Usage: /plugins enable <name>");
                return CommandOutcome::Continue;
            };
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };

            match runtime.enable(name).await {
                Ok(summary) => {
                    if summary.errors.is_empty() {
                        println!("Plugin '{name}' enabled and activated.");
                    } else {
                        println!(
                            "Plugin '{name}' enabled, but component activation is incomplete:"
                        );
                        for error in summary.errors {
                            println!("  - {error}");
                        }
                    }
                }
                Err(e) => println!("Enable failed: {e}"),
            }
        }

        "disable" => {
            let Some(name) = rest.first().copied() else {
                println!("Usage: /plugins disable <name>");
                return CommandOutcome::Continue;
            };
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };

            match runtime.disable(name).await {
                Ok(summary) => {
                    println!("Plugin '{name}' disabled and unloaded.");
                    for error in summary.errors {
                        println!("  Remaining plugin wiring error: {error}");
                    }
                }
                Err(e) => println!("Disable failed: {e}"),
            }
        }

        "info" | "details" => {
            let Some(name) = rest.first().copied() else {
                println!("Usage: /plugins info <name>");
                return CommandOutcome::Continue;
            };
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };

            match runtime.get(name).await {
                Some(entry) => {
                    println!("\n--- Plugin: {} ---", entry.manifest.name);
                    println!("  Version: {}", entry.manifest.version);
                    println!("  Description: {}", entry.manifest.description);
                    if let Some(ref author) = entry.manifest.author {
                        println!("  Author: {}", author.name);
                    }
                    if let Some(ref license) = entry.manifest.license {
                        println!("  License: {license}");
                    }
                    println!("  Scope: {}", entry.scope);
                    println!("  Enabled: {}", entry.enabled);
                    println!("  Path: {}", entry.root.display());

                    let caps = entry.manifest.inferred_capabilities();
                    if !caps.is_empty() {
                        println!(
                            "\n  Capabilities: {}",
                            caps.iter()
                                .map(|c| c.display_name())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }

                    if !entry.manifest.keywords.is_empty() {
                        println!("  Keywords: {}", entry.manifest.keywords.join(", "));
                    }

                    if !entry.manifest.dependencies.is_empty() {
                        println!("\n  Dependencies:");
                        for dep in &entry.manifest.dependencies {
                            print!("    - {}", dep.name());
                            if let Some(ver) = dep.version_constraint() {
                                print!(" ({ver})");
                            }
                            println!();
                        }
                    }

                    if !entry.manifest.config.is_empty() {
                        println!("\n  Configuration:");
                        for (key, cfg) in &entry.manifest.config {
                            let required = if cfg.required { " (required)" } else { "" };
                            println!("    - {key}: {}{}", cfg.title, required);
                            if !cfg.description.is_empty() {
                                println!("      {}", cfg.description);
                            }
                        }
                    }
                }
                None => println!("Plugin '{name}' not found."),
            }
        }

        "reload" => {
            println!("Reloading plugins...");
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };
            match runtime.reload().await {
                Ok(summary) => {
                    println!(
                        "Loaded {} plugins ({} enabled).",
                        summary.total, summary.enabled
                    );
                    println!("  Skills loaded:    {}", summary.skills_loaded);
                    println!("  Hooks registered: {}", summary.hooks_registered);
                    println!("  MCP connected:    {}", summary.mcp_connected);
                    println!("  Agents loaded:    {}", summary.agents_loaded);
                    println!("  LSP languages:    {}", summary.lsp_languages_loaded);
                    println!("  Monitors loaded:  {}", summary.monitors_loaded);
                    println!("  Themes loaded:    {}", summary.themes_loaded);
                    println!("  Output styles:    {}", summary.output_styles_loaded);
                }
                Err(error) => println!("Plugin reload failed: {error}"),
            }
        }

        "themes" => {
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };
            let active = runtime.active_theme().await;
            let themes = runtime.themes().await;
            if themes.is_empty() {
                println!("No plugin themes are loaded.");
            } else {
                for theme in themes {
                    println!(
                        "{}{} [{}] from {}",
                        if active.as_deref() == Some(theme.name.as_str()) {
                            "* "
                        } else {
                            "  "
                        },
                        theme.display_name.as_deref().unwrap_or(&theme.name),
                        if theme.dark { "dark" } else { "light" },
                        theme.plugin
                    );
                }
            }
        }

        "theme" => {
            let Some(name) = rest.first().copied() else {
                println!("Usage: /plugins theme <name|default>");
                return CommandOutcome::Continue;
            };
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };
            let selected = (!matches!(name, "default" | "off" | "none")).then_some(name);
            match runtime.activate_theme(selected).await {
                Ok(_) => match selected {
                    Some(name) => println!("Theme '{name}' activated."),
                    None => println!("Theme reset to default."),
                },
                Err(error) => println!("Theme activation failed: {error}"),
            }
        }

        "config" | "configure" => {
            let Some(name) = rest.first().copied() else {
                println!("Usage: /plugins config <name> <json-object>");
                return CommandOutcome::Continue;
            };
            let Some(json) = rest.get(1..).map(|values| values.join(" ")) else {
                println!("Usage: /plugins config <name> <json-object>");
                return CommandOutcome::Continue;
            };
            let values = match serde_json::from_str::<
                std::collections::HashMap<String, serde_json::Value>,
            >(&json)
            {
                Ok(values) => values,
                Err(error) => {
                    println!("Plugin config JSON is invalid: {error}");
                    return CommandOutcome::Continue;
                }
            };
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };
            match runtime.configure(name, values).await {
                Ok(summary) if summary.errors.is_empty() => {
                    println!("Plugin '{name}' configured and reloaded.");
                }
                Ok(summary) => {
                    println!("Plugin '{name}' configured with reload errors:");
                    println!("{}", summary.errors.join("\n"));
                }
                Err(error) => println!("Plugin configuration failed: {error}"),
            }
        }

        "styles" => {
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };
            let active = runtime.active_output_style().await;
            let styles = runtime.output_styles().await;
            if styles.is_empty() {
                println!("No plugin output styles are loaded.");
            } else {
                for style in styles {
                    println!(
                        "{}{} from {} - {}",
                        if active.as_deref() == Some(style.name.as_str()) {
                            "* "
                        } else {
                            "  "
                        },
                        style.name,
                        style.plugin,
                        style.description
                    );
                }
            }
        }

        "style" => {
            let Some(name) = rest.first().copied() else {
                println!("Usage: /plugins style <name|default>");
                return CommandOutcome::Continue;
            };
            let Some(runtime) = ctx.plugin_runtime.as_ref() else {
                println!("Plugin runtime is not initialized.");
                return CommandOutcome::Continue;
            };
            let selected = (!matches!(name, "default" | "off" | "none")).then_some(name);
            match runtime.activate_output_style(selected).await {
                Ok(()) => match selected {
                    Some(name) => println!("Output style '{name}' activated."),
                    None => println!("Output style reset to default."),
                },
                Err(error) => println!("Output style activation failed: {error}"),
            }
        }

        "init" => {
            let directory = rest.first().copied().unwrap_or("my-plugin");
            let default_name = std::path::Path::new(directory)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("my-plugin");
            let name = rest.get(1).copied().unwrap_or(default_name);
            match echo_agent_app_core::plugin_runtime::PluginRuntimeService::scaffold(
                directory, name,
            ) {
                Ok(result) => println!(
                    "Plugin '{}' scaffolded at {}",
                    result.name,
                    result.path.display()
                ),
                Err(error) => println!("Plugin scaffold failed: {error}"),
            }
        }

        "validate" => {
            let path = rest.first().copied().unwrap_or(".");
            let report = echo_agent_app_core::plugin_runtime::PluginRuntimeService::validate(path);
            if report.valid {
                println!(
                    "Plugin '{}' is valid.",
                    report.name.as_deref().unwrap_or("<unknown>")
                );
                println!("  Components: {}", report.components.join(", "));
            } else {
                println!("Plugin validation failed:");
                for error in report.errors {
                    println!("  - {error}");
                }
            }
        }

        _ => {
            println!("Unknown subcommand: {sub}");
            println!(
                "Available: list, install, uninstall, enable, disable, info, reload, config, themes, theme, styles, style, init, validate"
            );
        }
    }

    CommandOutcome::Continue
}

cmd!(
    PluginsCommand,
    "plugins",
    ["plugin"],
    CommandCategory::Config,
    "Manage plugins and live plugin components",
    cmd_plugins
);

// ── Register ───────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(PluginsCommand));
}
