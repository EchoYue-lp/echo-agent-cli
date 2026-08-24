# EKO 功能总览

本页按当前生产代码整理功能，不按历史设计文件整理。判定“已实现”要求定义存在、
已接入应用入口，并且可从至少一个真实 surface 调用；仅有类型、测试夹具或未注册模块
不计入。

## Agent 与会话

| 能力           | 当前实现                                                                    | 主要依据                                        |
| -------------- | --------------------------------------------------------------------------- | ----------------------------------------------- |
| 流式对话       | TUI、GUI、CLI/JSONL、channel 共享 `drive_chat` 与 typed `TurnOutcome`       | `echo-agent-app-core/src/chat_driver.rs`        |
| 多会话 Agent   | `AgentPool` 按 conversation 维护 Agent，忙会话不会被驱逐                    | `echo-agent-app-core/src/agent_pool.rs`         |
| 前台 turn 控制 | admission、steer、cancel、settlement 由 app-core 统一拥有                   | `echo-agent-app-core/src/foreground_turn.rs`    |
| 会话历史       | framework `FileConversationStore` 为权威，EKO 只做 workspace 绑定和 UI 投影 | `echo-agent-app-core/src/workspace/runtime.rs`  |
| 附件与长输入   | 上传文件、长粘贴和超预算文本落到 workspace artifact，再按引用读取           | `echo-agent-app-core/src/attachments.rs`        |
| 上下文压缩     | summary/sliding/adaptive 策略、手动压缩、usage/context 投影                 | `echo-agent-app-core/src/manual_compression.rs` |

## 任务与 Subagent

| 能力              | 当前实现                                                                             | 主要依据                                                         |
| ----------------- | ------------------------------------------------------------------------------------ | ---------------------------------------------------------------- |
| 统一任务关系      | `task_create/task_update/task_list/task_execute` 使用同一个 revisioned TaskRun graph | `echo-agent-app-core/src/tasks/task_runtime/register.rs`         |
| 动态 DAG          | 原子 plan、revision patch、claim、重试、取消、safe-point reload                      | `echo-agent-app-core/src/tasks/task_runtime/store.rs`            |
| 长程继续          | Goal、RunTurn、budget、provider retry、boot admission、checkpoint-backed hot state   | `echo-agent-app-core/src/tasks/task_runtime/continuation.rs`     |
| Subagent 执行     | direct、planned、fork、teammate、team 共用 prompt compiler 和结果合同                | `echo-agent-app-core/src/subagent_prompt.rs`                     |
| Subagent 控制     | message、follow-up、interrupt、attempt identity 与 durable result                    | `echo-agent-app-core/src/tasks/task_runtime/subagent_control.rs` |
| 后台 command cell | bounded async admission、typed cursor wait、owner cancel、boot orphan closure        | framework cell runtime + app-core `command_cells.rs`             |
| Worktree 隔离     | 逻辑任务复用、content-aware cleanup、review/merge/discard                            | `echo-agent-app-core/src/tasks/task_runtime/worktree.rs`         |

TaskRuntime 的框架/应用边界是稳定的：框架拥有 DAG、状态迁移和通用 task tools；
EKO 拥有文件投影、workspace、review、worktree、资源策略和各 surface 的呈现。
具体 authority、原子 settlement 和 blocking I/O 约束见
[RuntimeTaskService 适配决策](./architecture/runtime-task-service.md)。

## 工具与扩展

| 能力           | 当前实现                                                                                 | 主要依据                                               |
| -------------- | ---------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| 文件与代码修改 | canonical transactional `apply_patch`，不保留平行编辑工具                                | `echo-agent` tool registry                             |
| 代码执行与分析 | `run_code` 使用 EKO 锁定的 Python analytics runtime                                      | `echo-agent-app-core/src/analysis_runtime.rs`          |
| Terminal       | 用户交互 terminal session 和 Agent shell 路径分离                                        | `echo-agent-app-core/src/terminal.rs`                  |
| Browser/Chrome | 托管 Chromium、Chrome extension backend、tab/session/observation 投影                    | `echo-agent-app-core/src/browser/`                     |
| 工作流         | 一份 file-backed catalog 与 framework Graph executor；GUI/TUI/CLI/channel 共用服务       | `echo-agent-app-core/src/workflow_service.rs`          |
| 结构化抽取     | pooled Agent `extract_json`、JSON Schema 输入/输出验证、typed 多 surface outcome         | `echo-agent-app-core/src/structured_extraction.rs`     |
| MCP            | 一份用户 `mcp.json`、动态连接、plugin name ownership、resource tools                     | `echo-agent-app-core/src/mcp_config_runtime.rs`        |
| LSP            | 自动发现、诊断、定义、引用、hover 与 repo map                                            | framework LSP tools + app bootstrap                    |
| Tool output    | summary/detail 分离、opaque detail ref、cursor page、文件/JSONL 恢复                     | `echo-agent-app-core/src/tool_execution_projection.rs` |
| Hooks/Webhooks | 生命周期事件、command/Subagent/MCP actions、配置热重载                                   | `echo-agent-app-core/src/hook_config_loader.rs`        |
| Plugins        | flat `plugin.json` package、Skills/MCP/Subagents/Hooks/LSP/monitors/themes/output styles | `echo-agent-app-core/src/plugin_runtime.rs`            |
| Skills         | 递归发现、启停、上游检查与原子同步                                                       | `echo-agent-app-core/src/skills_hub/`                  |

## 专业工作台

| 能力         | 当前实现                                                                   | 主要依据                                                 |
| ------------ | -------------------------------------------------------------------------- | -------------------------------------------------------- |
| 数据分析     | file-backed analysis、脚本执行、数据/图表/report artifact                  | `echo-agent-app-core/src/analysis.rs`                    |
| 学术研究     | paper library、scholarly search、Zotero、Europe PMC、citation audit/export | `echo-agent-app-core/src/research.rs`                    |
| 医学研究     | PICO/PECO、screening、RoB、GRADE、PRISMA、适用性风险                       | `web-frontend/src/components/papers/ReviewWorkbench.tsx` |
| Sandbox      | GUI 已挂载配置、可用性检查和真实本地 sandbox execution                     | `src/tauri/commands/panels.rs`                           |
| 定时任务     | file-backed cron scheduler 与 GUI/TUI/CLI 控制                             | `echo-agent-app-core/src/scheduler/`                     |
| 记忆与自进化 | layered memory、Review Inbox、Curator、rule/Skill promotion、dashboard     | `echo-agent-app-core/src/evolution/`                     |

## 配置与可观测性

- 动态 Provider 和模型配置支持 Chat Completions、Responses、Anthropic 三种协议，
  以及 text/image/audio/video 输入能力。详见 [Provider 架构](./architecture/providers.md)。
- GUI、TUI 和 CLI 共享模型、thinking profile、permission mode、MCP、Plugin、Hook、
  Skill 和 TaskRuntime 权威。
- JSONL one-shot 可显式选择 interaction mode、permission/approval policy 和附件；HITL 请求
  进入 canonical event stream，不能回传的 input/selection 会明确拒绝而不静默挂起。
- Browser session、approval receipt、普通 Subagent event 和 workspace 删除投影均使用
  `(workspace, conversation)` 地址；同名 conversation 不会跨 workspace 混写或误删。
- Trace、usage/cache、context budget、Tool/Subagent execution 与 TaskRuntime event 均有
  typed projection；长输出按需加载，不进入首屏完整状态。
- 所有 EKO 数据以普通文件、JSON 或 JSONL 保存；应用不依赖 SQLite。

## 尚在收口

跨 workspace/conversation 的 host、精确 IPC/event identity、durable ordinary-input FIFO、
消息 journal、Agent groups、live delivery settlement、删除/eviction、boot recovery 和
TaskRuntime 进程级资源上限已接入生产路径并通过自动门禁。CommandCell/Awaiter 已共享进程资源
governor并拥有 typed terminal receipt，普通 conversation boot resume、dedicated Awaiter
surface projector、checkpoint-backed hot state 和 10k/100k 性能门已收口；剩余验收是 LH6
18 行故障矩阵、跨 surface real-provider probe、3x3 smoke、进程级
Agent/Subagent/shell/write/LLM governor 和 truthful self-retiring runner，现均已完成并通过。
10 分钟/2 小时与完整人工 GUI 场景统一属于项目研发完成后的 Final Integration Gate。

Workflow GUI 与结构化抽取的多 surface reachability 已收口；GUI palette 只展示有真实 handler
的命令。后续未完成工作只在 [`MASTER-PLAN`](./MASTER-PLAN.md) 维护，不在功能总览复制实施计划。
