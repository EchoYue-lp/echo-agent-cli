# EchoCoWork

> 一个基于 [echo-agent](https://github.com/EchoYue-lp/echo-agent) 的通用 AI Agent 产品，支持 Coding、数据分析和学术研究三大核心能力。

[![Rust](https://img.shields.io/badge/Rust-1.95%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## 📋 项目简介

EchoCoWork 是一个生产级的通用 Agent 产品，基于 Rust 生态构建，提供 **TUI（终端界面）** 和 **GUI（桌面应用）** 两种交互模式，专注于以下核心场景：

- **💻 Coding** — 代码生成、审查、重构、调试、测试
- **📊 数据分析** — 结构化数据分析、统计、可视化、报告生成
- **📚 学术研究** — arXiv/语义学者检索、论文阅读、学术写作辅助

### 核心特性

- 🤖 **双模式交互**：全屏终端（TUI）、桌面应用（Tauri GUI）
- 🔄 **长程任务支持**：断点续传、进度追踪、人机协作检查点
- 🧩 **可扩展架构**：MCP 服务器、插件系统（PluginRegistry）、技能管理
- 📡 **IM 通道集成**：支持 QQ Bot、飞书（飞书 webhook/long_poll 模式）
- 🔗 **Hooks 系统**：可配置的事件钩子，支持自定义工作流
- 🎨 **现代化 GUI**：React + Tailwind CSS + WebSocket 实时通信
- 🧠 **统一记忆系统**：User / Project / Local 三层记忆，支持自动提取
- 🔧 **LSP 集成**：诊断、跳转定义、查找引用、悬停提示
- 🔁 **自我进化**：轨迹回放、自我审查、自动改进

---

## 🏗️ 项目结构

```
echo-agent-cli/
├── Cargo.toml              # Rust 工作区配置（v1.0.0, edition 2024）
├── init.sh                 # 初始化脚本
├── config/                 # 配置文件（echo-agent.yaml, mcp.json）
├── docs/                   # 项目文档（架构、配置、入门指南）
├── src/                    # 应用入口
│   ├── main.rs             # TUI 主入口
│   ├── lib.rs              # 库导出
│   ├── cli/                # CLI 参数解析、REPL、Slash 命令（20+ 模块）
│   ├── tui/                # 终端 UI（ratatui，事件驱动架构）
│   ├── tauri/              # Tauri IPC 层（GUI 后端）
│   └── logging/            # 日志 inspector
├── echo-agent-app-core/    # 核心应用库（TUI/GUI 共享）
│   └── src/
│       ├── state.rs        # 应用状态管理
│       ├── agent_handle.rs # Agent 并发封装
│       ├── infra.rs        # Agent 创建、MCP 加载
│       ├── config*.rs      # 配置加载与热重载
│       ├── unified_memory.rs # 统一记忆系统
│       ├── tasks/          # 后台任务、长程任务、流水线
│       ├── hitl/           # 人机协作循环
│       ├── workspace/      # 工作区管理
│       ├── sessions/       # 会话管理（SQLite + FTS）
│       ├── project/        # 项目上下文、编码循环
│       ├── output/         # 输出渲染（Markdown、主题、语法高亮）
│       ├── scheduler/      # 定时任务调度
│       ├── skills_hub/     # 技能市场
│       ├── webhook/        # Webhook 事件回调
│       └── observability/  # Trace 观测
├── src-tauri/              # Tauri 桌面应用入口
└── web-frontend/           # GUI 前端（React + Tailwind）
```

---

## 🚀 快速开始

### 前置条件

- **Rust** >= 1.95（使用 `rustup` 安装）
- **Node.js** >= 18（仅 GUI 桌面应用需要）

### 安装依赖

```bash
# 进入项目目录
cd echo-agent-cli

# 安装 Rust 依赖
cargo fetch

# 安装前端依赖（仅 GUI 需要，TUI 不需要）
cd web-frontend
npm install
cd ..
```

> **提示**：如果只使用 TUI 模式，可以跳过 Node.js 和前端依赖安装。使用 `./init.sh` 会自动处理。

### 配置

设置以下三个环境变量即可使用（写入 `~/.bashrc` 或 `.env` 文件）：

```bash
export ECHOCOWORK_AUTH_TOKEN="your-api-key"
export ECHOCOWORK_BASE_URL="https://api.deepseek.com/v1"
export ECHOCOWORK_MODEL="deepseek-v4-flash"
```

也可以通过 `echo-agent.yaml` 配置文件设置。完整配置参考（含 echo-agent.yaml 和 mcp.json 完整示例，可直接复制）：

- [配置指南](docs/configuration.md)

#### 配置文件位置

EchoCoWork 按以下优先级查找配置文件：

1. 命令行参数: `--config <path>`
2. 当前目录: `./echo-agent.yaml`
3. 项目目录: `./.echo-agent/echo-agent.yaml`
4. 用户目录: `~/.echo-agent/config.yaml`

MCP 配置搜索路径：`./mcp.json` → `./.echo-agent/mcp.json` → `~/.echo-agent/mcp.json`

---

## 📦 编译

项目使用 Feature Flags 分离 TUI 和 GUI 构建。默认启用 `tui` feature。

### 编译 TUI（终端全屏界面）

```bash
# 编译 TUI（Debug）— 默认 feature，无需额外参数
cargo build --bin echo-agent-cli

# 编译 TUI（Release）
cargo build --bin echo-agent-cli --release

# 仅编译 TUI，排除 GUI 依赖（推荐，编译更快）
cargo build --bin echo-agent-cli --no-default-features --features tui

# 编译产物路径：
#   Debug:   target/debug/echo-agent-cli
#   Release: target/release/echo-agent-cli
```

### 编译 GUI（桌面应用）

```bash
# 编译 GUI（需要先构建前端）
cd web-frontend && npm run build && cd ..
cargo build --bin echo-agent-tauri --no-default-features --features gui --release

# 编译产物路径：
#   macOS:   target/release/echo-agent-tauri
#   Linux:   target/release/echo-agent-tauri
#   Windows: target/release/echo-agent-tauri.exe
```

### 同时编译 TUI + GUI（开发调试用）

```bash
cargo build --features "tui,gui"
```

### Feature Flags 说明

| Feature | 描述 | 默认启用 |
|---------|------|----------|
| `tui` | 终端全屏界面（ratatui） | ✅ |
| `gui` | 桌面应用（Tauri） | ❌ |
| `channels` | 多通道支持（IM） | ❌ |
| `telemetry` | 遥测数据收集 | ❌ |
| `devtools` | Tauri 开发者工具 | ❌ |

### echo-agent 依赖 Features

echo-agent-cli 启用以下 echo-agent 框架 features：

| Feature | 描述 |
|---------|------|
| `sqlite` | SQLite 会话存储 |
| `mcp` | MCP 协议支持 |
| `lsp` | LSP 语言服务器集成 |
| `human-loop` | 人机协作循环 |
| `subagent` | 子 Agent 编排 |
| `tasks` | 任务系统 |
| `eval` | 评测框架 |
| `improve` | 自我改进 |

---

## 🖥️ 安装与运行

### TUI — 命令行快捷进入

```bash
# 方式一：直接运行（不安装）
cargo run --bin echo-agent-cli

# 方式二：安装到 ~/.cargo/bin（推荐，可全局调用）
cargo install --path . --bin echo-agent-cli --no-default-features --features tui
# 安装后可直接运行：
echo-agent-cli
```

#### 创建快捷命令

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

### GUI — 桌面应用

```bash
# 方式一：直接运行（不安装）
cargo run --bin echo-agent-tauri --no-default-features --features gui

# 方式二：编译后安装到系统（macOS 示例）
cargo build --bin echo-agent-tauri --no-default-features --features gui --release
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
| `S-Tab` | 补全列表上一项 |
| `↑/↓` | 浏览输入历史 |
| `PageUp/PageDown` | 快速滚动聊天 |
| `y` / `n` | 批准 / 拒绝工具执行（HITL 审批） |

### Slash 命令

在输入框输入 `/` 可查看可用命令（命令面板，支持模糊搜索）：

#### Session 会话管理

| 命令 | 别名 | 描述 |
|------|------|------|
| `/reset` | `r` | 重置对话历史 |
| `/clear` | `cls` | 清屏 |
| `/history` | `hist` | 查看会话历史 |
| `/stats` | `st` | 显示会话统计 |
| `/status` | | 显示 Agent 状态 |
| `/new` | `n` | 创建新会话 |
| `/compact` | `cp` | 压缩上下文窗口 |
| `/undo` | `u` | 撤销上一步操作 |

#### Context 上下文管理

| 命令 | 别名 | 描述 |
|------|------|------|
| `/mode <mode>` | | 切换模式（general/coding/research/data/writing） |
| `/model <name>` | | 切换模型 |
| `/think` | | 切换推理/思考显示 |
| `/reasoning` | | 切换推理过程显示 |
| `/system [prompt]` | `sys` | 查看或设置系统提示词 |
| `/memory` | | 查看记忆内容 |
| `/remember <fact>` | | 保存一条记忆 |
| `/forget <fact>` | | 删除一条记忆 |
| `/compress` | | 手动压缩上下文 |
| `/context` | | 查看上下文信息 |
| `/refresh` | | 刷新项目上下文 |
| `/project` | `proj` | 项目管理 |

#### Coding 编码工具

| 命令 | 别名 | 描述 |
|------|------|------|
| `/plan` | | 进入计划模式（只读分析） |
| `/tasks` | | 查看活跃任务 |
| `/task-progress` | `tp` | 查看任务进度 |
| `/task-tree` | `tt` | 查看任务树 |
| `/test [name]` | | 运行测试 |
| `/code-review [path]` | `cr` | 请求代码审查 |
| `/fix` | | 自动修复问题 |
| `/diff [file]` | | 查看 git 或文件差异 |
| `/agents` | | 列出可用 Agent |
| `/agent` | | Agent 管理 |
| `/hooks` | `hk` | 管理 Hooks |

#### Git 操作

| 命令 | 别名 | 描述 |
|------|------|------|
| `/git <args>` | | 运行 git 命令 |

#### Research 学术研究

| 命令 | 别名 | 描述 |
|------|------|------|
| `/search-papers` | `sp` | 搜索学术论文 |
| `/fetch-paper` | `fp` | 获取指定论文 |
| `/papers` | | 列出已有论文 |

#### Pipeline 流水线

| 命令 | 别名 | 描述 |
|------|------|------|
| `/pipeline [list\|run]` | | 管理单流水线 |
| `/analyze` | `da` | 运行数据分析流水线 |
| `/write` | `wp` | 运行写作流水线 |

#### Skills & Plugins 技能与插件

| 命令 | 别名 | 描述 |
|------|------|------|
| `/skills` | `sk` | 技能管理 |
| `/mcp` | `m` | MCP 服务器管理 |
| `/plugins` | `plugin` | 插件管理 |

#### Evolution 自我进化

| 命令 | 别名 | 描述 |
|------|------|------|
| `/trajectories` | `traj` | 查看执行轨迹 |
| `/review` | | 自我审查 |
| `/curator` | | 轨迹策展 |
| `/critiques` | `cq` | 查看评审意见 |
| `/improve` | | 自我改进 |
| `/runs` | | 列出评测运行 |
| `/run` | | 查看评测运行详情 |

#### Scheduling 定时调度

| 命令 | 别名 | 描述 |
|------|------|------|
| `/cron [list\|add\|remove]` | | 管理定时任务 |
| `/auto-memory` | `am` | 自动记忆管理（on/off/extract/show/config） |

#### Advanced 高级功能

| 命令 | 别名 | 描述 |
|------|------|------|
| `/save` | `ss` | 保存会话状态 |
| `/load` | | 加载会话状态 |
| `/sessions` | | 会话管理 |
| `/export` | | 导出会话 |
| `/profile` | `prof` | 配置档案管理 |
| `/theme` | | 切换主题 |
| `/output` | | 切换输出格式 |
| `/verbose` | | 切换详细模式 |
| `/doctor` | `doc` | 诊断配置问题 |
| `/delegate` | `dl` | 委托子 Agent |
| `/search` | | 搜索功能 |
| `/inspect` | `ins` | 检查状态 |
| `/trace` | `tr` | 追踪观测（sessions/summary/stats） |
| `/workspace` | `ws` | 工作区管理 |

#### Security 安全

| 命令 | 别名 | 描述 |
|------|------|------|
| `/permission [mode]` | `perm` | 查看/设置权限模式 |

#### Info 信息

| 命令 | 别名 | 描述 |
|------|------|------|
| `/tools` | | 显示可用工具 |
| `/cost` | | 显示 Token 用量 |
| `/usage` | | 使用统计 |
| `/debug` | | 调试信息 |
| `/help` | `h`, `?` | 显示帮助信息 |

#### Exit 退出

| 命令 | 别名 | 描述 |
|------|------|------|
| `/quit` / `/exit` | `q` | 退出应用 |

### CLI 常用参数

| 参数 | 短形式 | 描述 |
|------|--------|------|
| `--model <name>` | `-m` | 指定模型名称 |
| `--config <path>` | | 指定配置文件路径 |
| `--mcp-config <path>` | | 指定 MCP 配置文件路径 |
| `--project <path>` | | 指定项目目录 |
| `--continue` | `-c` | 继续最近一次会话 |
| `--resume <id>` | `-r` | 恢复指定会话 |
| `--verbose` | `-v` | 详细输出模式 |

> **提示**：Agent 模式、系统提示词等可在 TUI 内通过 `/mode`、`/system` 等 slash 命令调整，无需通过 CLI 参数设置。

---

## 🧪 开发

### 代码检查

```bash
# Rust 编译检查（TUI 模式，默认）
cargo check --workspace

# 检查所有功能
cargo check --workspace --features "tui,gui"

# Clippy 检查
cargo clippy --workspace --features "tui,gui"

# 运行测试
cargo test --workspace --features "tui,gui"
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

### 分层架构

```
echo-agent-cli (二进制入口)
    ├── src/cli/      CLI / REPL / Slash 命令
    ├── src/tui/      TUI 前端 (ratatui)
    └── src/tauri/    GUI 前端 (Tauri IPC)
            │
            ▼
echo-agent-app-core (共享应用库)
    ├── state / config / memory
    ├── tasks / sessions / workspace
    ├── project / scheduler / skills_hub
    └── output / hitl / webhook / observability
            │
            ▼
echo-agent (AI Agent 框架)
    ├── react agent loop / tool execution
    ├── MCP / LSP / memory
    ├── subagent / eval / improve
    └── sqlite / tasks / human-loop
```

### Agent Streaming

使用 `tokio::sync::mpsc::unbounded_channel` 实现真正增量式流式输出：

- 后台任务获取 `RwLock<ReactAgent>` 并调用 `execute_stream()`/`chat_stream()`
- 逐事件通过 channel 发送
- 返回基于 channel 接收端的流，避免返回时持有锁

### 主题系统

统一 TUI 的 `ColorTheme` 和 GUI 的主题：

- `ColorTheme` 提供 6 种内置主题（dark、light、monokai、solarized、dracula、one-dark）
- TUI 通过 `Theme::from_color_theme()` 从 `ColorTheme` 生成
- GUI 使用 CSS 变量支持亮色/暗色主题切换
- 支持运行时切换主题（`/theme` 命令）

### 后台任务

支持多种任务类型（`BackgroundTaskKind`）：

- `AgentChat` — 单次对话
- `Cron` — 定时任务
- `Workflow` — 工作流编排
- `Research` — 学术研究流水线（论文检索 → 综合 → 撰写）
- `ResearchToWriting` — 研究到写作端到端流水线
- `DataPipeline` — 数据处理流水线（加载 → 分析 → 可视化 → 总结）
- `Writing` — 文档写作流水线

### 统一记忆系统

三层记忆架构：

- **User** — 全局用户偏好和指令（`~/.echo-agent/`）
- **Project** — 项目级上下文和规则（`.echocowork/`）
- **Local** — 本地开发环境特定配置

支持 `/auto-memory` 自动从会话中提取记忆。

### LSP 集成

当检测到 `.lsp.yaml` 配置时（项目目录或 `~/.echo-agent/`），自动注册 LSP 工具：

- 诊断信息获取
- 跳转到定义
- 查找引用
- 悬停提示
- LSP 状态查询

---

## 📁 工作区

工作区存储在 `~/.echo-agent/workspaces/` 下，包含：

```
workspaces/
├── {workspace-id}/
│   └── .echocowork/
│       ├── sessions/         # 会话历史（SQLite + FTS）
│       ├── conversations/    # 对话记录
│       ├── memory/            # 记忆存储
│       ├── tasks/             # 任务状态
│       ├── traces/            # 执行轨迹
│       ├── logs/              # 日志
│       ├── data/              # 数据文件
│       ├── papers/            # 论文文件
│       ├── artifacts/         # 生成物
│       ├── scratchpad.md      # 共享草稿
│       └── workspace.json     # 工作区清单
```

---

## 📖 更多文档

详细文档请参阅 `docs/` 目录：

- [架构设计](docs/architecture.md) — 系统架构与设计决策
- [配置指南](docs/configuration.md) — 配置文件详解
- [入门指南](docs/getting-started.md) — 从零开始使用

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
