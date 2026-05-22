//! 上下文管理命令

use super::super::state::TauriState;
use tauri::State;

#[tauri::command]
pub async fn get_context(state: State<'_, TauriState>) -> Result<serde_json::Value, String> {
    let guard = state.agent.inner().read().await;
    let messages = guard.get_messages().await;
    let token_est: usize = messages
        .iter()
        .map(|m| m.content.as_deref().unwrap_or("").len() / 4)
        .sum();
    Ok(serde_json::json!({
        "message_count": messages.len(),
        "estimated_tokens": token_est,
    }))
}

#[tauri::command]
pub async fn compress_context(
    state: State<'_, TauriState>,
    keep_recent: usize,
) -> Result<serde_json::Value, String> {
    let guard = state.agent.inner().read().await;
    let messages = guard.get_messages().await;
    let before_count = messages.len();
    let before_tokens: usize = messages
        .iter()
        .map(|m| m.content.as_deref().unwrap_or("").len() / 4)
        .sum();

    let after_count = if messages.len() > keep_recent {
        keep_recent
    } else {
        messages.len()
    };
    let after_tokens: usize = messages
        .iter()
        .rev()
        .take(keep_recent)
        .map(|m| m.content.as_deref().unwrap_or("").len() / 4)
        .sum();

    Ok(serde_json::json!({
        "before_count": before_count,
        "after_count": after_count,
        "before_tokens": before_tokens,
        "after_tokens": after_tokens,
        "evicted": before_count.saturating_sub(after_count),
    }))
}
