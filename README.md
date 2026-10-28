# Eclip /ˈɛk.lɪp/

**听见你的声音，让想法飞跃。**

读作 **EK-lip**，两个音节，重音在前。品牌延续 Echo 的初衷：让 AI 听见用户的声音、理解诉求、产生共鸣并解决问题。图标与使用规范见 [品牌说明](design/branding/README.md)。

> 一个基于 [echo-agent](https://github.com/EchoYue-lp/echo-agent) 的通用 AI Agent 产品，支持 Coding、数据分析和学术研究三大核心能力。

框架的跨语言 SDK 产品（Rust Host/Protocol、TypeScript、Python、Java 以及版本化合同）维护在独立的
[echo-agent-sdk](https://github.com/EchoYue-lp/echo-agent-sdk) 仓库；Eclip 只负责应用层组合与产品策略。

[![Rust](https://img.shields.io/badge/Rust-1.95%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/license/mit)

## 📋 项目简介

Eclip 是一个生产级的通用 Agent 产品，基于 Rust 生态构建，以 **GUI（桌面应用）** 为唯一产品主入口，继续接收 IM 和 cron 后台输入，专注于以下核心场景：

- **💻 Coding** — 代码生成、审查、重构、调试、测试
- **📊 数据分析** — 结构化数据分析、统计、可视化、报告生成
- **📚 学术研究** — arXiv/语义学者检索、论文阅读、学术写作辅助
- **🏥 医学研究** — PubMed 文献检索、临床试验查询、循证医学分析

### 核心特性

- 🤖 **桌面主入口**：Tauri GUI；IM 收发和 cron 继续沿共享 app-core 运行
- 🔄 **长程任务支持**：断点续传、进度追踪、人机协作检查点
- 🧩 **可扩展架构**：MCP 服务器、插件系统（PluginRegistry）、技能管理
- 📡 **IM 通道集成**：支持 QQ Bot、飞书（飞书 webhook/long_poll 模式）
- 🔗 **Hooks 系统**：可配置的事件钩子，支持自定义工作流
- 🎨 **现代化 GUI**：React + Tailwind CSS + typed Tauri IPC 实时投影
- 🧠 **统一记忆系统**：User / Project / Local 三层记忆，支持自动提取
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
│   ├── lib.rs              # 库导出
│   ├── tauri/              # Tauri IPC 层（GUI 后端）
│   └── logging/            # 日志 inspector
├── echo-agent-app-core/    # 共享应用库（GUI/IM/cron）
│   └── src/
│       ├── state/           # 配置、workspace/delivery scope 与 AppState 聚合
│       ├── agent_handle.rs # Agent 并发封装
│       ├── infra/          # Agent 创建、MCP 加载、日志与诊断
│       ├── config*.rs      # 配置加载与热重载
│       ├── unified_memory.rs # 统一记忆系统
│       ├── tasks/          # 后台任务、长程任务、流水线
│       ├── hitl/           # 人机协作循环
│       ├── workspace/      # 工作区管理
│       ├── conversation_projection.rs  # 会话 UI 投影 DTO
│       ├── project/        # 项目上下文、编码循环
│       ├── output/         # 共享输出投影
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
- **Node.js** 20.19+、22.13+ 或 24+（桌面开发/打包需要）
- **Tauri CLI 2.12.4**（GUI 开发/打包需要）：Cargo 与 npm 启动器独立安装，构建前核对实际入口版本。

### 安装依赖

```bash
cd echo-agent-cli

# 安装 Rust 依赖
cargo fetch

# 安装桌面前端依赖
cd web-frontend && npm install && cd ..
```


### 配置

GUI、IM 与 cron 共享 `model_providers`、`configured_models` 和 `default_model_id`。用户可以创建多个 Provider，并在每个 Provider 下创建多个模型；每个模型明确选择 Chat Completions、Responses 或 Anthropic 协议，纯文本能力默认启用，并可追加图像、音频、视频能力。用户填写的 API Key 优先级高于 Provider 配置的环境变量。

也可以通过 `eko.yaml` 设置模型。完整配置参考：

- [配置指南](docs/zh/configuration.md)

#### 配置文件位置

Eclip 按以下优先级查找配置文件：

1. 命令行参数: `--config <path>`
2. 环境变量: `EKO_CONFIG`
3. 当前目录: `./eko.yaml`
4. 用户目录: `~/.eko/config.yaml`

MCP 配置优先级：`--mcp-config` → YAML `mcp.config_path` →
`MCP_CONFIG_PATH` → `~/.eko/mcp.json`

---

## 📦 构建与安装

源码只保留既有 `echo-agent-tauri` 桌面产品入口，默认 `gui` feature 同时启用
`channels`。独立 TUI、REPL、CLI/JSONL 和 standalone `--channels` 已退出；Rust package
仍名为 `echo-agent-cli`，不因入口或品牌变化改名。GUI 内置 PTY/SSH 终端、Agent shell、
桌面四个配置参数及独立 SDK Host stdio 继续保留。

### GUI（桌面应用）

GUI 使用 Tauri 打包，包含两部分：

- `web-frontend/`：React 前端，构建产物为 `web-frontend/dist`；
- Rust GUI 运行时：由 `gui` feature 启用，并自动包含 `channels` 多通道能力，最终随 Tauri 一起打进桌面应用包。

Rust `tauri` 与前端 `@tauri-apps/api` 须保持同一 minor 版本，当前两个锁文件中均为 `2.12.4`。前端固定该精确版本并保留 `package-lock.json`；安装后仍需通过 Tauri 原有的版本匹配检查。

> Cargo 的 `default-run` 与首个 `[[bin]]` 均为 `echo-agent-tauri`，Tauri runner 另显式选择 GUI binary 与 `gui` feature。当前默认 feature 不再包含 `tui`；不保留旧模式逃逸分派。日常 GUI 开发推荐使用 `.cargo/config.toml` 中的 `cargo gui-dev` alias。

#### 开发运行

首次使用 GUI 前，先完成下方[工具安装](#生产打包)，并核对所选 `cargo tauri --version` 或 `tauri --version`。

```bash
# 推荐：启动前端 Vite 服务并打开 Tauri 窗口，显式选择同一 GUI 入口
cargo gui-dev

# 等价的完整命令
cargo tauri dev -- --no-default-features --features gui --bin echo-agent-tauri

# 同一 GUI 入口
cargo tauri dev
```

#### 生产打包

```bash
# 使用 Cargo 子命令时，先安装匹配版本的 Cargo CLI
cargo install tauri-cli --version 2.12.4 --locked

# 如选用独立的官方 npm tauri 启动器，可安装以下精确版本
npm install -g @tauri-apps/cli@2.12.4

# 推荐：自动构建前端 + 编译 Release + 生成平台原生安装包
cargo gui-bundle

# 同一 GUI 打包入口
cargo tauri build
```

打包产物路径：

| 平台    | 产物                                                         |
| ------- | ------------------------------------------------------------ |
| macOS   | `target/release/bundle/macos/Eclip.app`                      |
|         | `target/release/bundle/dmg/Eclip_*.dmg`                      |
| Linux   | `target/release/bundle/deb/echo-agent-tauri_*.deb`           |
|         | `target/release/bundle/appimage/echo-agent-tauri_*.AppImage` |
| Windows | `target/release/bundle/msi/Eclip_*.msi`                      |
|         | `target/release/bundle/nsis/Eclip_*.exe`                     |

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

#### 桌面包主入口如何选择？

`Cargo.toml` 的 `default-run` 与唯一产品 `[[bin]]` 均为 `echo-agent-tauri`，
Tauri runner 与 `mainBinaryName` 继续选择同一二进制。独立 `echo-agent-cli` binary 已移除；
这不修改包名、`com.eko.desktop` identifier 或 `~/.eko`／workspace `.eko` 数据路径。

新包须实际构建并核对 `Info.plist` 的 `CFBundleExecutable`、包内二进制和内嵌前端资源。入口配置修正后，仍须对每个实际新包分别完成打包结果和原生运行验收；旧裸 binary 和单独静态页面不作为新包已通过的证据。

`gui` feature 已依赖 `channels` feature，所以 GUI 打包命令无需额外写 `--features "gui,channels"`，多通道能力会随 GUI 一起编译进桌面应用。

当前 Tauri CLI 的 lifecycle command 工作目录是 `src-tauri/`，所以 `tauri.conf.json` 中的前端命令写作 `cd ../web-frontend && npm run dev:tauri` / `cd ../web-frontend && npm run build:tauri`。

#### GUI 功能状态

GUI 已接真实 Tauri 后端的聊天/会话、一级 Side Conversation、TaskRuntime/Subagent、记忆/自进化、工具、
MCP、技能、Plugin、模型供应商、权限/审计、压缩、定时任务、Trace、Terminal、
Browser、Sandbox、数据分析和论文/系统综述工作台。Workflow 和通用结构化抽取的后端
已存在，但 React panel 尚未接入生产导航，不能算 GUI 完成。当前代码依据与尚在收口的
缺口见 [功能总览](docs/zh/features.md)。

完成回复下方的“分叉会话”可复制至该回合的完整上下文并打开一个普通新会话；原会话与
带 `(branch)` 标题的新会话都保留在当前工作区会话列表，Fork 不会自动重发问题。

Side Conversation 依赖 GUI 的侧栏树和并行视图，只通过 Tauri 提供创建、恢复与管理；
IM 的普通 conversation、Subagent 和已有 fork 行为继续保留；不增加 Side Conversation
专用 channel wire contract，也不为保留共享能力恢复旧终端产品入口。

Agent 协作由模型工具和运行时自动完成，GUI 不提供手动消息或 Agent 组管理页面。
用户通过 Sidechat、Forkchat 参与独立探索，并在现有对话与任务视图中查看结果。

> **注意**：每个平台只能打包该平台原生的安装包。如需交叉编译请使用 CI/CD（如 GitHub Actions）。

### Feature Flags 说明

| Feature     | 描述                                   | 默认启用 |
| ----------- | -------------------------------------- | -------- |
| `gui`       | 桌面应用（Tauri，自动包含 `channels`） | ✅       |
| `channels`  | 多通道支持（IM）                       | 随 GUI   |
| `telemetry` | 遥测数据收集                           | ❌       |
| `devtools`  | Tauri 开发者工具                       | ❌       |

### echo-agent 依赖 Features

echo-agent-cli 启用以下 echo-agent 框架 features：

| Feature                   | 描述                  |
| ------------------------- | --------------------- |
| `mcp`                     | MCP 协议支持          |
| `human-loop`              | 人机协作循环          |
| `subagent`                | 子 Agent 编排         |
| `git` / `shell` / `files` | 编码与本地执行工具    |
| `web` / `media` / `chart` | Web、多模态与图表工具 |
| `research` / `rag`        | 学术研究与检索能力    |

Eclip 不启用 framework `sqlite` feature；会话、runtime、memory 和 TaskRun 使用文件存储。

---

## 🖥️ 使用指南

### GUI、IM 与后台运行

通过原 GUI 会话与任务视图发送输入、查看 Subagent 结果、附件与 HITL；IM 和 cron 继续经
同一 app-core 的输入/任务/恢复 owner 运行。隐藏窗口保持后台服务，真正退出关闭准入并等待
原生命周期物理结算，不留下 daemon。GUI 内置终端与 Agent shell 继续可用。

reasoning 与普通 assistant 正文分别保存，phase 只是消息阶段，不能因普通文字或工具完成
判定任务成功。最终候选经过原 Guard/Critic/Stop、持久化与运行时结算；运行时完成不等于
整个 TaskRun／Goal 已完成。交付仍消费原末尾状态与流 EOF。

### 桌面最小启动参数

| 参数 | 短形式 | 用途 |
| --- | --- | --- |
| `--model <id-or-name>` | `-m` | 选择已启用的配置模型 ID 或唯一模型名称 |
| `--config <path>` | | 指定配置文件 |
| `--mcp-config <path>` | | 指定 MCP 配置 |
| `--project <path>` | | 指定项目目录 |

`--tui`、`--cli`、`--jsonl`、`--channels`、console resume/permission/rendering 参数不属于
桌面入口，不作为另一个产品模式。会话和 Task/Subagent 恢复继续由 GUI/IM/cron 原 owner 承担。

---

## 🧪 开发

### 代码检查

```bash
# Rust workspace 快检
cargo check --workspace

# GUI 入口编译检查
cargo check --bin echo-agent-tauri --no-default-features --features gui

# Rust Clippy 快检
cargo clippy --workspace

# Rust workspace 测试
cargo test --workspace
```

> 裸后端与完整桌面包分别验收；日常快检不替代所属仓库的完整门禁与新包 native 验收。

### CI 依赖审计

CI 只由 `main` 推送触发；任务分支先按 [AGENTS.md](AGENTS.md) 完成本地适用门禁。
`linux-quality` 先用官方 `cargo-audit` 扫描本仓库真实 `Cargo.lock`，再复用同次检出的
framework `scripts/generate-vendored-media-audit-lock.py` 与其 `vendor/registry-origins.toml`，
以 `--lockfile` 指向本仓库锁、`--output` 指向新的临时文件，追加同一官方 scanner 审计。
临时视图只恢复三个 bundled parent 的原 registry source/checksum 归属，保留真实锁的其它节点和依赖边，
实际构建锁保持原样。Python 3.11+ 仅用于这项开发审计工具，不是 Eclip 运行依赖。

原交付按[人的精确风险决定](docs/supreme/specs/cancellation-observation-consumer/dependency-risk-decision.md)
曾接受 glib 0.18.5、proc-macro-error 1.0.4、bincode 1.3.3；本次源码退役后的最终锁、consumer范围与诊断须重新逐项核验，不自动继承或续写摘要。
`scripts/check_cli_dependency_risks.py` 保留完整官方扫描 JSON、stderr 和真实退出码，另行报告风险决定核对结果；
扫描 exit 1 不会被改称审计通过。仅允许清单中的 advisory、类别、版本、registry source/checksum；
新增告警、漏洞、yanked、扫描失败或输入漂移仍阻断。glib 的 soundness 风险尚未修复。
为防扫描配置静默关闭检查，gate 拒绝项目及 Cargo home 下的 `audit.toml` 覆盖。
`.cargo/accepted-dependency-risks.json` 同时绑定完整实际锁和应用 Rust 源码、manifest、Cargo 配置的输入指纹；
这些输入有任何变动都须重新评估消费者范围，不能自动刷新指纹延续接受。

```bash
# 使用 Python 3.11+；产物写入已忽略的任务目录，前缀每次取新名称
python3 -B scripts/test_cli_dependency_risks.py
python3 -B scripts/check_cli_dependency_risks.py \
  --report-prefix .echo-semantic/local/<task-id>/actual-audit
python3 -B scripts/check_cli_dependency_risks.py --registry-origins \
  --report-prefix .echo-semantic/local/<task-id>/origin-audit
```

默认更新官方 advisory 数据库；`--no-fetch` 只用于本地缓存诊断，不替代 CI 的在线扫描。
这项应用策略不改变框架/SDK政策，也不放宽 formatter、Clippy、测试或条件矩阵。

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
    ├── MCP / memory
    ├── Subagent / task DAG / workflow
    └── file stores / human-loop / model protocols
```

### 原生消息与执行观察

保留的GUI／IM／cron沿原AgentHandle与driver消费同一canonical流。reasoning、普通assistant
正文及tool call/result分别保留原身份和顺序；普通文字不被自动标作thinking。
phase或block complete是消息事实，不单独签发执行成功。最终候选经过原Guard/Critic/Stop、
持久化和物理owner结算，Stop要求继续仍返回同一主循环。兼容FinalAnswer之后仍消费最后
ExecutionStatus与原EOF；整个TaskRun／Goal是否完成继续由应用原权威判断。

### 界面配色

- GUI 使用 CSS 变量管理界面配色。
- 不提供只修改显示文案、却不改变渲染状态的伪主题命令。

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

项目根到当前工作目录的 `AGENTS.md` / `AGENTS.override.md` 会统一进入项目指令投影；
项目 `.eko/skills/` 中的 Skill 会在运行时自动发现。相同名称按项目 Skill、Plugin Skill、
用户 Skill 的顺序优先，项目 Skill 不写入全局启停文件。

GUI沿原自动记忆开关与提取入口生成带证据候选，统一进入Review Inbox；按原策略采纳后才写入长期记忆。独立console的`/auto-memory`不再是当前操作入口。

### 退役的语言服务

Eclip 不再请求 framework `lsp` feature，不注册语言服务工具，也不提供 `/lsp`、LSP 设置页或 IPC。
旧 `.lsp.yaml` 与插件 `lsp.yaml` 只产生明确退休诊断，用户文件保持原样；混合插件仍处理合法组件。
Framework 的具名物理撤销与 SDK 正式采用分别由各自精确源码和门禁承担，应用去依赖不替代其资格。
独立 TUI、REPL、CLI/JSONL 产品入口已退出；GUI、IM、cron、内置终端及其它共享 Agent 能力继续保留。

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

- [文档入口](docs/README.md) — `zh` 主源、`en` 审阅翻译和迁移状态
- [中文文档](docs/zh/README.md) — Eclip 长期文档主源（迁移完成后承载全部页面）
- [English documentation](docs/en/README.md) — reviewed English release tree
- [文档 parity gate](scripts/check-docs-parity.mjs) — 发布前的 fail-closed 检查

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
- [ratatui](https://github.com/ratatui-org/ratatui) — 已退役终端实现的历史致谢
- [Tauri](https://tauri.app/) — 桌面应用框架
- [React](https://react.dev/) + [Vite](https://vitejs.dev/) + [Tailwind CSS](https://tailwindcss.com/) — Web 前端技术栈

## 源码退役与交付资格

本文随独立终端源码删除采用；正式锁、删后构建、依赖策略、SDK正式Git来源、native窗口／
手机／SSH、main CI与发布分别验收。文案同步或旧focused通过不证明这些结果。
固定 `pre-retirement-2026-10-24` 与完整能力分支保旧入口，恢复需新任务评估，不改指 refs、
不回滚当前工作区、不删除用户会话、记忆、插件文件或任务journal。
