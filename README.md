# EKO

> 一个基于 [echo-agent](https://github.com/EchoYue-lp/echo-agent) 的通用 AI Agent 产品，支持 Coding、数据分析和学术研究三大核心能力。

[![Rust](https://img.shields.io/badge/Rust-1.95%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/license/mit)

## 📋 项目简介

EKO 是一个生产级的通用 Agent 产品，基于 Rust 生态构建，提供 **TUI（终端界面）** 和 **GUI（桌面应用）** 两种交互模式，专注于以下核心场景：

- **💻 Coding** — 代码生成、审查、重构、调试、测试
- **📊 数据分析** — 结构化数据分析、统计、可视化、报告生成
- **📚 学术研究** — arXiv/语义学者检索、论文阅读、学术写作辅助
- **🏥 医学研究** — PubMed 文献检索、临床试验查询、循证医学分析

### 核心特性

- 🤖 **双模式交互**：全屏终端（TUI）、桌面应用（Tauri GUI）
- 🔄 **长程任务支持**：断点续传、进度追踪、人机协作检查点
- 🧩 **可扩展架构**：MCP 服务器、插件系统（PluginRegistry）、技能管理
- 📡 **IM 通道集成**：支持 QQ Bot、飞书（飞书 webhook/long_poll 模式）
- 🔗 **Hooks 系统**：可配置的事件钩子，支持自定义工作流
- 🎨 **现代化 GUI**：React + Tailwind CSS + typed Tauri IPC 实时投影
- 🧠 **统一记忆系统**：User / Project / Local 三层记忆，支持自动提取
- 🔧 **LSP 集成**：诊断、跳转定义、查找引用、悬停提示
- 🔁 **自我进化**：轨迹回放、自我审查、自动改进

---

## 🏗️ 项目结构

```
echo-agent-cli/
├── Cargo.toml              # Rust 工作区配置（v1.0.0, edition 2024）
├── init.sh                 # 初始化脚本
├── config/                 # 配置文件（eko.yaml, mcp.json）
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
│       ├── conversation_projection.rs  # 会话 UI 投影 DTO
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
- **Node.js** 20.19+、22.13+ 或 24+（仅 GUI 桌面应用需要，TUI 不需要）
- **Tauri CLI**（仅打包桌面安装包时需要）：`npm install -g @tauri-apps/cli`

### 安装依赖

```bash
cd echo-agent-cli

# 安装 Rust 依赖
cargo fetch

# 安装前端依赖（仅 GUI 需要，TUI 可跳过）
cd web-frontend && npm install && cd ..
```

> **提示**：如果只使用 TUI 模式，可以跳过 Node.js 和前端依赖安装。使用 `./init.sh` 会自动处理。

### 配置

GUI、TUI 和 CLI 共享 `model_providers`、`configured_models` 和 `default_model_id`。用户可以创建多个 Provider，并在每个 Provider 下创建多个模型；每个模型明确选择 Chat Completions、Responses 或 Anthropic 协议，纯文本能力默认启用，并可追加图像、音频、视频能力。用户填写的 API Key 优先级高于 Provider 配置的环境变量。

也可以通过 `eko.yaml` 设置模型。完整配置参考：

- [配置指南](docs/configuration.md)

#### 配置文件位置

EKO 按以下优先级查找配置文件：

1. 命令行参数: `--config <path>`
2. 环境变量: `EKO_CONFIG`
3. 当前目录: `./eko.yaml`
4. 用户目录: `~/.eko/config.yaml`

MCP 配置优先级：`--mcp-config` → YAML `mcp.config_path` →
`MCP_CONFIG_PATH` → `~/.eko/mcp.json`

---

## 📦 构建与安装

项目使用 Feature Flags 分离 TUI 和 GUI 构建。默认启用 `tui` feature。

> **入口区别**：`echo-agent-cli` 是 TUI（终端全屏界面）入口；`echo-agent-tauri` 是 GUI（Tauri 桌面应用）入口。若运行 `target/release/echo-agent-cli`，看到 TUI 是预期行为。要生成可双击打开且包含前端资源的桌面产物，请使用 Tauri 打包命令：`cargo tauri build -- --no-default-features --features gui`。GUI feature 会同时启用 `channels`，因此桌面应用会一并打包多通道能力。

### TUI（终端全屏界面）

```bash
# 编译（Release）
cargo build --bin echo-agent-cli --release

# 直接运行（不安装）
cargo run --bin echo-agent-cli

# 安装到 ~/.cargo/bin（推荐，可全局调用）
cargo install --path . --bin echo-agent-cli --no-default-features --features tui

# 确认安装成功
which echo-agent-cli

echo-agent-cli   # 安装后直接运行
```

编译产物路径：`target/debug/echo-agent-cli` 或 `target/release/echo-agent-cli`

> **推荐**：确认 `which echo-agent-cli` 能找到 `~/.cargo/bin/echo-agent-cli` 后，再设置别名 `alias ecw='echo-agent-cli'`（添加到 `~/.bashrc` 或 `~/.zshrc`）。如果找不到，请先把 `~/.cargo/bin` 加入 `PATH`。

### GUI（桌面应用）

GUI 使用 Tauri 打包，包含两部分：

- `web-frontend/`：React 前端，构建产物为 `web-frontend/dist`；
- Rust GUI 运行时：由 `gui` feature 启用，并自动包含 `channels` 多通道能力，最终随 Tauri 一起打进桌面应用包。

> `tauri.conf.json` 已配置 GUI runner；直接运行 `cargo tauri dev/build` 会使用 `echo-agent-tauri` 和 `gui` feature，不会落到默认 TUI binary。Tauri CLI 目前会把 Cargo 默认 feature 展开进日志，因此你可能看到 `--features gui,tui`；这是 Tauri CLI 的参数展开行为，不代表启动了 TUI 入口。日常 GUI 开发推荐使用 `.cargo/config.toml` 中的 `cargo gui-dev` alias。

#### 开发运行

```bash
# 推荐：启动前端 Vite 服务并打开 Tauri 窗口，且显式禁用默认 tui feature
cargo gui-dev

# 等价的完整命令
cargo tauri dev -- --no-default-features --features gui --bin echo-agent-tauri

# 兼容入口：也会启动 GUI，但日志中可能出现 --features gui,tui
cargo tauri dev
```

#### 生产打包

```bash
# 首次需要安装 Tauri CLI
npm install -g @tauri-apps/cli

# 推荐：自动构建前端 + 编译 Release + 生成平台原生安装包
cargo gui-bundle

# 兼容入口：也会打包 GUI，但日志中可能出现 --features gui,tui
cargo tauri build
```

打包产物路径：

| 平台    | 产物                                                         |
| ------- | ------------------------------------------------------------ |
| macOS   | `target/release/bundle/macos/EKO.app`                        |
|         | `target/release/bundle/dmg/EKO_*.dmg`                        |
| Linux   | `target/release/bundle/deb/echo-agent-tauri_*.deb`           |
|         | `target/release/bundle/appimage/echo-agent-tauri_*.AppImage` |
| Windows | `target/release/bundle/msi/EKO_*.msi`                        |
|         | `target/release/bundle/nsis/EKO_*.exe`                       |

> **注意**：不要只把 `target/release/echo-agent-tauri` 复制进 `.app` 目录来当作安装包；那只是裸后端二进制，可能缺少前端资源、图标和平台元数据。

#### 裸 Cargo 调试（非分发产物）

如需单独调试 GUI 后端二进制，可手动构建前端后运行：

```bash
cd web-frontend && npm run build:tauri && cd ..

cargo build --bin echo-agent-tauri --no-default-features --features gui --release
cargo run --bin echo-agent-tauri --no-default-features --features gui

# 或使用项目 alias
cargo gui-build
cargo gui-run
```

裸可执行文件路径：`target/release/echo-agent-tauri`（Windows 为 `target/release/echo-agent-tauri.exe`）。

#### 为什么不会打成 TUI？

Tauri CLI 打包时会构建包名二进制 `echo-agent-cli`，项目已在 `--no-default-features --features gui` 下把它路由到同一套 GUI/Tauri 运行时；因此打包产物不会启动 TUI。

`gui` feature 已依赖 `channels` feature，所以 GUI 打包命令无需额外写 `--features "gui,channels"`，多通道能力会随 GUI 一起编译进桌面应用。

当前 Tauri CLI 的 lifecycle command 工作目录是 `src-tauri/`，所以 `tauri.conf.json` 中的前端命令写作 `cd ../web-frontend && npm run dev:tauri` / `cd ../web-frontend && npm run build:tauri`。

#### GUI 功能状态

GUI 已接真实 Tauri 后端的聊天/会话、TaskRuntime/Subagent、记忆/自进化、工具、
MCP、技能、Plugin、模型供应商、权限/审计、压缩、定时任务、Trace、Terminal、
Browser、Sandbox、数据分析和论文/系统综述工作台。Workflow 和通用结构化抽取的后端
已存在，但 React panel 尚未接入生产导航，不能算 GUI 完成。当前代码依据与尚在收口的
缺口见 [功能总览](docs/features.md)。

> **注意**：每个平台只能打包该平台原生的安装包。如需交叉编译请使用 CI/CD（如 GitHub Actions）。

### Feature Flags 说明

| Feature     | 描述                                   | 默认启用 |
| ----------- | -------------------------------------- | -------- |
| `tui`       | 终端全屏界面（ratatui）                | ✅       |
| `gui`       | 桌面应用（Tauri，自动包含 `channels`） | ❌       |
| `channels`  | 多通道支持（IM）                       | ❌       |
| `telemetry` | 遥测数据收集                           | ❌       |
| `devtools`  | Tauri 开发者工具                       | ❌       |

### echo-agent 依赖 Features

echo-agent-cli 启用以下 echo-agent 框架 features：

| Feature                   | 描述                  |
| ------------------------- | --------------------- |
| `mcp`                     | MCP 协议支持          |
| `lsp`                     | LSP 语言服务器集成    |
| `human-loop`              | 人机协作循环          |
| `subagent`                | 子 Agent 编排         |
| `tasks`                   | 任务系统              |
| `git` / `shell` / `files` | 编码与本地执行工具    |
| `web` / `media` / `chart` | Web、多模态与图表工具 |
| `research` / `rag`        | 学术研究与检索能力    |

EKO 不启用 framework `sqlite` feature；会话、runtime、memory 和 TaskRun 使用文件存储。

---

## 🖥️ 使用指南

### TUI 快捷键

| 快捷键              | 功能                             |
| ------------------- | -------------------------------- |
| `Ctrl+C` / `Ctrl+Q` | 退出应用                         |
| `Ctrl+B`            | 切换侧边栏                       |
| `Ctrl+L`            | 清空聊天                         |
| `Shift+Enter`       | 输入换行                         |
| `Enter`             | 发送消息                         |
| `Esc`               | 取消生成 / 关闭弹窗              |
| `Tab`               | 切换侧边栏标签                   |
| `S-Tab`             | 补全列表上一项                   |
| `↑/↓`               | 浏览输入历史                     |
| `PageUp/PageDown`   | 快速滚动聊天                     |
| `y` / `n`           | 批准 / 拒绝工具执行（HITL 审批） |

### Slash 命令

在输入框输入 `/` 可查看可用命令（命令面板，支持模糊搜索）：

#### Session 会话管理

| 命令       | 别名   | 描述                            |
| ---------- | ------ | ------------------------------- |
| `/clear`   | `cls`  | 清空当前对话并重置 Agent 上下文 |
| `/history` | `hist` | 查看会话历史                    |
| `/stats`   | `st`   | 显示会话统计                    |
| `/status`  |        | 显示 Agent 状态                 |
| `/new`     | `n`    | 创建新会话                      |
| `/compact` | `cp`   | 压缩上下文窗口                  |
| `/undo`    | `u`    | 撤销上一步操作                  |

#### Context 上下文管理

| 命令                 | 别名   | 描述                                                     |
| -------------------- | ------ | -------------------------------------------------------- |
| `/mode <mode>`       |        | 切换模式（general/coding/research/medical/data/writing） |
| `/model <name>`      |        | 切换模型                                                 |
| `/think [level]`     |        | 查看或设置当前模型支持的思考等级                         |
| `/reasoning [level]` |        | `/think` 的别名                                          |
| `/system [prompt]`   | `sys`  | 查看或设置系统提示词                                     |
| `/memory`            |        | 查看记忆内容                                             |
| `/remember <fact>`   |        | 保存一条记忆                                             |
| `/forget <fact>`     |        | 删除一条记忆                                             |
| `/compress`          |        | 手动压缩上下文                                           |
| `/context`           |        | 查看上下文信息                                           |
| `/refresh`           |        | 刷新项目上下文                                           |
| `/project`           | `proj` | 项目管理                                                 |

#### Coding 编码工具

| 命令                  | 别名 | 描述                     |
| --------------------- | ---- | ------------------------ |
| `/plan`               |      | 进入计划模式（只读分析） |
| `/tasks`              |      | 查看活跃任务             |
| `/task-progress`      | `tp` | 查看任务进度             |
| `/task-tree`          | `tt` | 查看任务树               |
| `/test [name]`        |      | 运行测试                 |
| `/code-review [path]` | `cr` | 请求代码审查             |
| `/fix`                |      | 自动修复问题             |
| `/diff [file]`        |      | 查看 git 或文件差异      |
| `/agents`             |      | 列出可用 Agent           |
| `/agent`              |      | Agent 管理               |
| `/hooks`              | `hk` | 管理 Hooks               |

#### Git 操作

| 命令          | 别名 | 描述          |
| ------------- | ---- | ------------- |
| `/git <args>` |      | 运行 git 命令 |

#### Research 学术研究

| 命令             | 别名 | 描述         |
| ---------------- | ---- | ------------ |
| `/search-papers` | `sp` | 搜索学术论文 |
| `/fetch-paper`   | `fp` | 获取指定论文 |
| `/papers`        |      | 列出已有论文 |

#### Medical 医学研究

医学模式通过 `/mode medical` 切换，Agent 可直接调用以下工具（无需 slash 命令）：

- `pubmed_search` — 搜索 PubMed 医学文献（PMID、MeSH 词、摘要）
- `clinical_trials_search` — 搜索 ClinicalTrials.gov 临床试验（NCT ID、状态、阶段、结局）
- `pdf_fetch` — 下载并解析论文全文
- `web_search` + `web_fetch` — 网络搜索补充信息

所有医学工具**免费使用**，无需 API Key。详见 [配置指南](docs/configuration.md)。

#### Pipeline 流水线

| 命令                    | 别名 | 描述               |
| ----------------------- | ---- | ------------------ |
| `/pipeline [list\|run]` |      | 管理单流水线       |
| `/analyze`              | `da` | 运行数据分析流水线 |
| `/write`                | `wp` | 运行写作流水线     |

#### Skills & Plugins 技能与插件

| 命令       | 别名     | 描述           |
| ---------- | -------- | -------------- |
| `/skills`  | `sk`     | 技能管理       |
| `/mcp`     | `m`      | MCP 服务器管理 |
| `/plugins` | `plugin` | 插件管理       |

EKO 插件采用扁平包结构：根目录使用 `plugin.json`，Skills、MCP、Subagents、
Hooks、LSP、monitors、themes 和 output styles 都从固定根位置发现，不使用
namespace 或组件路径声明。旧 `.echo-plugin/manifest.yaml` 不再支持。
完整格式见 [echo-agent 插件系统文档](../echo-agent/docs/zh/32-plugin-system.md)。

#### Evolution 自我进化

| 命令                | 别名 | 描述                                         |
| ------------------- | ---- | -------------------------------------------- |
| `/review`           |      | 从最近运行生成带证据的记忆候选（默认不保存） |
| `/curator`          |      | 管理技能生命周期                             |
| `/critiques`        | `cq` | 查看评审意见                                 |
| `/memory-review`    | `mr` | 审查已积累记忆（默认不自动语义合并）         |
| `/skill-candidates` | `sc` | 查看技能候选与草稿                           |
| `/runs`             |      | 列出最近运行                                 |
| `/run`              |      | 查看或导出运行详情                           |

#### Scheduling 定时调度

| 命令                        | 别名 | 描述                                       |
| --------------------------- | ---- | ------------------------------------------ |
| `/cron [list\|add\|remove]` |      | 管理定时任务                               |
| `/auto-memory`              | `am` | 自动记忆管理（on/off/extract/show/config） |

#### Advanced 高级功能

| 命令                | 别名    | 描述                                          |
| ------------------- | ------- | --------------------------------------------- |
| `/checkpoint`       | `/save` | 强制保存当前 runtime checkpoint               |
| `/sessions [query]` | `ss`    | 从 canonical ConversationStore 列出或搜索会话 |
| `/export`           |         | 导出会话                                      |
| `/profile`          | `prof`  | 配置档案管理                                  |
| `/theme`            |         | 切换主题                                      |
| `/output`           |         | 切换输出格式                                  |
| `/verbose`          |         | 切换详细模式                                  |
| `/doctor`           | `doc`   | 诊断配置问题                                  |
| `/delegate`         | `dl`    | 委托子 Agent                                  |
| `/search`           |         | 搜索功能                                      |
| `/inspect`          | `ins`   | 检查状态                                      |
| `/trace`            | `tr`    | 追踪观测（sessions/summary/stats）            |
| `/workspace`        | `ws`    | 工作区管理                                    |

#### Security 安全

| 命令                 | 别名   | 描述              |
| -------------------- | ------ | ----------------- |
| `/permission [mode]` | `perm` | 查看/设置权限模式 |

#### Info 信息

| 命令     | 别名     | 描述            |
| -------- | -------- | --------------- |
| `/tools` |          | 显示可用工具    |
| `/cost`  |          | 显示 Token 用量 |
| `/usage` |          | 使用统计        |
| `/debug` |          | 调试信息        |
| `/help`  | `h`, `?` | 显示帮助信息    |

#### Exit 退出

| 命令              | 别名 | 描述     |
| ----------------- | ---- | -------- |
| `/quit` / `/exit` | `q`  | 退出应用 |

### CLI 常用参数

| 参数                   | 短形式 | 描述                                                            |
| ---------------------- | ------ | --------------------------------------------------------------- |
| `--model <id-or-name>` | `-m`   | 选择已启用的配置模型 ID 或唯一模型名称；未知/重名会报错         |
| `--config <path>`      |        | 指定配置文件路径                                                |
| `--mcp-config <path>`  |        | 指定 MCP 配置文件路径                                           |
| `--project <path>`     |        | 指定项目目录                                                    |
| `--jsonl <prompt>`     |        | 非交互执行一次请求；stdout 每行输出一个 canonical chat envelope |
| `--continue`           | `-c`   | 继续最近一次会话                                                |
| `--resume <id>`        | `-r`   | 恢复指定会话                                                    |
| `--verbose`            | `-v`   | 详细输出模式                                                    |

> **提示**：Agent 模式、系统提示词等可在 TUI 内通过 `/mode`、`/system` 等 slash 命令调整，无需通过 CLI 参数设置。

---

## 🧪 开发

### 代码检查

```bash
# Rust 编译检查（TUI 模式，默认）
cargo check --workspace

# GUI 入口编译检查
cargo check --bin echo-agent-tauri --no-default-features --features gui

# TUI Clippy 检查
cargo clippy --workspace

# TUI 测试
cargo test --workspace
```

> **说明**：为避免混淆，日常开发也建议分别检查 TUI 与 GUI 入口；不要把 TUI/GUI 当成同一个打包产物处理。

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
    ├── tasks / conversations / workspace
    ├── project / scheduler / skills_hub
    └── output / hitl / webhook / observability
            │
            ▼
echo-agent (AI Agent 框架)
    ├── react agent loop / tool execution
    ├── MCP / LSP / memory
    ├── Subagent / task DAG / workflow
    └── file stores / human-loop / model protocols
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

- **User** — 全局用户偏好和指令（`~/.eko/`）
- **Project** — 项目级上下文和规则（`.eko/`）
- **Local** — 本地开发环境特定配置

支持 `/auto-memory` 从会话提取带证据候选，统一进入 Review Inbox，采纳后才写入长期记忆。

### LSP 集成

当检测到 `.lsp.yaml` 配置时（项目目录或 `~/.eko/`），自动注册 LSP 工具：

- 诊断信息获取
- 跳转到定义
- 查找引用
- 悬停提示
- LSP 状态查询

---

## 📁 工作区

工作区存储在 `~/.eko/workspaces/` 下，包含：

```
workspaces/
├── {workspace-id}/
│   └── .eko/
│       ├── conversations/    # 文件化对话记录与搜索
│       ├── memory/            # 记忆存储
│       ├── tasks/             # 任务状态
│       ├── traces/            # 执行轨迹
│       ├── logs/              # 日志
│       ├── data/              # 数据文件
│       ├── papers/            # 论文文件
│       ├── artifacts/         # 生成物
│       ├── scratchpad.md      # 共享草稿
│       ├── decisions.jsonl    # 决策日志
│       └── workspace.json     # 工作区清单
```

---

## 📖 更多文档

详细文档请参阅 `docs/` 目录：

- [文档索引](docs/README.md) — 项目文档边界与导航
- [功能总览](docs/features.md) — 已实现能力与代码依据
- [架构说明](docs/architecture.md) — 当前运行时所有权与数据流
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

MIT License。

---

## 🤝 致谢

- [echo-agent](https://github.com/EchoYue-lp/echo-agent) — 底层 AI Agent 框架
- [ratatui](https://github.com/ratatui-org/ratatui) — 终端 UI 库
- [Tauri](https://tauri.app/) — 桌面应用框架
- [React](https://react.dev/) + [Vite](https://vitejs.dev/) + [Tailwind CSS](https://tailwindcss.com/) — Web 前端技术栈
