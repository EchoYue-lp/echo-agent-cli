//! Typed Tauri adapter for the application-owned Extension command authority.

use crate::tauri::state::TauriState;
use echo_agent_app_core::api::extension_commands::{
    ExtensionCommand, ExtensionCommandDispatcher, ExtensionCommandIdentity,
    ExtensionCommandReceipt, ExtensionCommandRequest, ExtensionRequestScope,
};

#[tauri::command]
pub async fn execute_extension_command(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    conversation_id: String,
    request: ExtensionCommandRequest,
) -> Result<ExtensionCommandReceipt, String> {
    let scope = ExtensionRequestScope::new(
        workspace_id,
        workspace_generation,
        request
            .scope
            .as_ref()
            .and_then(|scope| scope.sender_id.clone()),
        request
            .scope
            .as_ref()
            .and_then(|scope| scope.sender_incarnation.clone()),
    )
    .map_err(|error| error.to_string())?;
    Ok(dispatch_scoped(
        &state,
        scope,
        &conversation_id,
        request.command,
        Some(ExtensionCommandIdentity {
            request_id: request.request_id,
            operation_id: request.operation_id,
        }),
    )
    .await)
}

pub(crate) async fn dispatch_scoped(
    state: &TauriState,
    scope: ExtensionRequestScope,
    conversation_id: &str,
    command: ExtensionCommand,
    identity: Option<ExtensionCommandIdentity>,
) -> ExtensionCommandReceipt {
    let identity = identity.unwrap_or_else(ExtensionCommandIdentity::random);
    ExtensionCommandDispatcher::new(state.app_state.clone())
        .dispatch_for_scope(
            scope.clone(),
            ExtensionCommandRequest {
                request_id: identity.request_id,
                operation_id: identity.operation_id,
                scope: Some(scope),
                command,
            },
            conversation_id.to_string(),
        )
        .await
}
