# EKO TUI 对标与验收

日期: 2026-07-11

## 目标与边界

EKO TUI 是与 GUI 共用 Agent、TaskRuntime、memory、skills、MCP、hooks 和 HITL 的完整产品入口,不是轻量 REPL。本次“对标”指覆盖 Claude Code、Codex 等成熟终端 Agent 的通用工作流;厂商专属云功能和命令名称不要求逐字复制。

实现放在 `echo-agent-cli` 应用层。输入编辑、会话选择、终端模式和运行时投影依赖 EKO 产品交互,不应污染通用 `echo-agent` 框架。唯一共享逻辑是应用层 `StoredMessage` 恢复器,供启动参数和 TUI 共用。

## 业界依据

### Claude Code

参考官方 [Interactive mode](https://code.claude.com/docs/en/interactive-mode):

- `Esc`/`Ctrl+C` 抵达执行层并中断当前响应;双击 Esc 回退最近一轮。
- Shift+Enter/Ctrl+J 多行输入,Ctrl+G 外部编辑器,Ctrl+R 反向历史。
- `!` shell mode、`@` 文件引用、图片粘贴、task list、模型/权限切换。
- MCP、skills/hooks 和后台 subagent 是同一终端产品的一等能力。

### Codex CLI

参考本机 Codex CLI 帮助及官方产品行为:

- 持久会话支持 resume/fork/delete,启动参数可直接恢复指定会话。
- 图片附件、approval/sandbox 模式和运行中断属于主流程。
- `--no-alt-screen` 保留原生终端 scrollback,适配 tmux 和终端历史。

### Hermes / OpenCode / OpenClaw

本机参考实现显示的跨产品共识:

- agent 忙碌时输入进入 FIFO 队列,不能静默丢失。
- composer 高度随多行内容增长,session/model 可搜索选择。
- task/todo 与 subagent 树必须实时投影,不能只显示一条状态字符串。
- UI 标签必须反映真实 runtime 配置,不能只改前端字段。

## 完整验收矩阵

| 能力 | EKO 实现 | 状态 |
|---|---|---|
| 真实中断 | 每轮独立 `CancellationToken`;Esc/Ctrl+C 取消并等待收敛 | 完成 |
| 双 Esc rewind | 删除最后一轮投影、恢复 Agent context、原输入回填 composer | 完成 |
| 忙碌输入队列 | 文本、附件、InteractionMode 一并冻结到 FIFO | 完成 |
| same-turn steer | `/steer` 注入当前 turn;不可 steer 时保留为 FIFO follow-up | 完成 |
| 多行编辑 | 动态 8 行视窗,Shift+Enter/Ctrl+J,行/词级编辑 | 完成 |
| 外部编辑器 | Ctrl+G 暂停 TUI并调用 `$VISUAL`/`$EDITOR` | 完成 |
| 历史搜索 | Ctrl+R 保留查询并向更早匹配遍历 | 完成 |
| 文件引用 | `@query` + Tab 在项目文件索引中补全 | 完成 |
| Shell mode | `!command` 本地执行并记录 stdout/stderr/exit code | 完成 |
| 附件 | `/attach`,Bracketed Paste 文本,Ctrl+V 剪贴板图片 | 完成 |
| transcript | Ctrl+O 展开/折叠 assistant 工具细节 | 完成 |
| terminal scrollback | `--no-alt-screen` | 完成 |
| 会话持久化 | GUI/TUI/启动参数统一 FileConversationStore,无 SQLite | 完成 |
| 会话生命周期 | `/new`, `/sessions [query]`, `/resume`, `/fork`, `/rename`, `/delete-session`; `--continue/--resume` | 完成 |
| 模型切换 | 列出 configured models,同步 LLM config/token/temperature/context 到 AgentPool | 完成 |
| thinking | `/think` 读取/设置真实 `ThinkingConfig` | 完成 |
| 权限 | `/permission` 同步主 Agent 与 AgentPool | 完成 |
| plan mode | 调用真实 `set_plan_mode`,由工具层阻断写操作 | 完成 |
| TaskRuntime | 权威 run/plan/todo 投影,取消/暂停/恢复 | 完成 |
| subagent | event bus 实时状态、tool count、tokens、duration | 完成 |
| HITL | TUI provider 支持批准、拒绝、修改、session scope | 完成 |
| memory | `/memory`, `/remember`, `/forget`, review/candidates | 完成 |
| MCP | `/mcp list/load/disconnect`,工具即时注册/移除 | 完成 |
| skills | `/skills list/search/info/install/uninstall/refresh`,安装后热加载 | 完成 |
| hooks | `/hooks list/reload/test` | 完成 |
| 工具命令 | test/review/diff/git/pipeline/cron 走正式 Agent turn和统一审批/工具链 | 完成 |
| 状态真实性 | model/permission/tools/token/request 均读取真实 runtime | 完成 |

## 关键数据流

1. 启动前确定唯一 `conversation_id`,同时注入 Agent config、ConversationStore、RuntimeStateStore 和 TUI。
2. `drive_chat` 仍是 GUI/TUI 共用驱动;TUI 只负责输入、事件渲染和产品命令。
3. run 收尾由框架投影用户可见 transcript;TUI 不并行维护第二份 session JSON。
4. resume/rewind/fork 从 FileConversationStore 读取 `StoredMessage`,经应用层恢复器重建 tool-call 关联后加载 Agent context。
5. TaskRuntime 与 subagent 都是只读 UI 投影;取消/暂停/恢复调用现有应用层 store API。

## 删除的重复机制

旧 `SessionManager`、`Session` types 和 exporter 没有运行时写入点,启动 resume 却曾从中读取,形成不可闭环的第二套会话系统。本次删除该死路径,会话权威统一为 FileConversationStore。
