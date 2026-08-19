//! CLI 命令行参数定义
//!
//! 使用 clap derive 模式定义 TUI/GUI 产品入口参数。

use clap::Parser;

/// EKO - TUI/GUI 通用 Agent
#[derive(Parser, Debug)]
#[command(name = "echo-agent-cli")]
#[command(version = "1.0.0")]
#[command(about = "EKO TUI/GUI 通用 Agent", long_about = None)]
pub struct Args {
    /// 启动全屏 TUI 交互（默认）
    #[arg(long, short = 't', default_value_t = false)]
    pub tui: bool,

    /// Render without the alternate screen so terminal scrollback is preserved.
    #[arg(long, default_value_t = false)]
    pub no_alt_screen: bool,

    /// 内部 Web 服务入口（仅供 GUI/调试使用；默认隐藏）
    #[arg(long, default_value_t = false, hide = true)]
    pub web: bool,

    /// 旧 REPL 入口（默认隐藏）
    #[arg(long, short = 'i', default_value_t = false, hide = true)]
    pub cli: bool,

    /// Run one non-interactive turn and emit canonical chat envelopes as JSONL.
    #[arg(
        long,
        value_name = "PROMPT",
        conflicts_with_all = ["tui", "web", "cli", "channels"]
    )]
    pub jsonl: Option<String>,

    /// Web 服务端口（仅内部 Web/GUI 使用）
    #[arg(long, short = 'p', default_value = "3000", hide = true)]
    pub port: u16,

    /// Web 服务地址（仅内部 Web/GUI 使用）
    #[arg(long, default_value = "127.0.0.1", hide = true)]
    pub host: String,

    /// 已配置模型 ID 或唯一模型名称（不指定则使用默认模型）
    #[arg(long, short = 'm', env = "MODEL_NAME")]
    pub model: Option<String>,

    /// 项目目录（自动加载 AGENTS.md 等项目指令）
    #[arg(long)]
    pub project: Option<String>,

    /// MCP 配置文件路径
    #[arg(long)]
    pub mcp_config: Option<String>,

    /// 配置文件路径 (echo-agent.yaml)
    #[arg(long)]
    pub config: Option<String>,

    /// 启用 IM 通道模式（内部/实验入口，默认隐藏）
    #[arg(long, hide = true)]
    pub channels: bool,

    /// 继续最近一次会话 (resume latest session)
    #[arg(long, short = 'c', default_value_t = false)]
    pub r#continue: bool,

    /// 恢复指定会话 ID (resume a specific session)
    #[arg(long, short = 'r', value_name = "SESSION_ID")]
    pub resume: Option<String>,

    /// 详细输出模式
    #[arg(long, short = 'v')]
    pub verbose: bool,
}
