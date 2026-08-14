//! Workspace slash commands — new, list, switch, link, migrate, info.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent::agent::Agent;
use echo_agent_app_core::state::AppState;
use echo_agent_app_core::workspace::WorkspaceKind;
use echo_agent_app_core::workspace::migration::LegacyMigrator;

pub struct WorkspaceCommandResult {
    pub output: String,
    pub generation_changed: bool,
}

// ── WorkspaceCommand ────────────────────────────────────────────────

async fn cmd_workspace(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let result = execute_workspace_command(ctx.app_state.as_deref(), args).await;
    if result.generation_changed {
        ctx.staged_attachments.lock().await.clear();
    }
    println!("{}", result.output);
    CommandOutcome::Continue
}

pub async fn execute_workspace_command(
    app_state: Option<&AppState>,
    args: &[&str],
) -> WorkspaceCommandResult {
    let Some(state) = app_state else {
        return WorkspaceCommandResult {
            output: "Workspace management is unavailable in this runtime.".to_string(),
            generation_changed: false,
        };
    };
    match args.first().copied() {
        Some("new") => unchanged(ws_new(state, args.get(1..).unwrap_or(&[]))),
        Some("list") | Some("ls") => unchanged(ws_list(state).await),
        Some("switch") | Some("sw") => ws_switch(state, args.get(1).copied()).await,
        Some("exit") => ws_exit(state).await,
        Some("link") => unchanged(ws_link(state, args.get(1).copied()).await),
        Some("migrate") => unchanged(ws_migrate(state, args.get(1).copied().unwrap_or(""))),
        Some("info") | None => unchanged(ws_info(state).await),
        Some(other) => unchanged(format!(
            "Unknown workspace subcommand: {other}\nUsage: /workspace [new|list|switch|exit|link|migrate|info]"
        )),
    }
}

fn unchanged(output: String) -> WorkspaceCommandResult {
    WorkspaceCommandResult {
        output,
        generation_changed: false,
    }
}

/// `/workspace new <name> [--kind code|data|research]`
fn ws_new(state: &AppState, args: &[&str]) -> String {
    let name = match args.first() {
        Some(n) => *n,
        None => {
            return "Usage: /workspace new <name> [--kind code|data|research]".to_string();
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

    match state.workspace.registry.create(name, kind) {
        Ok(ws) => format!(
            "Created workspace '{}' ({})\n  Root: {}\n  Kind: {}",
            ws.name,
            ws.id,
            ws.root.display(),
            ws.kind.display_name()
        ),
        Err(error) => format!("Failed to create workspace: {error}"),
    }
}

/// `/workspace list`
async fn ws_list(state: &AppState) -> String {
    let current_id = state
        .current_workspace()
        .await
        .map(|workspace| workspace.id);
    match state.workspace.registry.list() {
        Ok(workspaces) => {
            if workspaces.is_empty() {
                return "No workspaces found.\nCreate one with: /workspace new <name>".to_string();
            }
            let mut output = format!("Workspaces ({}):\n", workspaces.len());
            for ws in &workspaces {
                let project = ws
                    .project_root
                    .as_ref()
                    .map(|p| format!(" -> {}", p.display()))
                    .unwrap_or_default();
                output.push_str(&format!(
                    "  {} [{}]{}{}\n",
                    ws.name,
                    ws.kind.display_name(),
                    project,
                    if current_id.as_ref() == Some(&ws.id) {
                        " (active)"
                    } else {
                        ""
                    }
                ));
            }
            output.trim_end().to_string()
        }
        Err(error) => format!("Failed to list workspaces: {error}"),
    }
}

/// `/workspace switch <name>`
async fn ws_switch(state: &AppState, name: Option<&str>) -> WorkspaceCommandResult {
    let name = match name {
        Some(n) => n,
        None => {
            return unchanged("Usage: /workspace switch <name>".to_string());
        }
    };
    match state.workspace.registry.open_by_name(name) {
        Ok(ws) => {
            if let Err(error) = state.switch_workspace(ws.clone()).await {
                return unchanged(format!("Failed to switch workspace: {error}"));
            }
            reset_workspace_conversation(state).await;
            WorkspaceCommandResult {
                output: format!(
                    "Switched to workspace: {} ({})\n  Root: {}\n  Kind: {}{}",
                    ws.name,
                    ws.id,
                    ws.root.display(),
                    ws.kind.display_name(),
                    ws.project_root
                        .as_ref()
                        .map(|path| format!("\n  Project: {}", path.display()))
                        .unwrap_or_default()
                ),
                generation_changed: true,
            }
        }
        Err(error) => unchanged(format!("Failed to switch workspace: {error}")),
    }
}

async fn ws_exit(state: &AppState) -> WorkspaceCommandResult {
    if let Err(error) = state.exit_workspace().await {
        return unchanged(format!("Failed to exit workspace: {error}"));
    }
    reset_workspace_conversation(state).await;
    WorkspaceCommandResult {
        output: "Exited workspace; using global paths.".to_string(),
        generation_changed: true,
    }
}

async fn reset_workspace_conversation(state: &AppState) {
    let conversation_id = uuid::Uuid::new_v4().to_string();
    state
        .connection
        .agent
        .write_async(|agent| {
            Box::pin(async move {
                agent.reset().await;
                agent.set_conversation_id(conversation_id);
            })
        })
        .await;
}

/// `/workspace link <path>`
async fn ws_link(state: &AppState, path: Option<&str>) -> String {
    let path = match path {
        Some(p) => p,
        None => {
            return "Usage: /workspace link <path>\nLinks a project directory to the current workspace."
                .to_string();
        }
    };

    let project_path = std::path::PathBuf::from(path);
    if !project_path.exists() {
        return format!("Path does not exist: {path}");
    }
    let Some(current) = state.current_workspace().await else {
        return "No active workspace. Switch to one before linking a project.".to_string();
    };
    match state
        .workspace
        .registry
        .link_project(&current.id, project_path)
    {
        Ok(ws) => {
            *state.workspace.current.write().await = Some(ws.clone());
            format!(
                "Linked project to workspace '{}': {}",
                ws.name,
                ws.project_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            )
        }
        Err(error) => format!("Failed to link project: {error}"),
    }
}

/// `/workspace migrate [--dry-run]`
fn ws_migrate(state: &AppState, sub: &str) -> String {
    let migrator = LegacyMigrator::new();

    if !migrator.has_legacy_data() {
        return "No legacy data found to migrate.".to_string();
    }

    match migrator.audit() {
        Ok(plan) => {
            let mut output = format!(
                "Migration plan:\n  Workspaces to create: {}\n  Sessions to migrate: {}\n  Conversations: {}\n  Estimated size: {} KB",
                plan.workspaces_to_create.len(),
                plan.ungrouped_sessions.len(),
                plan.conversation_count,
                plan.estimated_size_bytes / 1024
            );
            for ws_plan in &plan.workspaces_to_create {
                output.push_str(&format!(
                    "\n  - '{}' ({} sessions, {} conversations)",
                    ws_plan.name,
                    ws_plan.session_files.len(),
                    ws_plan.conversation_files.len()
                ));
            }

            if sub == "--dry-run" || sub == "-n" {
                output.push_str("\nDry run; no changes made. Run /workspace migrate to execute.");
                return output;
            }
            match migrator.execute(&plan, state.workspace.registry.as_ref()) {
                Ok(report) => {
                    output.push_str(&format!(
                        "\nMigration complete:\n  Workspaces created: {}\n  Sessions migrated: {}\n  Conversations: {}",
                        report.workspaces_created.len(),
                        report.sessions_migrated,
                        report.conversations_migrated
                    ));
                    if !report.errors.is_empty() {
                        output.push_str(&format!("\n  Errors ({}):", report.errors.len()));
                        for err in &report.errors {
                            output.push_str(&format!("\n    - {err}"));
                        }
                    }
                    output
                }
                Err(error) => format!("Migration failed: {error}"),
            }
        }
        Err(error) => format!("Failed to audit legacy data: {error}"),
    }
}

/// `/workspace` or `/workspace info` — show current workspace info
async fn ws_info(state: &AppState) -> String {
    match state.current_workspace().await {
        Some(ws) => format!(
            "Current workspace:\n  Name: {}\n  ID: {}\n  Kind: {}\n  Root: {}{}\n  Created: {}\n  Active: {}",
            ws.name,
            ws.id,
            ws.kind.display_name(),
            ws.root.display(),
            ws.project_root
                .as_ref()
                .map(|path| format!("\n  Project: {}", path.display()))
                .unwrap_or_default(),
            ws.created_at.format("%Y-%m-%d %H:%M"),
            ws.last_active.format("%Y-%m-%d %H:%M")
        ),
        None => "No active workspace.\nCreate one with: /workspace new <name>".to_string(),
    }
}

cmd!(
    WorkspaceCommand,
    "workspace",
    ["ws"],
    CommandCategory::Session,
    "Manage workspaces (new/list/switch/exit/link/migrate)",
    cmd_workspace
);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(std::sync::Arc::new(WorkspaceCommand));
}
