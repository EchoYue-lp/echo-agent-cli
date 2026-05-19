//! CLI 命令行参数定义
//!
//! 使用 clap derive 模式定义所有命令行选项和子命令。

use clap::Parser;

/// Echo Agent CLI - AI Agent 命令行与 Web 服务
#[derive(Parser, Debug)]
#[command(name = "echo-agent-cli")]
#[command(version = "1.0.0")]
#[command(about = "AI Agent 命令行与 Web 服务 — 对标业界主流通用 Agent", long_about = None)]
pub struct Args {
    /// 启动 Web 服务
    #[arg(long, default_value_t = false)]
    pub web: bool,

    /// 启动命令行交互
    #[arg(long, short = 'i', default_value_t = false)]
    pub cli: bool,

    /// Web 服务端口
    #[arg(long, short = 'p', default_value = "3000")]
    pub port: u16,

    /// Web 服务地址
    #[arg(long, default_value = "0.0.0.0")]
    pub host: String,

    /// 模型名称（不指定则使用配置文件中的值）
    #[arg(long, short = 'm', env = "MODEL_NAME")]
    pub model: Option<String>,

    /// 系统提示词（不指定则使用配置文件中的值）
    #[arg(long, short = 's', env = "SYSTEM_PROMPT")]
    pub system_prompt: Option<String>,

    /// Agent 模式 (general, coding, research, data, writing)
    #[arg(long, default_value = "general")]
    pub mode: String,

    /// 项目目录（自动加载 AGENTS.md 等项目指令）
    #[arg(long)]
    pub project: Option<String>,

    /// MCP 配置文件路径
    #[arg(long, env = "MCP_CONFIG_PATH")]
    pub mcp_config: Option<String>,

    /// 配置文件路径 (echo-agent.yaml)
    #[arg(long)]
    pub config: Option<String>,

    /// 禁用彩色输出
    #[arg(long)]
    pub no_color: bool,

    /// 启用 IM 通道模式（QQ Bot、飞书等），需启用 channels feature
    #[arg(long)]
    pub channels: bool,

    /// 启动终端 UI (TUI) 模式
    #[arg(long)]
    pub tui: bool,

    /// 输出格式 (text, json, markdown, table)
    #[arg(long, short = 'o', default_value = "text")]
    pub output: String,

    /// 详细输出模式
    #[arg(long, short = 'v')]
    pub verbose: bool,

    /// 子命令
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// CLI 子命令
#[derive(Debug, clap::Subcommand)]
pub enum Commands {
    /// 一次性对话 (从参数或 stdin 读取)
    Run {
        /// 用户消息 (不指定则从 stdin 读取)
        message: Vec<String>,
        /// 从 stdin 读取 (管道模式)
        #[arg(long)]
        pipe: bool,
        /// 模型名称（不指定则使用配置文件中的值）
        #[arg(long, short = 'm')]
        model: Option<String>,
        /// 输出格式 (text, json, markdown)
        #[arg(long, short = 'o', default_value = "text")]
        output: String,
    },
    /// 管理配置档案
    Profiles {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// 管理会话
    Sessions {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// 生成 Shell 补全脚本
    Completions {
        /// Shell 类型 (bash, zsh, fish, elvish, powershell)
        shell: String,
        /// 同时生成所有 Shell 的补全
        #[arg(long)]
        all: bool,
    },
    /// 启动终端 UI 模式
    Tui,
    /// 交互式引导配置 (Onboarding Wizard)
    Onboard,
    /// 诊断配置问题
    Doctor,
}

/// 档案子命令
#[derive(Debug, clap::Subcommand)]
pub enum ProfileAction {
    /// 列出所有档案
    List,
    /// 查看档案详情
    Show { name: String },
    /// 创建新档案
    Create {
        name: String,
        #[arg(long, short = 'm')]
        model: Option<String>,
        #[arg(long, short = 's')]
        system_prompt: Option<String>,
    },
    /// 更新档案
    Update {
        name: String,
        #[arg(long, short = 'm')]
        model: Option<String>,
        #[arg(long, short = 's')]
        system_prompt: Option<String>,
        #[arg(long)]
        theme: Option<String>,
    },
    /// 激活档案
    Use { name: String },
    /// 删除档案
    Delete { name: String },
}

/// 会话子命令
#[derive(Debug, clap::Subcommand)]
pub enum SessionAction {
    /// 列出所有会话
    List,
    /// 查看会话详情
    Show { id: String },
    /// 从现有会话创建分支
    Branch {
        parent_id: String,
        branch_name: String,
    },
    /// 对比两个会话
    Diff {
        id_a: String,
        id_b: String,
    },
    /// 导出会话
    Export {
        id: String,
        #[arg(long, short = 'f', default_value = "json")]
        format: String,
        #[arg(long, short = 'o')]
        output: Option<String>,
    },
    /// 删除会话
    Delete { id: String },
}
