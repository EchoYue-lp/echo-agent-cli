//! Workspace-scoped LSP controls shared with terminal surfaces.

use crate::tauri::commands::extensions;
use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::api::extension_commands::{
    ExtensionCommand, ExtensionCommandReceipt, ExtensionRequestScope, LspCommand,
};

#[tauri::command]
pub async fn lsp_control(
    state: tauri::State<'_, TauriState>,
    request_scope: ExtensionRequestScope,
    action: String,
    language: Option<String>,
) -> Result<ExtensionCommandReceipt, IpcError> {
    let command = match action.as_str() {
        "list" => LspCommand::List,
        "status" => LspCommand::Status,
        "start" => LspCommand::Start {
            language: language
                .ok_or_else(|| IpcError::Validation("lsp start requires a language".to_string()))?,
        },
        "stop" => LspCommand::Stop {
            language: language
                .ok_or_else(|| IpcError::Validation("lsp stop requires a language".to_string()))?,
        },
        "restart" => LspCommand::Restart {
            language: language.ok_or_else(|| {
                IpcError::Validation("lsp restart requires a language".to_string())
            })?,
        },
        _ => {
            return Err(IpcError::Validation(format!(
                "unknown LSP action '{action}'"
            )));
        }
    };
    Ok(extensions::dispatch_scoped(
        &state,
        request_scope,
        "tauri-lsp-control",
        ExtensionCommand::Lsp(command),
        None,
    )
    .await)
}
