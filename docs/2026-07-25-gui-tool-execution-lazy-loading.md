# GUI Tool Execution Lazy Loading

日期: 2026-07-25

状态: 已实现并通过提交门禁; selector 稳定性热修 `b8c9077`

## 目标

EKO GUI 不再把主 Agent 或 Subagent 的完整工具参数、stdout、stderr 和
结构化结果塞进聊天消息、Zustand 热状态或初始 DOM。主 Agent 与 Subagent
内部工具共用一套持久化、事件、查询和渲染逻辑:

- 默认只显示一行:工具名称、UTF-8 安全的参数摘要、running/success/failed/
  cancelled 状态和耗时。
- 用户展开后,通过不透明 `detail_ref` 按需读取完整参数和输出。
- Subagent 仍保留完整提示词、工具执行过程和完整最终结果三个区域。
- EKO 产品中的 Subagent 为单层执行者;通用 `echo-agent` 框架仍保留可嵌套能力。
- 全部数据使用本地文件和 JSONL,不引入 SQLite。

## 业界依据与取舍

- [OpenAI Codex exec events](https://github.com/openai/codex/blob/main/codex-rs/exec/src/exec_events.rs)
  使用稳定 item identity 和 started/completed/failed 生命周期事件,消费者不从
  大段文本反推工具状态。
- [OpenAI Codex rollout recorder](https://github.com/openai/codex/blob/main/codex-rs/core/src/rollout.rs)
  与 [thread rollout truncation](https://github.com/openai/codex/blob/main/codex-rs/core/src/thread_rollout_truncation.rs)
  将完整持久化事实与送入模型或界面的有界投影分离。
- [Claude Code session storage](https://code.claude.com/docs/en/agent-sdk/session-storage.md)
  和 [sessions](https://code.claude.com/docs/en/sessions.md) 以本地持久化 transcript/
  sidecar 作为恢复依据,界面状态不是唯一事实源。
- [Cursor background agents](https://docs.cursor.com/background-agent) 将长时执行建模为
  可重新进入和审阅的独立 run,前台不需要常驻渲染全部执行载荷。

这些实现共同收敛到三个原则:稳定执行 ID、完整事实独立持久化、有界视图按需读取。
EKO 是本地桌面应用,因此采用文件/JSONL 和 Tauri IPC,不增加网络推流服务、后端缓存
或数据库索引层。

## 框架与应用边界

### `echo-agent`

框架只负责通用执行事实:

- `SubagentEvent::DispatchToolCompleted` 透传 `ToolResult.metadata` 和 `truncated`。
- Subagent executor 将流终态中的 artifact metadata 带到工具完成事件。
- 框架 artifact 能力继续独立存在,供 EKO 和其它复用方选择。

实现提交:framework commit `27bb5a4`。

框架不包含 EKO 的 `detail_ref`、JSONL 目录、Tauri command、Zustand store 或 GUI
折叠规则。

### `echo-agent-cli`

EKO 应用层拥有 `ToolExecutionRepository`、文件生命周期、Tauri IPC、消息投影和
React 渲染。这些都依赖本地桌面产品的审计与交互规则,不下沉到通用框架。

实现提交:application commit `d8b2211`; React/Zustand selector 稳定性热修
`b8c9077`。

## 持久化合同

默认根目录为 `~/.echo-agent/tool-executions/`,物理布局为:

```text
tool-executions/
└── <conversation-scope>/
    └── <run-scope>/
        ├── events.jsonl
        └── details/
            ├── <detail-ref>.json
            └── <detail-ref>.jsonl
```

- `events.jsonl` 只记录 started/finished/cancelled 生命周期和轻量 summary,用于顺序
  恢复与索引重建。
- `<detail-ref>.json` 原子写入完整参数、状态、失败事实、metadata、truncated 和输出
  字节数。
- `<detail-ref>.jsonl` 保存 stdout/stderr/log/result 分通道完整输出;原始块最大
  8 KiB,确保 JSON 转义后的单行不会突破 64 KiB 页面上限。
- `detail_ref` 是 UUID 逻辑 ID。前端不接触或拼接物理路径。
- 若框架已生成完整 artifact,详情读取仍通过同一个 `detail_ref` 分页,不会把
  `artifact_path` 暴露给前端。

应用启动时扫描 journal 重建内存索引。仅最后一行 JSON 损坏时丢弃该半写记录并
修复文件;遗留 running 工具统一收敛为 cancelled。每个活动工具有独立锁,不同工具
的输出写入不会被一个全局详情锁串行化。

## 事件与前端投影

主 Agent 和 Subagent 工具都只向 `execution://event kind=tool` 发送
`ToolExecutionSummary`。`chat://event` 和 Subagent 生命周期事件不再携带完整参数、
输出或结果。

前端规则:

- `toolExecutionStore` 按 `detail_ref` 归一化 summary,并按 chat message 或
  `subagent_run_id` 建立 owner ID 列表。
- conversation 只保存 `execution_steps` 中的工具 ID 和
  `execution_rounds[].tool_call_ids`,不复制工具载荷。
- 折叠的 `InlineToolCall` 只渲染单行摘要。展开时并行读取完整 manifest 和首个
  输出页。
- 单次 IPC 响应最多 64 KiB。已完成工具只在用户点击“加载更多”时继续读取,不会
  自动拉完全部日志。
- running 工具展开后每 500 ms 使用 `next_cursor` 增量读取;自动累计达到
  256 Ki characters 后暂停,保留手动继续加载入口。
- Subagent 的“执行过程”直接复用 `InlineToolCall`;thinking/token 原文不再进入
  Subagent GUI 热状态。“提示词 / 任务”和“结果”始终展示完整内容。

## 终态与清理

- 工具正常完成后写入 succeeded/failed、耗时、失败事实和 metadata。
- 主聊天取消/错误、Subagent completed/failed/cancelled 时,尚未终态的工具收敛为
  cancelled,不会永久留在 running。
- 删除 conversation 时先把详情 scope 原子重命名到 `.trash` tombstone,立即从索引
  移除,再由后台线程删除目录。框架 artifact 清理由 `spawn_blocking` 执行,不阻塞
  Tauri command 的 UI 响应。
- 存储或详情读取失败不会篡改原工具的业务成功/失败语义;GUI 显示独立的加载错误。

## EKO 单层 Subagent

EKO 创建 Subagent 时不注册 `agent_tool`,不注入 child registry,能力声明固定为
`can_delegate: false`。这只约束 EKO 产品,没有删除 `echo-agent` 的通用嵌套 Subagent
API。

## 验收标准

- 主 Agent 与 Subagent 工具产生相同 summary、详情文件、Tauri API 和 React 行组件。
- 首次进入历史会话只加载 summary,不读取任何完整详情文件。
- 100 MiB 级日志不会一次进入 IPC、Zustand 或 DOM;每页硬上限 64 KiB。
- running 日志可增量观察,终态后可继续逐页读取到完整内容。
- 中文和 emoji 参数摘要、输出分块和 artifact 分页不产生非法 UTF-8 或 panic。
- 崩溃后的半行 journal 和 running 状态可恢复;删除会话不会同步阻塞 UI。
- Subagent Prompt 与 Result 完整可见,Execution 只包含统一工具行。

## 验证结果

- `echo-agent` (`27bb5a4`):fmt、两组 Clippy、all-targets/all-features 全量测试、
  no-default-features 和 12 个隔离 feature 组合全部通过。
- `echo-agent-cli` (`d8b2211`):fmt、all-targets/all-features Clippy、workspace 全量测试、
  app-core no-default-features、GUI binary check 和 GUI feature 测试全部通过。
- `web-frontend`:热修 `b8c9077` 后,Prettier、19 个 Vitest 文件共 65 项测试、
  TypeScript/Vite production build 全部通过。
