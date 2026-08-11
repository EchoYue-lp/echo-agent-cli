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
            if rest.is_empty() {
                println!("Usage: /plugins install <path|git-url> [--scope user|project|local]");
                return CommandOutcome::Continue;
            }

            let source_str = rest[0];
            let scope = rest
                .windows(2)
                .find(|w| w[0] == "--scope")
                .and_then(|w| PluginScope::from_arg(w[1]))
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
            if rest.is_empty() {
                println!("Usage: /plugins uninstall <name> [--keep-data]");
                return CommandOutcome::Continue;
            }

            let name = rest[0];
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
            if rest.is_empty() {
                println!("Usage: /plugins enable <name>");
                return CommandOutcome::Continue;
            }

            let name = rest[0];
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
            if rest.is_empty() {
                println!("Usage: /plugins disable <name>");
                return CommandOutcome::Continue;
            }

            let name = rest[0];
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
            if rest.is_empty() {
                println!("Usage: /plugins info <name>");
                return CommandOutcome::Continue;
            }

            let name = rest[0];
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
                    if !summary.errors.is_empty() {
                        println!("Errors ({}):", summary.errors.len());
                        for error in summary.errors {
                            println!("  - {error}");
                        }
                    }
                }
                Err(error) => println!("Plugin reload failed: {error}"),
            }
        }

        "init" => {
            let name = rest.first().copied().unwrap_or("my-plugin");
            let dir = std::path::PathBuf::from(name);
            let manifest_dir = dir.join(".echo-plugin");

            if manifest_dir.exists() {
                println!(
                    "Plugin directory already exists: {}",
                    manifest_dir.display()
                );
                return CommandOutcome::Continue;
            }

            if let Err(e) = std::fs::create_dir_all(&manifest_dir) {
                println!("Failed to create directory: {e}");
                return CommandOutcome::Continue;
            }

            let manifest = format!(
                r#"# EchoAgent Plugin Manifest
name: {name}
display_name: "{}"
version: "0.1.0"
description: "A new EchoAgent plugin"
license: MIT
keywords: []

components:
  skills: "./skills/"
  hooks: "./hooks/hooks.yaml"
  mcp_servers: "./.mcp.json"
"#,
                name.split('-')
                    .map(|w| {
                        let mut c = w.chars();
                        match c.next() {
                            None => String::new(),
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            );

            if let Err(e) = std::fs::write(manifest_dir.join("manifest.yaml"), &manifest) {
                println!("Failed to write manifest: {e}");
                return CommandOutcome::Continue;
            }

            for path in [dir.join("skills"), dir.join("hooks"), dir.join("agents")] {
                if let Err(error) = std::fs::create_dir_all(&path) {
                    println!("Failed to create {}: {error}", path.display());
                    return CommandOutcome::Continue;
                }
            }

            for (path, content) in [
                (dir.join("hooks").join("hooks.yaml"), "{}\n"),
                (dir.join(".mcp.json"), "{\n  \"mcpServers\": {}\n}\n"),
            ] {
                if let Err(error) = std::fs::write(&path, content) {
                    println!("Failed to write {}: {error}", path.display());
                    return CommandOutcome::Continue;
                }
            }

            println!("Plugin scaffolded at: {}", dir.display());
            println!("  .echo-plugin/manifest.yaml");
            println!("  skills/");
            println!("  hooks/hooks.yaml");
            println!("  agents/");
            println!("  .mcp.json");
            println!("\nEdit manifest.yaml to configure your plugin.");
        }

        "validate" => {
            let path = rest.first().copied().unwrap_or(".");
            let manifest_path = std::path::PathBuf::from(path)
                .join(".echo-plugin")
                .join("manifest.yaml");

            if !manifest_path.exists() {
                println!("No manifest found at: {}", manifest_path.display());
                return CommandOutcome::Continue;
            }

            match echo_agent::plugin::PluginManifest::from_file(&manifest_path) {
                Ok(manifest) => {
                    let errors = manifest.validate();
                    if errors.is_empty() {
                        println!("Plugin manifest is valid.");
                        println!("  Name: {}", manifest.name);
                        println!("  Version: {}", manifest.version);
                        println!("  Description: {}", manifest.description);
                        let caps = manifest.inferred_capabilities();
                        if !caps.is_empty() {
                            println!(
                                "  Capabilities: {}",
                                caps.iter()
                                    .map(|c| c.display_name())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                    } else {
                        println!("Manifest validation errors:");
                        for err in &errors {
                            println!("  - {err}");
                        }
                    }
                }
                Err(e) => println!("Failed to parse manifest: {e}"),
            }
        }

        _ => {
            println!("Unknown subcommand: {sub}");
            println!(
                "Available: list, install, uninstall, enable, disable, info, reload, init, validate"
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
    "Manage plugins (install, enable, disable, info)",
    cmd_plugins
);

// ── Register ───────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(PluginsCommand));
}
