//! Cross-workspace Agent address discovery and durable message acceptance.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::api::agent_router::{AgentAddress, AgentGroupMember, AgentMessage};
use echo_agent_app_core::api::workspace::WorkspaceId;

fn delivery_status_response(
    records: Vec<echo_agent_app_core::api::agent_router::AgentDeliveryRecord>,
) -> serde_json::Value {
    serde_json::json!({
        "count": records.len(),
        "records": records,
    })
}

#[tauri::command]
pub async fn list_agent_endpoints(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let endpoints = state
        .app_state
        .discover_agent_endpoints()
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    Ok(serde_json::json!({
        "endpoints": endpoints,
        "count": endpoints.len(),
    }))
}

#[tauri::command]
pub async fn get_agent_delivery_status(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
    message_id: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let target = AgentAddress::new(WorkspaceId::from_raw(workspace_id), conversation_id);
    let records = state
        .app_state
        .agent_delivery_records(&target)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?
        .into_iter()
        .filter(|record| {
            message_id
                .as_deref()
                .is_none_or(|id| record.message_id == id)
        })
        .collect::<Vec<_>>();
    Ok(delivery_status_response(records))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn agent_delivery_wire_uses_only_canonical_receipt_vocabulary() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router =
            echo_agent_app_core::api::agent_router::AgentRouter::new(root.path().to_path_buf());
        let target = AgentAddress::new(WorkspaceId::from_name("wire-target"), "wire-conversation");
        let receipt = router
            .enqueue(AgentMessage::user_text(None, target.clone(), "persist me"))
            .await
            .map_err(|error| error.to_string())?;
        let receipt_json = serde_json::to_value(receipt).map_err(|error| error.to_string())?;
        assert_eq!(
            receipt_json.get("phase"),
            Some(&serde_json::json!("persisted"))
        );
        assert_eq!(receipt_json.get("drained"), Some(&serde_json::json!(false)));
        assert!(receipt_json.get("status").is_none());
        assert!(receipt_json.get("accepted_at").is_none());

        let response = delivery_status_response(
            router
                .records(&target)
                .await
                .map_err(|error| error.to_string())?,
        );
        let record = response
            .get("records")
            .and_then(serde_json::Value::as_array)
            .and_then(|records| records.first())
            .ok_or_else(|| "canonical Agent delivery record is missing".to_string())?;
        assert_eq!(record.get("phase"), Some(&serde_json::json!("persisted")));
        assert!(record.get("status").is_none());
        assert!(record.get("settled_at").is_none());
        assert!(record.get("error").is_none());
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn send_agent_message(
    state: tauri::State<'_, TauriState>,
    to_workspace_id: String,
    to_conversation_id: String,
    text: String,
    from_workspace_id: Option<String>,
    from_conversation_id: Option<String>,
    message_id: Option<String>,
    correlation_id: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let from = match (from_workspace_id, from_conversation_id) {
        (Some(workspace_id), Some(conversation_id)) => Some(AgentAddress::new(
            WorkspaceId::from_raw(workspace_id),
            conversation_id,
        )),
        (None, None) => None,
        _ => {
            return Err(IpcError::Validation(
                "source workspace and conversation must be provided together".to_string(),
            ));
        }
    };
    let target = AgentAddress::new(WorkspaceId::from_raw(to_workspace_id), to_conversation_id);
    let mut message = AgentMessage::user_text(from, target, text);
    if let Some(message_id) = message_id {
        message.message_id = message_id;
    }
    message.correlation_id = correlation_id;
    let receipt = state
        .app_state
        .send_agent_message_owned(message)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    Ok(serde_json::json!({
        "success": true,
        "receipt": receipt,
    }))
}

#[tauri::command]
pub async fn list_agent_groups(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let groups = state
        .app_state
        .list_agent_groups()
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    Ok(serde_json::json!({
        "groups": groups,
        "count": groups.len(),
    }))
}

#[tauri::command]
pub async fn create_agent_group(
    state: tauri::State<'_, TauriState>,
    name: String,
    leader: AgentAddress,
    members: Vec<AgentGroupMember>,
) -> Result<serde_json::Value, IpcError> {
    let group = state
        .app_state
        .create_agent_group(name, leader, members)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    Ok(serde_json::json!({ "success": true, "group": group }))
}

#[tauri::command]
pub async fn update_agent_group(
    state: tauri::State<'_, TauriState>,
    group_id: String,
    name: String,
    leader: AgentAddress,
    members: Vec<AgentGroupMember>,
) -> Result<serde_json::Value, IpcError> {
    let group = state
        .app_state
        .update_agent_group(group_id, name, leader, members)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    Ok(serde_json::json!({ "success": true, "group": group }))
}

#[tauri::command]
pub async fn delete_agent_group(
    state: tauri::State<'_, TauriState>,
    group_id: String,
) -> Result<serde_json::Value, IpcError> {
    let deleted = state
        .app_state
        .delete_agent_group(&group_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    Ok(serde_json::json!({ "success": true, "deleted": deleted }))
}
