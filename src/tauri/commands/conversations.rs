//! 对话历史管理命令

use tauri::State;
use super::super::state::TauriState;

#[tauri::command]
pub async fn list_conversations(state: State<'_, TauriState>) -> Result<Vec<serde_json::Value>, String> {
    let dir = state.persistence.conversations_dir();
    let mut conversations = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) {
                        conversations.push(val);
                    }
                }
            }
        }
    }
    Ok(conversations)
}

#[tauri::command]
pub async fn get_conversation(
    state: State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, String> {
    let path = state.persistence.conversations_dir().join(format!("{}.json", id));
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&data).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_conversation(
    state: State<'_, TauriState>,
    id: String,
) -> Result<(), String> {
    let path = state.persistence.conversations_dir().join(format!("{}.json", id));
    std::fs::remove_file(path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn export_conversation(
    state: State<'_, TauriState>,
    id: String,
) -> Result<String, String> {
    state.persistence.export_conversation_markdown(&id).map_err(|e| e.to_string())
}
