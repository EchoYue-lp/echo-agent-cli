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
| 长程继续          | Goal、RunTurn、budget、provider retry、boot admission、checkpoint projection         | `echo-agent-app-core/src/tasks/task_runtime/continuation.rs`     |
| Subagent 执行     | direct、planned、fork、teammate、team 共用 prompt compiler 和结果合同                | `echo-agent-app-core/src/subagent_prompt.rs`                     |
| Subagent 控制     | message、follow-up、interrupt、attempt identity 与 durable result                    | `echo-agent-app-core/src/tasks/task_runtime/subagent_control.rs` |
| 后台 command cell | bounded async admission、typed cursor wait、owner cancel、boot orphan closure        | framework cell runtime + app-core `command_cells.rs`             |
| Worktree 隔离     | 逻辑任务复用、content-aware cleanup、review/merge/discard                            | `echo-agent-app-core/src/tasks/task_runtime/worktree.rs`         |

TaskRuntime 的框架/应用边界是稳定的：框架拥有 DAG、状态迁移和通用 task tools；
EKO 拥有文件投影、workspace、review、worktree、资源策略和各 surface 的呈现。

## 工具与扩展

| 能力           | 当前实现                                                                                 | 主要依据                                               |
| -------------- | ---------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| 文件与代码修改 | canonical transactional `apply_patch`，不保留平行编辑工具                                | `echo-agent` tool registry                             |
| 代码执行与分析 | `run_code` 使用 EKO 锁定的 Python analytics runtime                                      | `echo-agent-app-core/src/analysis_runtime.rs`          |
| Terminal       | 用户交互 terminal session 和 Agent shell 路径分离                                        | `echo-agent-app-core/src/terminal.rs`                  |
| Browser/Chrome | 托管 Chromium、Chrome extension backend、tab/session/observation 投影                    | `echo-agent-app-core/src/browser/`                     |
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
- Trace、usage/cache、context budget、Tool/Subagent execution 与 TaskRuntime event 均有
  typed projection；长输出按需加载，不进入首屏完整状态。
- 所有 EKO 数据以普通文件、JSON 或 JSONL 保存；应用不依赖 SQLite。

## 尚在收口

跨 workspace/conversation 的 host、精确 IPC/event identity、durable ordinary-input FIFO、
消息 journal、Agent groups、live delivery settlement、删除/eviction、boot recovery 和
TaskRuntime 进程级资源上限已接入生产路径并通过自动门禁。端到端可靠性仍未最终验收：
真实 GUI 证据和两小时多 workspace soak 尚未记录；CommandCell/Awaiter 还需把同一进程级
资源策略扩展到 cell/Awaiter 路径。

这些未完成项由
[`design/specs/runtime-reliability.md`](../design/specs/runtime-reliability.md)
跟踪。CommandCell/Awaiter、普通 conversation boot resume、terminal repair 和 hot-state
performance 的详细修复由
[`design/specs/long-horizon-runtime-closure.md`](../design/specs/long-horizon-runtime-closure.md)
跟踪。另有两个非运行时 reachability 缺口：Workflow GUI 未挂载，结构化抽取没有统一
多 surface 服务；它们记录在
[`design/specs/surface-parity.md`](../design/specs/surface-parity.md)。`docs/` 不维护第二份
实施计划。
