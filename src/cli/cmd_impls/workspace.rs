//! Workspace slash commands — new, list, switch, link, migrate, info.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent_app_core::workspace::WorkspaceKind;
use echo_agent_app_core::workspace::migration::LegacyMigrator;
use echo_agent_app_core::workspace::registry::WorkspaceRegistry;

// ── WorkspaceCommand ────────────────────────────────────────────────

async fn cmd_workspace(_ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    match args.first().copied() {
        Some("new") => ws_new(args.get(1..).unwrap_or(&[])),
        Some("list") | Some("ls") => ws_list(),
        Some("switch") | Some("sw") => ws_switch(args.get(1).copied()),
        Some("link") => ws_link(args.get(1).copied()),
        Some("migrate") => ws_migrate(args.get(1).copied().unwrap_or("")),
        Some("info") | None => ws_info(),
        Some(other) => {
            println!("Unknown workspace subcommand: {other}");
            println!("Usage: /workspace [new|list|switch|link|migrate|info]");
        }
    }
    CommandOutcome::Continue
}

/// `/workspace new <name> [--kind code|data|research]`
fn ws_new(args: &[&str]) {
    let name = match args.first() {
        Some(n) => *n,
        None => {
            println!("Usage: /workspace new <name> [--kind code|data|research]");
            return;
        }
    };

    // Parse optional --kind flag
    let kind = if let Some(idx) = args.iter().position(|&a| a == "--kind" || a == "-k") {
        args.get(idx + 1)
            .map(|k| WorkspaceKind::from_str_loose(k))
            .unwrap_or_default()
    } else {
        WorkspaceKind::default()
    };

    let registry = match WorkspaceRegistry::new() {
        Ok(r) => r,
        Err(e) => {
            println!("Failed to open workspace registry: {e}");
            return;
        }
    };

    match registry.create(name, kind) {
        Ok(ws) => {
            println!("Created workspace '{}' ({})", ws.name, ws.id);
            println!("  Root: {}", ws.root.display());
            println!("  Kind: {}", ws.kind.display_name());
        }
        Err(e) => {
            println!("Failed to create workspace: {e}");
        }
    }
}

/// `/workspace list`
fn ws_list() {
    let registry = match WorkspaceRegistry::new() {
        Ok(r) => r,
        Err(e) => {
            println!("Failed to open workspace registry: {e}");
            return;
        }
    };

    match registry.list() {
        Ok(workspaces) => {
            if workspaces.is_empty() {
                println!("No workspaces found.");
                println!("Create one with: /workspace new <name>");
                return;
            }
            println!("\n  Workspaces ({}):", workspaces.len());
            println!("  {:-<60}", "");
            for ws in &workspaces {
                let icon = ws.kind.icon();
                let kind = ws.kind.display_name();
                let project = ws
                    .project_root
                    .as_ref()
                    .map(|p| format!(" -> {}", p.display()))
                    .unwrap_or_default();
                println!(
                    "  {} {:<20} [{}]{}{}",
                    icon,
                    ws.name,
                    kind,
                    project,
                    if ws.id.as_str() == "default" {
                        " (active)"
                    } else {
                        ""
                    }
                );
            }
            println!();
        }
        Err(e) => {
            println!("Failed to list workspaces: {e}");
        }
    }
}

/// `/workspace switch <name>`
fn ws_switch(name: Option<&str>) {
    let name = match name {
        Some(n) => n,
        None => {
            println!("Usage: /workspace switch <name>");
            return;
        }
    };

    let registry = match WorkspaceRegistry::new() {
        Ok(r) => r,
        Err(e) => {
            println!("Failed to open workspace registry: {e}");
            return;
        }
    };

    match registry.open_by_name(name) {
        Ok(ws) => {
            println!("Switched to workspace: {} ({})", ws.name, ws.id);
            println!("  Root: {}", ws.root.display());
            println!("  Kind: {}", ws.kind.display_name());
            if let Some(ref p) = ws.project_root {
                println!("  Project: {}", p.display());
            }
        }
        Err(e) => {
            println!("Failed to switch workspace: {e}");
            println!("Available workspaces:");
            ws_list();
        }
    }
}

/// `/workspace link <path>`
fn ws_link(path: Option<&str>) {
    let path = match path {
        Some(p) => p,
        None => {
            println!("Usage: /workspace link <path>");
            println!("Links a project directory to the current workspace.");
            return;
        }
    };

    let project_path = std::path::PathBuf::from(path);
    if !project_path.exists() {
        println!("Path does not exist: {path}");
        return;
    }

    // For now, link to "default" workspace — in the future this will use
    // the currently active workspace from CommandContext.
    let registry = match WorkspaceRegistry::new() {
        Ok(r) => r,
        Err(e) => {
            println!("Failed to open workspace registry: {e}");
            return;
        }
    };

    // Find the most recently active workspace, or create "default"
    let ws_id = match registry.list() {
        Ok(workspaces) if !workspaces.is_empty() => workspaces[0].id.clone(),
        _ => {
            // Auto-create default workspace
            match registry.create("default", WorkspaceKind::General) {
                Ok(ws) => ws.id.clone(),
                Err(e) => {
                    println!("Failed to create default workspace: {e}");
                    return;
                }
            }
        }
    };

    match registry.link_project(&ws_id, project_path) {
        Ok(ws) => {
            println!(
                "Linked project to workspace '{}': {}",
                ws.name,
                ws.project_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            );
        }
        Err(e) => {
            println!("Failed to link project: {e}");
        }
    }
}

/// `/workspace migrate [--dry-run]`
fn ws_migrate(sub: &str) {
    let migrator = LegacyMigrator::new();

    if !migrator.has_legacy_data() {
        println!("No legacy data found to migrate.");
        return;
    }

    match migrator.audit() {
        Ok(plan) => {
            println!("\n  Migration Plan:");
            println!("  {:-<40}", "");
            println!(
                "  Workspaces to create: {}",
                plan.workspaces_to_create.len()
            );
            println!("  Sessions to migrate:  {}", plan.ungrouped_sessions.len());
            println!("  Conversations:        {}", plan.conversation_count);
            println!(
                "  Estimated size:       {} KB",
                plan.estimated_size_bytes / 1024
            );

            for ws_plan in &plan.workspaces_to_create {
                println!(
                    "    - '{}' ({} sessions, {} conversations)",
                    ws_plan.name,
                    ws_plan.session_files.len(),
                    ws_plan.conversation_files.len()
                );
            }

            if sub == "--dry-run" || sub == "-n" {
                println!("\n  Dry run — no changes made.");
                println!("  Run /workspace migrate to execute.");
                return;
            }

            let registry = match WorkspaceRegistry::new() {
                Ok(r) => r,
                Err(e) => {
                    println!("Failed to open workspace registry: {e}");
                    return;
                }
            };

            match migrator.execute(&plan, &registry) {
                Ok(report) => {
                    println!("\n  Migration complete:");
                    println!(
                        "    Workspaces created: {}",
                        report.workspaces_created.len()
                    );
                    println!("    Sessions migrated:  {}", report.sessions_migrated);
                    println!("    Conversations:      {}", report.conversations_migrated);
                    if !report.errors.is_empty() {
                        println!("    Errors ({}):", report.errors.len());
                        for err in &report.errors {
                            println!("      - {err}");
                        }
                    }
                }
                Err(e) => {
                    println!("Migration failed: {e}");
                }
            }
        }
        Err(e) => {
            println!("Failed to audit legacy data: {e}");
        }
    }
}

/// `/workspace` or `/workspace info` — show current workspace info
fn ws_info() {
    let registry = match WorkspaceRegistry::new() {
        Ok(r) => r,
        Err(e) => {
            println!("Failed to open workspace registry: {e}");
            return;
        }
    };

    // Show the most recently active workspace as "current"
    match registry.list() {
        Ok(workspaces) if !workspaces.is_empty() => {
            let ws = &workspaces[0];
            println!("\n  Current Workspace:");
            println!("  {:-<40}", "");
            println!("  Name:    {}", ws.name);
            println!("  ID:      {}", ws.id);
            println!("  Kind:    {} {}", ws.kind.icon(), ws.kind.display_name());
            println!("  Root:    {}", ws.root.display());
            if let Some(ref p) = ws.project_root {
                println!("  Project: {}", p.display());
            }
            println!("  Created: {}", ws.created_at.format("%Y-%m-%d %H:%M"));
            println!("  Active:  {}", ws.last_active.format("%Y-%m-%d %H:%M"));
            println!();
        }
        _ => {
            println!("No active workspace.");
            println!("Create one with: /workspace new <name>");
        }
    }
}

cmd!(
    WorkspaceCommand,
    "workspace",
    ["ws"],
    CommandCategory::Session,
    "Manage workspaces (new/list/switch/link/migrate)",
    cmd_workspace
);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(std::sync::Arc::new(WorkspaceCommand));
}
