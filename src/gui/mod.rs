//! egui 原生桌面 GUI
//!
//! 通过 `--gui` 标志启动。进程内直接调用 agent，无需 HTTP/WebSocket。

mod app;
mod human_loop;
mod message;
mod render;
mod settings;
mod syntax;
mod theme;

pub use app::EchoGuiApp;
pub use human_loop::GuiHumanLoopHandler;

/// 启动 GUI 模式
pub fn run_gui(agent_handle: crate::agent_handle::AgentHandle) -> anyhow::Result<()> {
    let persistence = crate::persistence::Persistence::new();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("Echo Agent"),
        ..Default::default()
    };

    eframe::run_native(
        "Echo Agent",
        options,
        Box::new(|cc| {
            theme::setup(&cc.egui_ctx, true);
            Ok(Box::new(EchoGuiApp::new(cc, agent_handle, persistence)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {}", e))
}