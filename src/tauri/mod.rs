//! Tauri desktop app module.
//!
//! The Tauri builder is configured here with native plugins and IPC commands.
//! Critical paths (file I/O, notifications) use IPC for low latency.
//! Everything else goes through the embedded Axum HTTP server.

pub mod ipc;

pub fn build_tauri_app() -> tauri::Builder<tauri::Wry> {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            ipc::native_read_file,
            ipc::native_write_file,
            ipc::native_notify,
            ipc::get_system_info,
        ])
        .setup(|app| {
            // Register global shortcut: CmdOrCtrl+Shift+E toggles window visibility
            use tauri::Manager;
            use tauri_plugin_global_shortcut::GlobalShortcutExt;

            let handle = app.handle().clone();
            app.global_shortcut()
                .on_shortcut("CmdOrCtrl+Shift+E", move |_app, _window, shortcut| {
                    if shortcut.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        if let Some(window) = handle.get_webview_window("main") {
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                    }
                })
                .ok();

            Ok(())
        })
}
