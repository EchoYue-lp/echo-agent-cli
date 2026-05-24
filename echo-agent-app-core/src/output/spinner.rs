//! 进度动画 (Spinner)
//!
//! 基于 `indicatif` 的终端进度指示器。
//! 用于表示异步操作正在进行中（如等待 LLM 响应、工具调用执行）。

use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Arc;
use std::time::Duration;

/// Spinner 句柄 — 持有 ProgressBar，Drop 时自动清除
pub struct SpinnerHandle {
    bar: Option<Arc<ProgressBar>>,
}

impl SpinnerHandle {
    /// 创建并启动一个 spinner
    pub fn new(message: &str) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("{spinner:.green} {msg}")
                .unwrap()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(Duration::from_millis(80));

        // Move cursor to new line so spinner doesn't interfere with input
        println!();

        SpinnerHandle {
            bar: Some(Arc::new(bar)),
        }
    }

    /// 更新 spinner 消息
    pub fn set_message(&self, message: &str) {
        if let Some(ref bar) = self.bar {
            bar.set_message(message.to_string());
        }
    }

    /// 停止并清除 spinner
    pub fn finish(&mut self, message: &str) {
        if let Some(ref bar) = self.bar.take() {
            bar.finish_with_message(message.to_string());
        }
    }

    /// 停止 spinner 并显示清除后的状态
    pub fn finish_and_clear(&mut self) {
        if let Some(ref bar) = self.bar.take() {
            bar.finish_and_clear();
        }
    }

    /// 停止 spinner 并显示成功标记
    pub fn finish_success(&mut self, message: &str) {
        if let Some(ref bar) = self.bar.take() {
            bar.finish_with_message(format!("✅ {}", message));
        }
    }

    /// 停止 spinner 并显示失败标记
    pub fn finish_error(&mut self, message: &str) {
        if let Some(ref bar) = self.bar.take() {
            bar.finish_with_message(format!("❌ {}", message));
        }
    }
}

impl Drop for SpinnerHandle {
    fn drop(&mut self) {
        self.finish_and_clear();
    }
}
