//! Thin desktop controls for the single application channel runtime.
//! The frontend sends snake_case argument keys; make this explicit instead of
//! Tauri's default camelCase. This affects top-level IPC arguments, not patch serde fields.
use crate::tauri::{error::IpcError, state::TauriState};
use echo_agent_app_core::api::channel_runtime::{
    ChannelConfigurationPatch, ChannelControlAction, ManagedChannel, WeixinQrStatus,
};

#[tauri::command(rename_all = "snake_case")]
pub async fn get_channel_configuration(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    state
        .channel_runtime
        .configuration()
        .await
        .map_err(IpcError::Internal)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn begin_weixin_login(
    state: tauri::State<'_, TauriState>,
    expected_revision: String,
) -> Result<WeixinQrStatus, IpcError> {
    state
        .channel_runtime
        .weixin_begin(expected_revision)
        .await
        .map_err(IpcError::Internal)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_weixin_login_status(
    state: tauri::State<'_, TauriState>,
    challenge_id: Option<String>,
) -> Result<WeixinQrStatus, IpcError> {
    state
        .channel_runtime
        .weixin_status(challenge_id)
        .await
        .map_err(IpcError::Internal)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn verify_weixin_login(
    state: tauri::State<'_, TauriState>,
    challenge_id: String,
    code: String,
) -> Result<WeixinQrStatus, IpcError> {
    state
        .channel_runtime
        .weixin_verify(challenge_id, code)
        .await
        .map_err(IpcError::Internal)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn cancel_weixin_login(
    state: tauri::State<'_, TauriState>,
    challenge_id: String,
) -> Result<WeixinQrStatus, IpcError> {
    state
        .channel_runtime
        .weixin_cancel(challenge_id)
        .await
        .map_err(IpcError::Internal)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn forget_weixin_login(
    state: tauri::State<'_, TauriState>,
    expected_revision: String,
) -> Result<(), IpcError> {
    state
        .channel_runtime
        .weixin_forget(expected_revision)
        .await
        .map_err(IpcError::Internal)
}
#[tauri::command(rename_all = "snake_case")]
pub async fn save_channel_configuration(
    state: tauri::State<'_, TauriState>,
    patch: ChannelConfigurationPatch,
) -> Result<serde_json::Value, IpcError> {
    state
        .channel_runtime
        .save_configuration(patch)
        .await
        .map_err(IpcError::Internal)
}
#[tauri::command(rename_all = "snake_case")]
pub async fn list_channel_status(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    state
        .channel_runtime
        .status()
        .await
        .map_err(IpcError::Internal)
}
#[tauri::command(rename_all = "snake_case")]
pub async fn control_channel(
    state: tauri::State<'_, TauriState>,
    channel: ManagedChannel,
    expected_revision: String,
    expected_mutation_epoch: Option<String>,
    expected_applied_revision: Option<String>,
    action: ChannelControlAction,
) -> Result<serde_json::Value, IpcError> {
    state
        .channel_runtime
        .control(
            channel,
            expected_revision,
            expected_mutation_epoch,
            expected_applied_revision,
            action,
        )
        .await
        .map_err(IpcError::Internal)
}

/// Notification configuration belongs to EKO, not the framework CronTask.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_weixin_notification_configuration(
    state: tauri::State<'_, TauriState>,
    definition_id: String,
) -> Result<serde_json::Value, IpcError> {
    let (config, revision) = state.app_state.config.configuration_snapshot().await;
    let destination = config.channels.weixin.notifications.get(&definition_id);
    Ok(
        serde_json::json!({"enabled": destination.is_some(), "revision": revision,
        "account_id": destination.map(|value| value.account_id.clone())}),
    )
}

#[tauri::command(rename_all = "snake_case")]
pub async fn configure_weixin_notification(
    state: tauri::State<'_, TauriState>,
    task_id: String,
    definition_id: String,
    expected_revision: String,
    enabled: bool,
) -> Result<serde_json::Value, IpcError> {
    let runner = state
        .app_state
        .scheduler
        .runner
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Scheduler is unavailable".to_string()))?;
    let task = runner
        .list_tasks()
        .await
        .into_iter()
        .find(|task| task.id == task_id)
        .ok_or_else(|| IpcError::NotFound("Scheduled task no longer exists".to_string()))?;
    if task.definition_id != definition_id || definition_id.trim().is_empty() {
        return Err(IpcError::Validation(
            "Scheduled definition changed; reload before saving".to_string(),
        ));
    }
    let destination = if enabled {
        Some(
            state
                .channel_runtime
                .capture_weixin_notification(&definition_id, &expected_revision)
                .await
                .map_err(IpcError::Validation)?,
        )
    } else {
        None
    };
    let key = definition_id.clone();
    let receipt = state
        .app_state
        .update_channels_config_owned(expected_revision, move |channels| {
            if let Some(destination) = destination {
                if !channels.weixin.account.as_ref().is_some_and(|account| {
                    account.account_id == destination.account_id
                        && account.generation == destination.credential_generation
                }) {
                    return Err(
                        "Weixin authorization changed; select the destination again".to_string()
                    );
                }
                channels.weixin.notifications.insert(key, destination);
            } else {
                channels.weixin.notifications.remove(&key);
            }
            Ok(())
        })
        .await
        .map_err(|_| {
            IpcError::Validation(
                "Unable to save notification; reload the original configuration".to_string(),
            )
        })?;
    Ok(serde_json::json!({"enabled":enabled,"revision":receipt.configuration_revision}))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_weixin_notification_result(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = state
        .app_state
        .tasks
        .runtime
        .as_ref()
        .ok_or_else(|| IpcError::Internal("TaskRuntime is unavailable".to_string()))?;
    let status = store.weixin_notification_status(&run_id).map_err(|_| {
        IpcError::Internal("Unable to read original notification receipt".to_string())
    })?;
    Ok(match status {
        Some(record) => {
            serde_json::json!({"configured":true,"run_id":run_id,"phase":record.phase.as_str(),
            "outcome":record.outcome.map(|outcome| outcome.as_str()),"reason":record.reason})
        }
        None => {
            let configured = store
                .frozen_weixin_notification(&run_id)
                .map_err(|_| {
                    IpcError::Internal("Unable to read original notification binding".to_string())
                })?
                .is_some();
            serde_json::json!({"configured":configured,"run_id":run_id,"phase":if configured {Some("awaiting_result")} else {None}})
        }
    })
}
