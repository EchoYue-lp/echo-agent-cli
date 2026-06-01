# Echo Agent CLI

> 一个基于 [echo-agent](https://github.com/EchoYue-lp/echo-agent) 的通用 AI Agent 产品，支持 Coding、数据分析和学术研究三大核心能力。

[![Rust](https://img.shields.io/badge/Rust-1.95%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## 📋 项目简介

Echo Agent CLI 是一个生产级的通用 Agent 产品，基于 Rust 生态构建，提供 **TUI（终端界面）** 和 **GUI（桌面应用）** 两种交互模式，专注于以下核心场景：

- **💻 Coding** — 代码生成、审查、重构、调试、测试
- **📊 数据分析** — 结构化数据分析、统计、可视化、报告生成
- **📚 学术研究** — arXiv/语义学者检索、论文阅读、学术写作辅助

### 核心特性

- 🤖 **双模式交互**：全屏终端（TUI）、桌面应用（Tauri GUI）
- 🔄 **长程任务支持**：断点续传、进度追踪、人机协作检查点
- 🛡️ **安全优先**：JWT 认证、速率限制、审计日志
- 🧩 **可扩展架构**：MCP 服务器、插件系统、技能管理
- 🎨 **现代化 GUI**：React + Tailwind CSS + WebSocket 实时通信

---

## 🏗️ 项目结构

```
echo-agent-cli/
├── Cargo.toml                    # Rust 工作区配置
├── src/                          # TUI 入口
│   ├── main.rs                   # 主入口（默认启动 TUI）
│   └── tui/                      # 终端 UI
│       ├── mod.rs                # TUI 主循环
│       ├── events.rs             # 事件处理（状态机模式）
│       ├── markdown.rs           # Markdown 渲染（表格/任务列表）
│       ├── widgets/              # UI 组件
│       └── keymap.rs             # 键盘映射
├── echo-agent-app-core/          # 核心应用库
│   ├── src/
│   │   ├── state.rs              # 应用状态管理
│   │   ├── agent_handle.rs       # Agent 流式输出封装
│   │   ├── security/             # JWT/认证/权限
│   │   ├── tasks/                # 后台任务系统
│   │   ├── hitl/                 # 人机协作循环
│   │   ├── workspace/            # 工作区管理
│   │   └── output/               # 输出渲染（Markdown/表格/主题）
│   └── Cargo.toml
├── src-tauri/                    # Tauri 桌面应用
│   └── src/main.rs
└── web-frontend/                 # GUI 前端（React + Tailwind）
    ├── src/
    │   ├── App.tsx               # 主应用
    │   ├── components/           # React 组件
    │   ├── stores/               # Zustand 状态管理
    │   └── hooks/                # 自定义 Hooks
    └── package.json
```

---

## 🚀 快速开始

### 前置条件

- **Rust** >= 1.95（使用 `rustup` 安装）
- **Node.js** >= 18（用于 GUI 前端）

### 安装依赖

```bash
# 进入项目目录
cd echo-agent-cli

# 安装 Rust 依赖
cargo fetch

# 安装前端依赖（GUI 需要）
cd web-frontend
npm install
cd ..
```

---

## 📦 编译

### 编译 TUI（终端全屏界面）

```bash
# 编译 TUI（Debug）
cargo build --bin echo-agent-cli

# 编译 TUI（Release）
cargo build --bin echo-agent-cli --release

# 编译产物路径：
#   Debug:   target/debug/echo-agent-cli
#   Release: target/release/echo-agent-cli
```

### 编译 GUI（桌面应用）

```bash
# 编译 GUI（需要先构建前端）
cd web-frontend && npm run build && cd ..
cargo build --bin echo-agent-tauri --release

# 编译产物路径：
#   macOS:   target/release/echo-agent-tauri
#   Linux:   target/release/echo-agent-tauri
#   Windows: target/release/echo-agent-tauri.exe
```

### 安装到系统 PATH

#### TUI — 命令行快捷进入

```bash
# 方式一：直接运行（不安装）
cargo run --bin echo-agent-cli

# 方式二：安装到 ~/.cargo/bin（推荐，可全局调用）
cargo install --path .
# 安装后可直接运行：
echo-agent-cli
```

#### 创建快捷命令（像 `claude` 一样快捷进入）

安装后，建议创建一个短命令别名方便日常使用：

```bash
# Bash / Zsh（添加到 ~/.bashrc 或 ~/.zshrc）
alias ecw='echo-agent-cli'
alias echocowork='echo-agent-cli'

# Fish（添加到 ~/.config/fish/config.fish）
alias ecw='echo-agent-cli'
alias echocowork='echo-agent-cli'

# 重新加载配置
source ~/.zshrc  # 或 source ~/.bashrc
```

现在可以像使用 `claude` 一样直接输入：

```bash
ecw          # 快捷进入 TUI
echocowork   # 完整命令名
```

#### GUI — 桌面应用

```bash
# 方式一：直接运行（不安装）
cargo run --bin echo-agent-tauri

# 方式二：安装到系统（macOS 示例）
cargo install --path .
# 然后复制到 Applications
sudo cp target/release/echo-agent-tauri /Applications/EchoCoWork.app/Contents/MacOS/echocowork
```

---

## 🖥️ 使用指南

### TUI 快捷键

| 快捷键 | 功能 |
|--------|------|
| `Ctrl+C` / `Ctrl+Q` | 退出应用 |
| `Ctrl+B` | 切换侧边栏 |
| `Ctrl+L` | 清空聊天 |
| `Shift+Enter` | 输入换行 |
| `Enter` | 发送消息 |
| `Esc` | 取消生成 / 关闭弹窗 |
| `Tab` | 切换侧边栏标签 |
| `↑/↓` | 浏览输入历史 |
| `Shift+↑/↓` | 滚动聊天 |
| `PageUp/PageDown` | 快速滚动 |

### Slash 命令

在输入框输入 `/` 可查看可用命令：

| 命令 | 描述 |
|------|------|
| `/help` | 显示帮助信息 |
| `/mode <mode>` | 切换 Agent 模式 |
| `/model <name>` | 切换模型 |
| `/reset` | 重置会话 |
| `/stats` | 显示会话统计 |
| `/status` | 显示当前状态 |
| `/cost` | 显示 Token 用量 |
| `/tools` | 显示可用工具 |
| `/quit` / `/exit` | 退出应用 |

---

## 🧪 开发

### 代码检查

```bash
# Rust 编译检查
cargo check --workspace

# Clippy 检查
cargo clippy --workspace

# 运行测试
cargo test --workspace
```

### GUI 前端开发

`web-frontend/` 目录包含 GUI 的前端代码（React + Tailwind CSS），由 Tauri 桌面应用嵌入：

```bash
cd web-frontend

# 开发服务器
npm run dev

# 生产构建
npm run build

# TypeScript 检查
npx tsc -b
```

---

## 🏗️ 架构说明

### Agent Streaming

使用 `tokio::sync::mpsc::unbounded_channel` 实现真正的增量式流式输出：

- 后台任务获取 `RwLock<ReactAgent>` 并调用 `execute_stream()`/`chat_stream()`
- 逐事件通过 channel 发送
- 返回基于 channel 接收端的流，避免返回时持有锁

### 主题系统

统一 TUI 的 `ColorTheme` 和 GUI 的主题：

- `ColorTheme` 提供 6 种内置主题（dark、light、monokai、solarized、dracula、one-dark）
- TUI 通过 `Theme::from_color_theme()` 从 `ColorTheme` 生成
- GUI 使用 CSS 变量支持亮色/暗色主题切换
- 支持运行时切换主题

### 后台任务

支持多种任务类型：

- `AgentChat` — 单次对话
- `Cron` — 定时任务
- `Workflow` — 工作流编排
- `Research` — 学术研究流水线
- `DataPipeline` — 数据处理流水线
- `Composite` — 组合任务（支持并行/顺序执行）

### 安全机制

- **JWT 认证**：自动生成密钥、bcrypt 密码哈希、令牌过期
- **速率限制**：基于 governor 的直接模式（避免 keyed 内存泄漏）
- **审计日志**：FIFO 淘汰的审计日志
- **权限控制**：工具级别的细粒度权限

---

## 📁 工作区

工作区存储在 `~/.echo-agent/workspaces/` 下，包含：

```
workspaces/
├── {workspace-id}/
│   ├── .echocowork/
│   │   ├── sessions/         # 会话历史
│   │   ├── conversations/    # 对话记录
│   │   ├── memory/            # 记忆存储
│   │   ├── tasks/             # 任务状态
│   │   ├── traces/            # 执行轨迹
│   │   ├── logs/              # 日志
│   │   └── workspace.json     # 工作区清单
│   ├── data/                  # 数据文件
│   ├── papers/                # 论文文件
│   ├── artifacts/             # 生成物
│   └── scratchpad.md          # 共享草稿
```

---

## 📝 贡献指南

1. Fork 仓库
2. 创建功能分支：`git checkout -b feature/your-feature`
3. 提交代码：`git commit -m "Add some feature"`
4. 推送到分支：`git push origin feature/your-feature`
5. 创建 Pull Request

### 代码规范

- 使用 `cargo clippy` 检查代码
- 所有功能需通过 `cargo test` 测试
- 遵循 Rust 命名约定和代码风格

---

## 📄 许可证

MIT License — 详见 [LICENSE](LICENSE) 文件。

---

## 🤝 致谢

- [echo-agent](https://github.com/EchoYue-lp/echo-agent) — 底层 AI Agent 框架
- [ratatui](https://github.com/ratatui-org/ratatui) — 终端 UI 库
- [Tauri](https://tauri.app/) — 桌面应用框架
- [React](https://react.dev/) + [Vite](https://vitejs.dev/) + [Tailwind CSS](https://tailwindcss.com/) — Web 前端技术栈
