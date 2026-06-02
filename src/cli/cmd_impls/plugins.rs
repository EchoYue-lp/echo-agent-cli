//! Plugin management slash commands.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent::plugin::{InstallSource, PluginRegistry, PluginScope};
use std::path::PathBuf;
use std::sync::Arc;

// ── PluginsCommand ───────────────────────────────────────────────────

fn detect_project_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut dir = cwd.as_path();
    loop {
        if dir.join(".echo-agent").exists() || dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

async fn cmd_plugins(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let sub = args.first().copied().unwrap_or("list");
    let rest: &[&str] = args.get(1..).unwrap_or(&[]);

    let project_root = detect_project_root();
    let mut registry = PluginRegistry::new(project_root.clone());

    match sub {
        "list" | "ls" | "" => {
            if let Err(e) = registry.scan_all() {
                println!("Error scanning plugins: {e}");
                return CommandOutcome::Continue;
            }

            let plugins = registry.list();
            if plugins.is_empty() {
                println!("\n--- No plugins installed ---");
                println!("Use /plugins install <path|git-url> to add plugins.");
                return CommandOutcome::Continue;
            }

            println!("\n--- Installed Plugins ({}) ---", plugins.len());
            for entry in &plugins {
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

            match registry.install(&source, scope) {
                Ok(id) => {
                    println!("Plugin '{id}' installed successfully.");
                    if let Some(entry) = registry.get(&id) {
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
                    println!("\nRestart or run /plugins reload to activate.");
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

            if let Err(e) = registry.scan_all() {
                println!("Error scanning plugins: {e}");
                return CommandOutcome::Continue;
            }

            println!("Uninstalling plugin '{name}'...");
            match registry.uninstall(name, keep_data) {
                Ok(()) => {
                    println!("Plugin '{name}' uninstalled.");
                    if keep_data {
                        println!("  (Data directory preserved)");
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
            if let Err(e) = registry.scan_all() {
                println!("Error scanning plugins: {e}");
                return CommandOutcome::Continue;
            }

            match registry.enable(name) {
                Ok(()) => {
                    println!("Plugin '{name}' enabled.");
                    println!("Run /plugins reload to activate.");
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
            if let Err(e) = registry.scan_all() {
                println!("Error scanning plugins: {e}");
                return CommandOutcome::Continue;
            }

            match registry.disable(name) {
                Ok(()) => {
                    println!("Plugin '{name}' disabled.");
                    println!("Run /plugins reload to deactivate.");
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
            if let Err(e) = registry.scan_all() {
                println!("Error scanning plugins: {e}");
                return CommandOutcome::Continue;
            }

            match registry.get(name) {
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

                    // Resolve components
                    if let Ok(resolved) = registry.resolve_components(name) {
                        println!("\n  Resolved Components:");
                        if !resolved.skill_dirs.is_empty() {
                            println!("    Skills: {} directories", resolved.skill_dirs.len());
                        }
                        if !resolved.agent_files.is_empty() {
                            println!("    Agents: {} files", resolved.agent_files.len());
                        }
                        if resolved.hooks_file.is_some() {
                            println!("    Hooks: configured");
                        }
                        if resolved.mcp_config_file.is_some() {
                            println!("    MCP Servers: configured");
                        }
                        if resolved.lsp_config_file.is_some() {
                            println!("    LSP Servers: configured");
                        }
                    }
                }
                None => println!("Plugin '{name}' not found."),
            }
        }

        "reload" => {
            println!("Reloading plugins...");
            if let Err(e) = registry.scan_all() {
                println!("Error reloading plugins: {e}");
                return CommandOutcome::Continue;
            }

            let count = registry.count();
            let enabled = registry.list_enabled().len();
            println!("Loaded {count} plugins ({enabled} enabled).");
            println!("Note: Plugin components will be wired on next agent restart.");
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

            // Create default directories
            let _ = std::fs::create_dir_all(dir.join("skills"));
            let _ = std::fs::create_dir_all(dir.join("hooks"));
            let _ = std::fs::create_dir_all(dir.join("agents"));

            // Create placeholder files
            let _ = std::fs::write(dir.join("hooks").join("hooks.yaml"), "# Hook definitions\n");
            let _ = std::fs::write(dir.join(".mcp.json"), "{\n  \"mcpServers\": {}\n}\n");

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
