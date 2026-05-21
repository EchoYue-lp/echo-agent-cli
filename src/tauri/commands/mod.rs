//! Tauri IPC 命令模块

pub mod chat;
pub mod config;
pub mod context;
pub mod conversations;
pub mod mcp;
pub mod permissions;
pub mod skills;
pub mod tools;

pub fn register_commands(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    builder
        .invoke_handler(tauri::generate_handler![
            chat::chat_stream,
            chat::cancel_chat,
            config::get_config,
            config::update_config,
            context::get_context,
            context::compress_context,
            conversations::list_conversations,
            conversations::get_conversation,
            conversations::delete_conversation,
            conversations::export_conversation,
            mcp::list_mcp_servers,
            mcp::connect_mcp_server,
            mcp::disconnect_mcp_server,
            permissions::get_permission_status,
            skills::list_skills,
            skills::load_skills,
            tools::list_tools,
        ])
}
