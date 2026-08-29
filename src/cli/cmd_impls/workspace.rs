//! Workspace slash commands — new, list, switch, link, and info.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent_app_core::api::state::AppState;
use echo_agent_app_core::api::workspace::WorkspaceKind;

pub struct WorkspaceCommandResult {
    pub output: String,
    pub generation_changed: bool,
}

pub const WORKSPACE_SUBCOMMAND_USAGE: &str = "[new|list|switch|exit|link|info] [args]";

// ── WorkspaceCommand ────────────────────────────────────────────────

async fn cmd_workspace(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let result = execute_workspace_command(ctx.app_state.as_ref(), args).await;
    if result.generation_changed {
        let attachments = {
            let mut staged = ctx.staged_attachments.lock().await;
            std::mem::take(&mut *staged)
        };
        if let Err(error) =
            echo_agent_app_core::api::attachments::discard_staged_attachment_refs(&attachments)
        {
            tracing::warn!(%error, "failed to clean staged attachments after workspace change");
        }
    }
    println!("{}", result.output);
    CommandOutcome::Continue
}

pub async fn execute_workspace_command(
    app_state: Option<&std::sync::Arc<AppState>>,
    args: &[&str],
) -> WorkspaceCommandResult {
    let Some(state) = app_state else {
        return WorkspaceCommandResult {
            output: "Workspace management is unavailable in this runtime.".to_string(),
            generation_changed: false,
        };
    };
    match args.first().copied() {
        Some("new") => unchanged(ws_new(state, args.get(1..).unwrap_or(&[])).await),
        Some("list") | Some("ls") => unchanged(ws_list(state).await),
        Some("switch") | Some("sw") => ws_switch(state, args.get(1).copied()).await,
        Some("exit") => ws_exit(state).await,
        Some("link") => unchanged(ws_link(state, args.get(1).copied()).await),
        Some("info") | None => unchanged(ws_info(state).await),
        Some(other) => unchanged(format!(
            "Unknown workspace subcommand: {other}\nUsage: /workspace {WORKSPACE_SUBCOMMAND_USAGE}"
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
async fn ws_new(state: &std::sync::Arc<AppState>, args: &[&str]) -> String {
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

    match state.create_workspace_owned(name, kind, None).await {
        Ok((workspace, _created)) => format!(
            "Created workspace '{}' ({})\n  Root: {}\n  Kind: {}",
            workspace.name,
            workspace.id,
            workspace.root.display(),
            workspace.kind.display_name()
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
async fn ws_switch(state: &std::sync::Arc<AppState>, name: Option<&str>) -> WorkspaceCommandResult {
    let name = match name {
        Some(n) => n,
        None => {
            return unchanged("Usage: /workspace switch <name>".to_string());
        }
    };
    match state.workspace.registry.open_by_name(name) {
        Ok(ws) => {
            let transition = match state.switch_workspace_registered(ws.id.clone()).await {
                Ok(transition) => transition,
                Err(error) => return unchanged(format!("Failed to switch workspace: {error}")),
            };
            let ws = state.current_workspace().await.unwrap_or(ws);
            WorkspaceCommandResult {
                output: format!(
                    "Switched to workspace: {} ({})\n  Root: {}\n  Kind: {}{}{}",
                    ws.name,
                    ws.id,
                    ws.root.display(),
                    ws.kind.display_name(),
                    ws.project_root
                        .as_ref()
                        .map(|path| format!("\n  Project: {}", path.display()))
                        .unwrap_or_default(),
                    transition_warning(&transition),
                ),
                generation_changed: true,
            }
        }
        Err(error) => unchanged(format!("Failed to switch workspace: {error}")),
    }
}

async fn ws_exit(state: &std::sync::Arc<AppState>) -> WorkspaceCommandResult {
    let transition = match state.exit_workspace().await {
        Ok(transition) => transition,
        Err(error) => return unchanged(format!("Failed to exit workspace: {error}")),
    };
    WorkspaceCommandResult {
        output: format!(
            "Exited workspace; using global paths.{}",
            transition_warning(&transition)
        ),
        generation_changed: true,
    }
}

fn transition_warning(
    transition: &echo_agent_app_core::api::state::WorkspaceTransitionReceipt,
) -> String {
    if transition.status != echo_agent_app_core::api::state::WorkspaceTransitionStatus::Degraded {
        return String::new();
    }
    let details = transition
        .degraded_subsystems
        .iter()
        .map(|subsystem| format!("{}: {}", subsystem.subsystem, subsystem.error))
        .collect::<Vec<_>>()
        .join("; ");
    format!("\n  Warning: workspace committed with degraded subsystems: {details}")
}

/// `/workspace link <path>`
async fn ws_link(state: &std::sync::Arc<AppState>, path: Option<&str>) -> String {
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
    match state
        .link_current_workspace_project_owned(project_path)
        .await
    {
        Ok(ws) => {
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
    "Manage workspaces (new/list/switch/exit/link/info)",
    cmd_workspace
);

// ── Register ─────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(std::sync::Arc::new(WorkspaceCommand));
}

#[cfg(test)]
mod tests {
    use super::WORKSPACE_SUBCOMMAND_USAGE;

    #[test]
    fn workspace_usage_has_no_legacy_migration_surface() {
        assert_eq!(
            WORKSPACE_SUBCOMMAND_USAGE,
            "[new|list|switch|exit|link|info] [args]"
        );
        assert!(!WORKSPACE_SUBCOMMAND_USAGE.contains("migrate"));
    }
}
