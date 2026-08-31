# EKO 功能总览

本页按当前生产代码整理功能，不按历史设计文件整理。判定“已实现”要求定义存在、
已接入应用入口，并且可从至少一个真实 surface 调用；仅有类型、测试夹具或未注册模块
不计入。

## Agent 与会话

| 能力           | 当前实现                                                                      | 主要依据                                        |
| -------------- | ----------------------------------------------------------------------------- | ----------------------------------------------- |
| 流式对话       | TUI、GUI、CLI/JSONL、channel 共享 `drive_chat` 与 typed `TurnOutcome`         | `echo-agent-app-core/src/chat_driver.rs`        |
| 多会话 Agent   | `AgentPool` 按 conversation 维护 Agent，忙会话不会被驱逐                      | `echo-agent-app-core/src/agent_pool/pool.rs`         |
| Channel 会话   | sender-scoped 产品 ID + incarnation Agent/checkpoint/cache ID，reset 保留历史 | `src/cli/channels.rs`                           |
| 前台 turn 控制 | admission、steer、cancel、settlement 由 app-core 统一拥有                     | `echo-agent-app-core/src/foreground_turn.rs`    |
| 会话追加输入   | 四个交互 surface 共用 durable frontier、tracked drain 与 exact terminal       | `echo-agent-app-core/src/conversation_input.rs` |
| 会话历史       | framework `FileConversationStore` 为权威，EKO 只做 workspace 绑定和 UI 投影   | `echo-agent-app-core/src/workspace/runtime.rs`  |
| 附件与长输入   | 上传文件、长粘贴和超预算文本落到 workspace artifact，再按引用读取             | `echo-agent-app-core/src/attachments.rs`        |
| 上下文压缩     | summary/sliding/adaptive 策略、手动压缩、usage/context 投影                   | `echo-agent-app-core/src/manual_compression.rs` |

## 任务与 Subagent

| 能力              | 当前实现                                                                             | 主要依据                                                         |
| ----------------- | ------------------------------------------------------------------------------------ | ---------------------------------------------------------------- |
| 统一任务关系      | `task_create/task_update/task_list/task_execute` 使用同一个 revisioned TaskRun graph | `echo-agent-app-core/src/tasks/task_runtime/register.rs`         |
| 动态 DAG          | 原子 plan、revision patch、claim、重试、取消、safe-point reload                      | `echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs`            |
| 长程继续          | Goal、RunTurn、budget、provider retry、boot admission、checkpoint-backed hot state   | `echo-agent-app-core/src/tasks/task_runtime/continuation.rs`     |
| Subagent 执行     | direct、planned、fork、teammate、team 共用 prompt compiler；typed message、有效工具面和 workspace 由 invocation 统一编译 | `echo-agent-app-core/src/subagent_prompt.rs`                     |
| Subagent 控制     | message、follow-up、interrupt、attempt identity 与 durable outcome                  | `echo-agent-app-core/src/tasks/task_runtime/subagent_control.rs` |
| 后台 command cell | bounded async admission、typed cursor wait、owner cancel、boot orphan closure        | framework cell runtime + app-core `command_cells.rs`             |
| Worktree 隔离     | 逻辑任务复用、content-aware cleanup、review/merge/discard                            | `echo-agent-app-core/src/tasks/task_runtime/worktree.rs`         |

插件 Subagent 同样使用 `EkoSubagentPromptCompiler` 并共享 framework tool visibility policy；
注册期 prompt 声明 concrete tool surface，invocation 再合并有效 allowlist、workspace 与 typed
附件，fork/team/teammate 只继承过滤后的 user 与 final-assistant 消息。TaskRuntime 的
可选 follow-up JSON 复用 framework framing，不在应用层重复解析 Markdown fence。

TaskRuntime 的框架/应用边界是稳定的：框架拥有 DAG、状态迁移和通用 task tools；
EKO 拥有文件投影、workspace、review、worktree、资源策略和各 surface 的呈现。
具体 authority、原子 settlement 和 blocking I/O 约束见
[RuntimeTaskService 适配决策](./architecture/runtime.md)。
Task 依赖只存在于单个 revisioned TaskRun 的 `PlanRevision.tasks[].depends_on`；background
launcher 不再维护跨 TaskRun metadata DAG 或轮询器。CLI 使用 `/tasks dag <run-id>` 查看该权威图。

## 工具与扩展

| 能力           | 当前实现                                                                                 | 主要依据                                               |
| -------------- | ---------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| 文件与代码修改 | canonical transactional `apply_patch`，不保留平行编辑工具                                | `echo-agent` tool registry                             |
| Workspace diff | 显式 workspace generation、已验证 Git ref、app-core 统一结构化 hunk，Tauri 仅映射 wire DTO | `echo-agent-app-core/src/diff.rs`                      |
| 代码执行与分析 | `run_code` 使用 EKO 锁定的 Python analytics runtime                                      | `echo-agent-app-core/src/analysis_runtime.rs`          |
| Terminal       | 用户交互 terminal session 和 Agent shell 路径分离                                        | `echo-agent-app-core/src/terminal.rs`                  |
| Browser/Chrome | 托管 Chromium、Chrome extension backend、tab/session/observation 与五入口控制            | Browser runtime + Extension dispatcher                 |
| 工作流         | 一份 file-backed catalog 与 framework Graph executor；GUI/TUI/CLI/channel 共用服务       | `echo-agent-app-core/src/workflow_service.rs`          |
| 结构化抽取     | pooled Agent `extract_json`、JSON Schema 输入/输出验证、typed 多 surface outcome         | `echo-agent-app-core/src/structured_extraction.rs`     |
| MCP            | 一份用户 `mcp.json`、真实 reconcile、scope-keyed health、plugin name ownership           | `McpConfigRuntime` + Extension control                 |
| LSP            | 自动发现、诊断、定义、引用、hover、repo map、五入口控制与配置热重载                      | framework LSP tools + `ExtensionControlService`        |
| Tool output    | summary/detail 分离、opaque detail ref、cursor page、文件/JSONL 恢复                     | `echo-agent-app-core/src/tool_execution_projection.rs` |
| Agent 协作控制面 | discriminated Conversation/TaskSubagent target、bounded list/inspect/message/followup/wait/interrupt | `echo-agent-app-core/src/agent_control.rs` |
| Hooks/Webhooks | 生命周期事件、command/Subagent/MCP actions、配置热重载                                   | `echo-agent-app-core/src/hook_config_loader.rs`        |
| Plugins        | framework prepared generation、captured target fanout、typed generation receipt、Subagents/LSP/scope-qualified monitors/themes/styles | `echo-agent-app-core/src/plugin_runtime/runtime.rs` |
| Skills         | 递归发现、安装、启停、upstream staging sync 与 durable desired/settled generation        | `skills_hub/` + `extension_control/skills.rs`                 |

### Extension Control Authority

`ExtensionControlService` 是 Skills、Plugins、MCP、Hooks、LSP、Browser 的 EKO mutation
admission，并把真实执行委托给既有 specialist owner。它不建立第二套 registry、manager 或
store。

- Skill 使用 v2 durable desired/settled generation、atomic commit、typed degraded/debt
  receipt 与 caller-drop owned settlement；GUI/headless boot、workspace load 和下一 mutation
  都重放 debt。
- GUI 使用 generated typed generic IPC；JSONL 输出 journaled typed `ExtensionReceipt` 且不
  进入模型；CLI、TUI、channel 使用同一 app-core authority 并保留 terminal settlement。
- Browser 与 LSP 的完整命令面在 GUI、TUI、CLI/JSONL 和 channel 可达。
- MCP health 由 Extension control 按 authority scope 维护；Hook/LSP project identity 来自
  captured workspace root，不使用 process cwd。
- Plugin portable components 只由 framework prepare/apply；EKO mutation 使用一次捕获的
  workspace/ABA target cut。cold workspace 继承 committed framework generation，LSP config
  更新不再触发完整 Plugin reload，monitor key 按 target scope 隔离；所有 surface 收到 overall
  status 以及 previous/candidate generation 的逐 target typed receipt。

完整合同见 [Extension Control ADR](./adr/0012-extension-control-authority.md) 和
[framework prepared Plugin generation ADR](./adr/0022-framework-prepared-plugin-generations.md)。

## 专业工作台

| 能力         | 当前实现                                                                   | 主要依据                                                 |
| ------------ | -------------------------------------------------------------------------- | -------------------------------------------------------- |
| 数据分析     | file-backed analysis、脚本执行、数据/图表/report artifact                  | `echo-agent-app-core/src/analysis.rs`                    |
| 学术研究     | paper library、scholarly search、Zotero、Europe PMC、citation audit/export | `echo-agent-app-core/src/research.rs`                    |
| 医学研究     | PICO/PECO、screening、RoB、GRADE、PRISMA、适用性风险                       | `web-frontend/src/components/papers/ReviewWorkbench.tsx` |
| Sandbox      | GUI 已挂载配置、可用性检查和真实本地 sandbox execution                     | `src/tauri/commands/panels.rs`                           |
| 定时任务     | file-backed cron scheduler、GUI/TUI/CLI 控制与原样 prompt                  | `echo-agent-app-core/src/scheduler/`                     |
| 记忆与自进化 | generation-bound layered memory、safe-point hot projection、`/reflect`、Review Inbox、Curator、rule/Skill promotion、dashboard | `echo-agent-app-core/src/evolution/`、`echo-agent-app-core/src/reflection.rs` |

## 配置与可观测性

- 动态 Provider 和模型配置支持 Chat Completions、Responses、Anthropic 三种协议，
  以及 text/image/audio/video 输入能力。详见 [Provider 架构](./architecture/providers.md)。
- GUI、TUI、CLI/JSONL 和 channel 共享模型、thinking profile、permission mode、
  TaskRuntime 与 Extension authority。
- JSONL one-shot 可显式选择 permission/approval policy 和附件；HITL 请求
  进入 canonical event stream，不能回传的 input/selection 会明确拒绝而不静默挂起。
- Browser session、approval receipt、普通 Subagent event 和 workspace 删除投影均使用
  `(workspace, conversation)` 地址；同名 conversation 不会跨 workspace 混写或误删。
- Trace、usage/cache、context budget、Tool/Subagent execution 与 TaskRuntime event 均有
  typed projection；长输出按需加载，不进入首屏完整状态。
- 所有 EKO 数据以普通文件、JSON 或 JSONL 保存；应用不依赖 SQLite。

## CommandCell 观察

`watch_cell` 启动有界的 framework deterministic watcher，并立即返回 EKO durable receipt。
watcher retained cell、按 typed cursor drain 到真实终态，再向全部 surface 发布唯一 Ready fact；它不
派发 Subagent，也不依赖 model/provider output。`interrupt_command_cell_watch` 只取消观察意图，
绝不隐式停止 command 本身。

内置 Skill 采用 catalog 与 runtime 分离：SkillsHub 可以列出和安装全部随附产物，
`enabled-skills.json` 决定哪些 bundled descriptor 能进入 Agent。disabled Skill 不会贡献
私有 Hook 扩展、progressive activation entry 或 IntentRouter 候选。

捆绑 Skill 的 `SKILL.md` 只使用 agentskills.io 官方标准字段（`name` / `description` /
`license` / `compatibility` / `metadata` 字符串映射 / 空格分隔的 `allowed-tools`），不引入
任何私有扩展命名空间；LLM routing 是 description-driven。Skill 文件不携带私有 Hook 文件，
Hooks 继续由 application/plugin configuration 负责。
framework 提供 `validate_skill_dir`（`skills-ref validate` 的进程内等价物），catalog gate
测试遍历 `skills/` 强制零违规并保证 `BUILTIN_SKILL_NAMES` 与磁盘一致。详见 ADR
[0033](./adr/0033-skill-catalog-contraction-and-official-frontmatter.md)。
