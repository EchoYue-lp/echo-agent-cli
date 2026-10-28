# EKO 功能总览

本页按当前生产代码整理功能，不按历史设计文件整理。判定“已实现”要求定义存在、
已接入应用入口，并且可从至少一个真实 surface 调用；仅有类型、测试夹具或未注册模块
不计入。

GUI是当前唯一产品主入口；IM收发与cron后台触发继续保留。独立TUI、REPL、CLI/JSONL
及standalone channels入口已退出；GUI内置终端、Agent shell、MCP、Task/Subagent、附件、
记忆和共享恢复继续原owner。历史来源枚举不表示仍有独立产品入口。源码可达、focused、
native与发布各自验收，文稿同步不把保留能力统称“全验收通过”。

## Agent 与会话

聊天 Token 不再根据旧 ThinkStart/ThinkEnd 改写为 Reasoning。运行等待行显示实际
模型或工具操作、耗时与最后事件时间；UI 计时器不会生成助手正文或完成事实。
原生消息分层与60秒安全点报告策略按各自真实源码和回执验收；已有接缝/focused不代新版原生验收，
详见 [ADR 0057](./adr/0057-native-messages-and-progress-policy.md)。

| 能力           | 当前实现                                                                                                                                                                                                                                 | 主要依据                                                                                                                 |
| -------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| 流式对话       | GUI与channel共享 `drive_chat` 与 typed `TurnOutcome`                                                                                                                                                                                     | `echo-agent-app-core/src/chat_driver.rs`                                                                                 |
| 多会话 Agent   | `AgentPool` 按 conversation 维护 Agent，忙会话不会被驱逐                                                                                                                                                                                 | `echo-agent-app-core/src/agent_pool/pool.rs`                                                                             |
| Channel 会话   | app-core 原 owner 管理 sender-scoped 产品 ID 与 incarnation Agent/checkpoint/cache ID，reset 保留历史                                                                                                                                    | `echo-agent-app-core/src/channel_runtime.rs`、`channel_runtime/input_pump.rs`                                            |
| 前台 turn 控制 | admission、steer、cancel、settlement 由 app-core 统一拥有                                                                                                                                                                                | `echo-agent-app-core/src/foreground_turn.rs`                                                                             |
| 会话追加输入   | 保留交互surface共用 durable frontier、tracked drain 与 exact terminal                                                                                                                                                                    | `echo-agent-app-core/src/conversation_input.rs`                                                                          |
| 会话历史       | framework `FileConversationStore` 为权威，EKO 只做 workspace 绑定和 UI 投影                                                                                                                                                              | `echo-agent-app-core/src/workspace/runtime.rs`                                                                           |
| 分叉会话       | GUI 完成回复下方提供“复制 / 重新生成 / 分叉会话”；Fork 包含该回复所在的完整回合和工具记录，打开带 `(branch)` 标题的独立普通会话，刷新当前工作区列表并保留原会话；后续回合及隐藏内部消息不复制。编辑/重新生成仍在目标用户回合前分叉并重发 | `src/tauri/commands/conversations.rs`、`web-frontend/src/components/chat/ChatPanel.tsx`                                  |
| 支线对话       | GUI 专属侧栏布局；committed context 快照、一级 Subagent 关系、独立模型/turn 与隐藏内部消息，channel无专用入口                                                                                                                            | `echo-agent-app-core/src/side_conversation.rs`、`web-frontend/src/components/chat/SideConversationDialog.tsx`            |
| 会话归档       | 侧边栏只显示未归档会话；“设置 -> 项目数据 -> 归档”可恢复或永久删除                                                                                                                                                                       | `echo-agent-app-core/src/conversation_archive.rs`、`web-frontend/src/components/sessions/ArchivedConversationsPanel.tsx` |
| 附件与长输入   | 上传文件、长粘贴和超预算文本落到 workspace artifact，再按引用读取                                                                                                                                                                        | `echo-agent-app-core/src/attachments.rs`                                                                                 |
| 上下文压缩     | summary/hybrid/sliding/adaptive 策略、默认 token 近期窗口、跨 surface focus/cancel 与 goal/recovery 投影                                                                                                                                 | `echo-agent-app-core/src/manual_compression.rs`                                                                          |

GUI 聊天正文沿同一内容列直接对齐，不再为助手消息预留机器人头像列。历史中没有正文、
附件、思考、已恢复工具步骤、关联 Subagent 或内部消息标记的空记录不占消息行；流式
等待仍由聊天页既有状态行呈现，正文到达后继续显示原内容与完成操作。工具、思考和
Subagent 继续使用原有可展开执行过程，附件、错误、内部消息标记、复制、重新生成及分叉
入口保持可见或按原状态开放。界面整理沿用原历史与工具状态恢复，不推断缺失的执行结果。

工具历史保留原始调用、请求参数和记录正文，执行状态以同一工作空间、会话和消息的
真实摘要为依据。缺少精确状态时明确显示“历史执行状态未恢复”，不按输出文字猜成功
或失败。未关联的工具结果保留独立工具记录，可展开、复制原文，不作为助手回复参与
分叉或 Subagent 归属。损坏记录加载失败时保留当前对话与原文件，并提示重试。

## 任务与 Subagent

| 能力              | 当前实现                                                                                                                 | 主要依据                                                         |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------- |
| 统一任务关系      | `task_create/task_update/task_list/task_execute` 使用同一个 revisioned TaskRun graph                                     | `echo-agent-app-core/src/tasks/task_runtime/register.rs`         |
| 动态 DAG          | 原子 plan、revision patch、claim、重试、取消、safe-point reload                                                          | `echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs`    |
| 长程继续          | Goal、RunTurn、budget、provider retry、boot admission、checkpoint-backed hot state                                       | `echo-agent-app-core/src/tasks/task_runtime/continuation.rs`     |
| Subagent 执行     | direct、planned、fork、teammate、team 共用 prompt compiler；typed message、有效工具面和 workspace 由 invocation 统一编译 | `echo-agent-app-core/src/subagent_prompt.rs`                     |
| Subagent 控制     | message、follow-up、interrupt、attempt identity 与 durable outcome                                                       | `echo-agent-app-core/src/tasks/task_runtime/subagent_control.rs` |
| 后台 command cell | bounded async admission、typed cursor wait、owner cancel、boot orphan closure                                            | framework cell runtime + app-core `command_cells.rs`             |
| Worktree 隔离     | 逻辑任务复用、content-aware cleanup、review/merge/discard                                                                | `echo-agent-app-core/src/tasks/task_runtime/worktree.rs`         |

插件 Subagent 同样使用 `EkoSubagentPromptCompiler` 并共享 framework tool visibility policy；
注册期 prompt 声明 concrete tool surface，invocation 再合并有效 allowlist、workspace 与 typed
附件，fork/team/teammate 只继承过滤后的 user 与 final-assistant 消息。TaskRuntime 的
可选 follow-up JSON 复用 framework framing，不在应用层重复解析 Markdown fence。

TaskRuntime 的框架/应用边界是稳定的：框架拥有 DAG、状态迁移和通用 task tools；
EKO 拥有文件投影、workspace、review、worktree、资源策略和各 surface 的呈现。
具体 authority、原子 settlement 和 blocking I/O 约束见
[RuntimeTaskService 适配决策](./architecture/runtime.md)。
Task 依赖只存在于单个 revisioned TaskRun 的 `PlanRevision.tasks[].depends_on`；background
launcher不维护跨TaskRun metadata DAG或轮询器。依赖图由同一TaskRuntime/框架任务工具读取；独立CLI的`/tasks dag`不再是当前入口，不据此新增GUI图编辑器。

## 工具与扩展

| 能力             | 当前实现                                                                                                                          | 主要依据                                               |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| 文件与代码修改   | canonical transactional `apply_patch`，不保留平行编辑工具                                                                         | `echo-agent` tool registry                             |
| Workspace diff   | 显式 workspace generation、已验证 Git ref、app-core 统一结构化 hunk，Tauri 仅映射 wire DTO                                        | `echo-agent-app-core/src/diff.rs`                      |
| 代码执行与分析   | `run_code` 使用 EKO 锁定的 Python analytics runtime                                                                               | `echo-agent-app-core/src/analysis_runtime.rs`          |
| Terminal         | 本地/系统 OpenSSH 交互终端与 Agent shell 分离；源码已准备，原生/远端待验                                                          | `echo-agent-app-core/src/terminal.rs`                  |
| Browser/Chrome   | 托管 Chromium、Chrome extension backend、tab/session/observation 与保留入口／Subagent的同owner控制                                | Browser runtime + Extension dispatcher                 |
| 工作流           | 一份 file-backed catalog 与 framework Graph executor；GUI/channel 共用服务                                                        | `echo-agent-app-core/src/workflow_service.rs`          |
| 结构化抽取       | pooled Agent `extract_json`、JSON Schema 输入/输出验证、typed 多 surface outcome                                                  | `echo-agent-app-core/src/structured_extraction.rs`     |
| MCP              | 一份用户 `mcp.json`、真实 reconcile、scope-keyed health、plugin name ownership                                                    | `McpConfigRuntime` + Extension control                 |
| Tool output      | summary/detail 分离、opaque detail ref、cursor page、文件/JSONL 恢复                                                              | `echo-agent-app-core/src/tool_execution_projection.rs` |
| Agent 协作控制面 | discriminated Conversation/TaskSubagent target、bounded list/inspect/message/followup/wait/interrupt                              | `echo-agent-app-core/src/agent_control.rs`             |
| Hooks/Webhooks   | 生命周期事件、command/Subagent/MCP actions、配置热重载                                                                            | `echo-agent-app-core/src/hook_config_loader.rs`        |
| Plugins          | framework prepared generation、captured target fanout、typed generation receipt、Subagents/scope-qualified monitors/themes/styles | `echo-agent-app-core/src/plugin_runtime/runtime.rs`    |
| Skills           | 递归发现、安装、启停、upstream staging sync、flat policy 原子写与即时 reconcile                                                   | `skills_hub/` + `extension_control/skills.rs`          |

### Extension Control Authority

Agent 间通信和协作组编排由 AI 使用工具自动完成，GUI 不提供人工消息、组管理或协作动态控制台。用户通过 Sidechat/Forkchat 参与独立探索，现有对话与任务/Subagent 视图继续呈现执行和结果；模型工具、消息结算及其它 surface 的控制能力保留。

`ExtensionControlService` 是 Skills、Plugins、MCP、Hooks、Browser 的 EKO mutation
admission，并把真实执行委托给既有 specialist owner。它不建立第二套 registry、manager 或
store。

- Skill 使用 version 3 `{enabled}` flat policy、atomic commit、typed settled/degraded receipt 与
  caller-drop owned settlement；GUI/headless boot、workspace load 和下一 mutation 都重新读取
  policy 并 reconcile，不保留 repair debt。
- GUI使用generated typed generic IPC，channel使用同一app-core authority与terminal settlement；
  canonical journal/replay仍保留，但不再提供独立JSONL stdout产品。
- Browser的保留命令面沿GUI与channel的原权限、session和owner可达；不保留console控制器。
- MCP health 由 Extension control 按 authority scope 维护；Hook project identity 来自
  captured workspace root，不使用 process cwd。
- Plugin portable components 只由 framework prepare/apply；EKO mutation 使用一次捕获的
  workspace/ABA target cut。cold workspace 继承 committed framework generation，monitor key 按 target scope 隔离；所有 surface 收到 overall
  status 以及 previous/candidate generation 的逐 target typed receipt。

完整合同见 [Extension Control ADR](./adr/0012-extension-control-authority.md) 和
[framework prepared Plugin generation ADR](./adr/0022-framework-prepared-plugin-generations.md)。

## 专业工作台

| 能力         | 当前实现                                                                                                                       | 主要依据                                                                      |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------- |
| 数据分析     | file-backed analysis、脚本执行、数据/图表/report artifact                                                                      | `echo-agent-app-core/src/analysis.rs`                                         |
| 学术研究     | paper library、scholarly search、Zotero、Europe PMC、citation audit/export                                                     | `echo-agent-app-core/src/research.rs`                                         |
| 医学研究     | PICO/PECO、screening、RoB、GRADE、PRISMA、适用性风险                                                                           | `web-frontend/src/components/papers/ReviewWorkbench.tsx`                      |
| Sandbox      | GUI 已挂载配置、可用性检查和真实本地 sandbox execution                                                                         | `src/tauri/commands/panels.rs`                                                |
| 定时任务     | file-backed cron scheduler、GUI与保留渠道控制与原样 prompt                                                                     | `echo-agent-app-core/src/scheduler/`                                          |
| 记忆与自进化 | generation-bound layered memory、safe-point hot projection、`/reflect`、Review Inbox、Curator、rule/Skill promotion、dashboard | `echo-agent-app-core/src/evolution/`、`echo-agent-app-core/src/reflection.rs` |

GUI 设置已挂载渠道与定时任务页面，配置、控制及历史继续使用原 ChannelRuntime、scheduler
和 TaskRuntime 权威。IM handler由app-core的ChannelRuntime及原management/input-pump/outbound拥有；这不表示真实平台
收发、GUI/IM 人工请求组合、隐藏/退出/重启及完整产品验收均已通过。边界见
[IM/cron owner 决策](./adr/0053-im-cron-gui-control.md)。

## 配置与可观测性

- 动态 Provider 和模型配置支持 Chat Completions、Responses、Anthropic 三种协议，
  以及 text/image/audio/video 输入能力。详见 [Provider 架构](./architecture/providers.md)。
- GUI、IM与cron共享模型、thinking profile、permission mode、
  TaskRuntime 与 Extension authority。
- 桌面与IM的权限、审批和附件沿原绑定；cron保持原执行策略，不从入口退役获得新许可。
  HITL请求/回答仍按原配对身份处理，取消或过期不补授权；不保留JSONL one-shot控制器。
- Browser session、approval receipt、普通 Subagent event 和 workspace 删除投影均使用
  `(workspace, conversation)` 地址；同名 conversation 不会跨 workspace 混写或误删。
- Trace、usage/cache、context budget、Tool/Subagent execution 与 TaskRuntime event 均有
  typed projection；长输出按需加载，不进入首屏完整状态。
- 所有 EKO 数据以普通文件、JSON 或 JSONL 保存；应用不依赖 SQLite。

设置页使用统一的表单字号、间距、控件高度与错误反馈，基础配置常显，QQ、飞书和个人微信
继续在独立 IM 渠道页管理。模型编辑沿原弹窗设置初始焦点，按 Esc 关闭后回到触发按钮；
取消和保存失败保留原来的草稿与写入规则。界面排版调整不改变配置含义、默认值或保存接口。

## CommandCell 观察

`watch_cell` 启动有界的 framework deterministic watcher，并立即返回 EKO durable receipt。
watcher retained cell、按 typed cursor drain 到真实终态，再向全部 surface 发布唯一 Ready fact；它不
派发 Subagent，也不依赖 model/provider output。`interrupt_command_cell_watch` 只取消观察意图，
绝不隐式停止 command 本身。

EKO 不再随应用捆绑 Skill，也不维护默认启用集、方法论 baseline 或内置 catalog。
SkillsHub 管理用户安装到 `~/.eko/skills/` 的独立 Skill；当前项目
`<project>/.eko/skills/` 由项目运行时直接发现；Plugin 中的 Skill 由插件 generation 提供。
`enabled-skills.json` 只记录用户 Skill 的启用选择，禁用项不会注册
progressive activation entry 或 IntentRouter 候选。

外部 `SKILL.md` 仍由 framework 的 agentskills.io 标准解析器和 validator 统一处理，
Hooks 继续属于 application/plugin configuration。仓库根 `.agents/skills/` 是开发本项目时
使用的 Agent 指导，不属于 EKO 产品运行时；当前项目 `AGENTS.md` 会作为项目 instruction
projection 参与每个 Agent。详见 ADR
[0041](./adr/0041-external-only-skills.md)。

### 退役的 LSP 消费者

EKO 不再请求框架 LSP feature、注册语言服务器工具或暴露 LSP 命令/设置/IPC。
旧配置与插件声明给出明确退休诊断，不删除用户文件；混合插件保留合法组件及原发布/清理
收据。框架 A2A/LSP 的删除与组合验证按各自仓库独立验收。EKO 的独立 TUI、REPL、
CLI/JSONL 产品入口也已退出；GUI 内置终端、Agent shell、IM、cron、MCP 与 Subagent
继续保留。源码退出不代表本轮删后 Rust、原生或发布验收已完成。

### 自动协作通信的时间线呈现

AI向其它会话发送的消息显示紧凑发送记录，typed目标可点击打开。来自其它Agent的输入显示来源、原始正文与投递观察，右对齐并支持展开全文；这不会赋予用户身份、Goal权限或本人编辑操作。输入来源由当前写路径耐久保存，新版本重载与Fork继续保留，旧wrapper不做兼容解码。这里只呈现既有自动协作事实，不提供手工收件人、组或收据管理页面。
