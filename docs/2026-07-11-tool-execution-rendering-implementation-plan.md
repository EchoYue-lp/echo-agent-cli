# EKO Tool Execution Rendering Implementation Plan

日期: 2026-07-11

状态: shell 垂直切片与 M5 第一批 read/edit/search renderer 已完成;browser/MCP/subagent renderer 待后续阶段

## 0. 实施结果 (2026-07-11)

本轮完成 Phase 1-9 中支撑 shell 动态执行体验的主路径:

- `echo-agent` 已提供稳定 `call_id`、context-aware streaming、stdout/stderr/log 通道、并行 multiplex、Complete 元数据透传,并完成 shell/local sandbox 真流式与 Docker/K8s 明确退化。
- GUI/TUI 均以 `call_id` 维护 running/succeeded/failed/cancelled 状态,实时归并输出,错误和取消会收敛终态。
- GUI 展示 command、spinner、elapsed、exit code、输出 tail、展开日志和复制操作;桌面与 390px 窄屏无横向溢出。
- TUI 使用结构化工具消息原地刷新,显示 command、elapsed/exit code 和输出 tail,保留文件写工具原有 diff 视图。
- 会话文件只保存有界最终投影,包含 call_id、状态、stdout/stderr/log、metadata 和 truncated;兼容旧 thinking array 格式。
- TaskRuntime trace 同步接收 tool output/complete 事件,GUI/TUI/任务运行时共享相同生命周期语义。

本轮明确延后:

- Phase 10 的 read/search/browser/MCP/subagent 专属 renderer registry。
- 超长完整日志独立 artifact 落盘。
- 将 execution order/batch 内部引用进一步归一成纯 call_id 索引;当前结构已按 call_id 更新且行为正确,后续可在不改变事件合同的前提下瘦身。

M5 第一批完成 (2026-07-11):

- GUI 新增纯函数 renderer registry,覆盖 `read_file`、`edit_file/write_file/create_file`、`grep/glob/code_search/search_text`,未知工具回退 Generic。
- read 展示路径与行范围;write/edit/create 展示动作、路径、dry-run/行数/size 变化;search 展示查询、范围、过滤条件和可解析的命中数。
- 上述工具成功态默认压缩为单行,失败态仍显示错误 tail,展开后保留完整输出与 diff。
- TUI 使用同一字段语义生成紧凑标题与 detail,成功态不刷大段结果,失败态保留 tail;文件写工具原有完整 diff 视图不变。
- Browser、MCP、subagent 未注册到该 registry,继续使用既有专属事件/面板或 Generic fallback,避免重复卡片。

验证结果:

- `echo-agent`: `verify-all-crates.sh` 全绿,8 crate 共 1469 项测试、clippy 零警告、独立 feature 矩阵通过。
- `echo-agent-cli`: workspace 520 项测试通过,GUI feature 40 项测试通过,默认与 GUI clippy 零警告,channels 与 tui+telemetry+eval+improve 组合通过。
- `web-frontend`: 14 项 Vitest、TypeScript build、Vite production build 通过。

## 1. 目标

把 GUI 与 TUI 的工具展示从“调用开始 + 最终文本”的静态记录,升级为具有稳定执行身份、明确生命周期、实时输出和工具专属摘要的统一执行体验。

第一阶段以 `shell` 为垂直样板,同时建立可复用的工具事件底座。完成后继续覆盖文件、搜索、Web/MCP 与 subagent 工具。

最终体验要求:

- 工具从开始执行起立即可见,运行中不能显示成功状态。
- 同名并行工具互不串线,所有事件按稳定 `call_id` 归并。
- shell stdout/stderr 实时到达 GUI/TUI,可取消、可超时、可显示耗时与退出码。
- GUI/TUI 功能对等,差异仅在渲染方式和交互键位。
- 默认紧凑、失败优先展示关键错误、完整日志按需展开。
- 历史恢复展示最终执行状态,不依赖 SQLite,不持久化逐 chunk 事件。

## 2. 非目标

- 不把聊天内 shell 日志做成完整交互式终端。交互终端继续使用现有 PTY + xterm.js 路径。
- 不给每个工具制造独立事件协议。所有工具使用同一执行生命周期,应用层按工具类别投影。
- 不把 React、ratatui、EKO 文案、卡片状态或 UI 展开逻辑放进 `echo-agent`。
- 不为旧事件结构保留长期双协议或兼容层。本项目尚处开发阶段,调用点一次性迁移。
- 不引入 SQLite、数据库 schema 或迁移脚本。
- 不因 shell 展示改造增加新的权限门控。自动工具行为沿用现有审批/风险路径,用户交互式终端不受影响。

## 3. 业界依据

### 3.1 Codex

Codex `exec --json` 将命令执行建模为独立 item,事件使用 `item.started`、`item.completed`、`item.failed`,item 自带稳定 ID 与 `in_progress` 状态。命令执行、文件变化、MCP 调用、Web 搜索和 plan update 都是可追踪 item,而不是靠显示名称配对。

参考:

- https://learn.chatgpt.com/docs/non-interactive-mode#make-output-machine-readable
- 示例核心结构:`item.started { id, type: "command_execution", command, status: "in_progress" }`

### 3.2 Claude / Anthropic

Anthropic 流式工具模型使用 start/delta/stop 累积契约。客户端在 start 时建立槽位,持续消费 delta,在 stop 后形成最终值。细粒度流明确区分“实时响应片段”和“最终完整结果”。

参考:

- https://platform.claude.com/docs/en/agents-and-tools/tool-use/fine-grained-tool-streaming
- https://platform.claude.com/docs/en/build-with-claude/streaming

### 3.3 跨系统共识与 EKO 取舍

| 共识 | EKO 取舍 |
| --- | --- |
| 每次执行有稳定 ID | 复用模型 tool call ID,缺失时在 pipeline 生成 UUID |
| 生命周期显式 | queued/running/succeeded/failed/cancelled,不再用可空 success 推断 |
| 增量与最终结果分离 | `ToolStreamEvent` 传增量,`ToolResult` 作为最终权威结果 |
| 并行调用独立更新 | 所有工具事件携带 `call_id`,批次只表达并发分组 |
| 展示是事件投影 | 框架只发通用事件,EKO GUI/TUI 各自渲染 |
| 默认紧凑 | 完成态一行摘要,运行/失败态显示有限 tail,完整日志展开查看 |

## 4. 现状审计

### 4.1 已有能力,必须复用

`echo-agent` 已有:

- `echo-core/src/tools/mod.rs::ToolStreamEvent`
  - `Progress { message, percent }`
  - `PartialOutput { chunk }`
  - `Complete(ToolResult)`
- `Tool::execute_stream()` 与 `Tool::supports_streaming()`。
- `AgentEvent::ToolStream { name, event }`。
- pipeline 内已有 `ToolExecutionContext.call_id`,缺失时生成 UUID。

因此不新增第二套 `ExecutionProgressEvent` 或 EKO 专属框架事件。

### 4.2 当前断点

1. `Tool::execute_stream()` 没有 `ToolContext`,无法安全获得 working_dir、run/turn/execution identity、cancel 与 trace sink。
2. `ToolManager::execute_tool_stream_collect()` 把 stream 收集成 `Vec` 后才返回,实时性被消除。
3. `ExecuteStage` 始终调用 `execute_tool_with_context()`,不走 streaming 路径。
4. ReAct `run_tools()` 只发 `ToolCall` 与最终 `ToolResult/ToolError`,且事件没有 `call_id`。
5. GUI `chatStore.completeToolCall()` 按工具名 FIFO 匹配,两个同名并行调用可能错误归并。
6. GUI `success === undefined` 会显示绿色成功状态,运行态不真实。
7. TUI `TuiChatSink` 丢弃 `AgentEvent::ToolStream`,工具调用/结果被转成 `System` 字符串。
8. `ShellTool` 使用 `Command::output()`,stdout/stderr 只能在进程结束后一次性获得。
9. 沙箱执行器只返回完整 `SandboxResult`,沙箱路径同样无法实时输出。

## 5. 架构边界

### 5.1 放在框架 `echo-agent`

以下能力对所有长时间工具、代码执行、MCP wrapper 和未来复用方都成立:

- 稳定 tool call identity。
- 通用执行生命周期和流式输出事件。
- context-aware streaming Tool API。
- ToolManager 并发、超时、重试、取消与背压。
- ReAct 并行工具流 multiplex。
- stdout/stderr/log 等通用输出通道。
- shell 与 sandbox 的流式进程执行实现。

### 5.2 放在应用 `echo-agent-cli`

以下依赖 EKO 产品形态和 GUI/TUI 交互:

- `ToolExecutionView`/前端 store/TUI message projection。
- shell 命令摘要、耗时文案、自动展开策略。
- 文件 diff、搜索结果、MCP/Web 等专属 renderer。
- GUI 折叠、复制、自动滚动和日志高度。
- TUI spinner、键盘展开、行宽裁剪和颜色。
- 会话文件中的最终 UI 投影。

## 6. 目标事件合同

### 6.1 框架事件

一次性修改 `AgentEvent`:

```rust
ToolCall {
    call_id: String,
    name: String,
    args: Value,
}

ToolStream {
    call_id: String,
    name: String,
    event: ToolStreamEvent,
}

ToolResult {
    call_id: String,
    name: String,
    output: String,
}

ToolError {
    call_id: String,
    name: String,
    error: String,
}
```

`ToolBatchStart`/`ToolBatchEnd` 保留,仅表示同一 LLM round 内的并发分组,不承担工具身份。

### 6.2 流事件

将 `PartialOutput` 扩展为带输出通道的通用事件:

```rust
pub enum ToolOutputChannel {
    Stdout,
    Stderr,
    Log,
}

pub enum ToolStreamEvent {
    Progress {
        message: String,
        percent: Option<u8>,
    },
    Output {
        channel: ToolOutputChannel,
        chunk: String,
    },
    Complete(ToolResult),
}
```

不保留 `PartialOutput` 旧变体。所有内部调用点一次性迁移。

### 6.3 最终元数据

通用执行元数据放入现有 `ToolResult.metadata`:

- `duration_ms`
- `exit_code`
- `working_dir`
- `output_truncated`
- `stdout_bytes`
- `stderr_bytes`

框架不解析 `cargo test` 的 passed/failed 数量。此类摘要由 EKO shell renderer 从最终输出尽力提取,解析失败时退回 exit code + duration。

## 7. 详细实施任务

### Phase 0: 基线与合同测试

**目标:** 在改生产代码前锁定当前缺口和目标行为。

**文件:**

- `echo-agent/echo-core/src/agent/mod.rs`
- `echo-agent/echo-core/src/tools/mod.rs`
- `echo-agent/src/agent/react/run/phases/tools.rs`
- `echo-agent/echo-execution/src/tools.rs`
- `echo-agent-cli/web-frontend/src/stores/chatStore.ts`
- `echo-agent-cli/src/tui/events.rs`

- [ ] 为两个同名并行工具编写失败测试,证明当前按 name 无法稳定关联。
- [ ] 为 `ToolStreamEvent` 编写 serde round-trip 测试。
- [ ] 为 AgentEvent call identity 编写编译期/行为测试。
- [ ] 为 GUI reducer 写 running/succeeded/failed 状态测试。
- [ ] 为 TUI reducer 写 ToolStream 当前被丢弃的回归测试。
- [ ] 记录 shell 现有静态 GUI/TUI 基线截图,用于最终对比,不作为代码依赖。

**退出条件:** 所有新增测试因目标接口或目标行为尚不存在而失败,失败原因明确。

### Phase 1: 框架稳定 identity

**目标:** 所有工具事件使用模型原始 tool call ID,并行同名工具可独立追踪。

**文件:**

- `echo-agent/echo-core/src/agent/mod.rs`
- `echo-agent/src/agent/react/run/phases/tools.rs`
- `echo-agent/src/agent/react/run/pipeline.rs`
- `echo-agent/src/agent/subagent/executor.rs`
- `echo-agent/src/a2a/server.rs`
- 所有 `AgentEvent::{ToolCall,ToolResult,ToolError,ToolStream}` 消费点和测试

- [ ] 给四类 AgentEvent 增加 `call_id`。
- [ ] `run_tools()` 发 ToolCall 时使用 `steps` 中已有 ID。
- [ ] 执行结果与错误沿用同一个 ID,禁止按 zip 外的名称回查。
- [ ] pipeline 缺失 ID 时安全生成 UUID,不使用空字符串作为执行键。
- [ ] 更新 subagent、A2A、trace、webhook、eval 等消费者。
- [ ] 删除应用层按 name FIFO 关联的假设和注释。
- [ ] 增加同名并行工具乱序完成测试。

**退出条件:** 同一 batch 内两个 `shell` 可乱序结束,事件仍归入各自 call。

### Phase 2: context-aware 真流式 Tool API

**目标:** 激活已有 Tool streaming 能力,且不丢工作目录与运行上下文。

**文件:**

- `echo-agent/echo-core/src/tools/mod.rs`
- `echo-agent/echo-execution/src/tools.rs`
- `echo-agent/src/agent/react/run/pipeline.rs`
- `echo-agent/src/agent/react/run/phases/tools.rs`

- [ ] 新增 `Tool::execute_stream_with_context(params, ctx)`。
- [ ] 默认实现调用 `execute_with_context()` 并产生单个 `Complete`。
- [ ] `execute_stream()` 作为空 context 兼容入口,内部委托新方法。
- [ ] ToolManager 新增返回实时 stream 的 context-aware API。
- [ ] 删除或改写 `execute_tool_stream_collect()`,生产路径禁止先 collect 再返回。
- [ ] 将 ToolManager 的 semaphore permit 生命周期绑定到 stream 完成或 drop。
- [ ] 超时覆盖整个流消费周期,而不只是 stream 创建 future。
- [ ] 取消时 drop stream 并释放 permit；工具负责 kill-on-drop 子进程。
- [ ] 重试只允许发生在尚未向外发出可见 Output 前；已发 chunk 后失败不得从头重放造成重复日志。
- [ ] 非 streaming 工具继续只产生最终 Complete,无调用方特判。

**退出条件:** mock streaming tool 的第一个 chunk 能在工具完成前抵达 ReAct 消费者。

### Phase 3: ReAct 并行流 multiplex

**目标:** 多工具并行时实时转发各自事件,同时保持最终上下文写入顺序正确。

**文件:**

- `echo-agent/src/agent/react/run/phases/tools.rs`
- `echo-agent/src/agent/react/run/execution.rs`
- `echo-agent/src/agent/react/run/stream_macros.rs`

- [ ] 每个并发工具 future 把 `(call_id, name, ToolStreamEvent)` 发送到内部 bounded mpsc。
- [ ] 主循环同时消费增量事件、工具完成、取消和 batch timeout。
- [ ] `Progress/Output` 立即映射为 `AgentEvent::ToolStream`。
- [ ] `Complete(success)` 只产生一个最终 `ToolResult`。
- [ ] `Complete(failure)` 或基础设施错误产生 `ToolError`。
- [ ] 最终 Message::tool_result 仍使用原始 tool call ID,供下一轮模型上下文关联。
- [ ] `ToolBatchEnd` 只在所有工具终态收敛后发出。
- [ ] channel 满时采用有界背压,不静默丢弃 stderr；取消仍优先响应。

**退出条件:** 两个 mock 工具的 chunk 可交错出现,各自只有一个终态事件,batch 正确闭合。

### Phase 4: ShellTool 与 sandbox 流式执行

**目标:** shell stdout/stderr 实时输出,最终结果携带退出元数据。

**文件:**

- `echo-agent/echo-tools/src/shell.rs`
- `echo-agent/echo-execution/src/sandbox/mod.rs`
- `echo-agent/echo-execution/src/sandbox/local.rs`
- `echo-agent/echo-execution/src/sandbox/docker.rs`
- `echo-agent/echo-execution/src/sandbox/k8s.rs`

- [ ] `ShellTool::supports_streaming()` 返回 true。
- [ ] 实现 `execute_stream_with_context()`。
- [ ] 非沙箱路径使用 `spawn()` + piped stdout/stderr,禁止 `Command::output()`。
- [ ] 两条 pipe 并行读取,按实际到达顺序产生 Output。
- [ ] UTF-8 解码使用增量/有损安全路径,不得按任意字节位置切 Rust `str`。
- [ ] 记录 start time、cwd、exit code 与字节计数。
- [ ] timeout/cancel/drop 后终止并回收子进程,避免 orphan。
- [ ] 设定单工具内存输出上限与 UI stream 上限；最终 ToolResult 标记 truncated。
- [ ] sandbox trait 增加通用流式执行入口,默认可退化为单次 Complete。
- [ ] Local sandbox 优先实现真流式；Docker/K8s 若暂不能流式,必须明确退化并有测试,不能虚报实时。
- [ ] stdout 为空但 stderr 有内容的失败命令仍保留 stderr。

**退出条件:** `printf + sleep + printf` 的首段在进程结束前可见；非零退出、超时、取消均正确收敛。

### Phase 5: 应用层统一 transport contract

**目标:** Tauri、WebSocket、TUI 消费同一执行事件含义。

**文件:**

- `echo-agent-cli/echo-agent-app-core/src/types/response.rs`
- `echo-agent-cli/src/tauri/commands/chat.rs`
- `echo-agent-cli/echo-agent-app-core/src/chat_driver.rs`
- `echo-agent-cli/web-frontend/src/types/api.ts`
- `echo-agent-cli/web-frontend/src/generated/*`
- `echo-agent-cli/src/tui/events.rs`

目标 ChatEvent:

```text
tool_started  { call_id, name, args, started_at }
tool_progress { call_id, message, percent }
tool_output   { call_id, channel, chunk }
tool_finished { call_id, name, success, result, metadata, finished_at }
```

- [ ] Tauri 与 WebSocket 字段、命名和终态语义一致。
- [ ] 不再把 `tool_result success` 当完整生命周期。
- [ ] TUI sink 透传 ToolStream,不得进入 unrendered 分支。
- [ ] 所有 transport 序列化测试覆盖 call_id 与 Output channel。
- [ ] 错误和取消必须产生终态,避免 GUI/TUI 永久 running。

**退出条件:** 同一录制事件序列可分别驱动 GUI 与 TUI reducer 得到等价状态。

### Phase 6: GUI execution store

**目标:** 用结构化执行状态替换 `pendingToolCalls + completed toolCalls` 双数组。

**文件:**

- `echo-agent-cli/web-frontend/src/stores/chatStore.ts`
- Create: `echo-agent-cli/web-frontend/src/lib/toolExecution.ts`
- Create: `echo-agent-cli/web-frontend/src/lib/toolExecution.test.ts`
- `echo-agent-cli/web-frontend/src/types/api.ts`

建议模型:

```ts
type ToolExecutionStatus =
  | 'queued'
  | 'running'
  | 'succeeded'
  | 'failed'
  | 'cancelled';

interface ToolExecutionView {
  id: string;
  name: string;
  kind: ToolKind;
  status: ToolExecutionStatus;
  args: unknown;
  stdout: string;
  stderr: string;
  progress?: { message: string; percent?: number };
  startedAt: number;
  finishedAt?: number;
  metadata: Record<string, string>;
  truncated: boolean;
}
```

- [ ] reducer 只按 call_id 更新。
- [ ] running 与 success 是互斥状态。
- [ ] chunk append 使用有界 buffer,保留 tail 并记录 truncated。
- [ ] 消息中的 execution order 记录 call_id,不复制整份对象。
- [ ] batch/round 只保存 call_id 列表。
- [ ] 恢复历史时直接构造终态 ToolExecutionView。
- [ ] 删除 `pendingToolCalls.findIndex(tc.name === name)`。

**退出条件:** GUI store 单测覆盖开始、交错 chunk、乱序完成、失败、取消、重复终态和未知 call_id。

### Phase 7: GUI 专属渲染

**目标:** shell 达到成熟 coding agent 的紧凑、动态、可检查体验。

**文件:**

- Replace/refactor: `web-frontend/src/components/chat/InlineToolCall.tsx`
- Create: `web-frontend/src/components/chat/tools/ToolExecution.tsx`
- Create: `web-frontend/src/components/chat/tools/ShellExecution.tsx`
- Create: `web-frontend/src/components/chat/tools/GenericToolExecution.tsx`
- Create: `web-frontend/src/components/chat/tools/toolRenderers.ts`
- Modify: `web-frontend/src/components/chat/MessageBubble.tsx`
- Tests: colocated Vitest files

视觉合同:

```text
运行中  ⠋ cargo test                         8.4s
        Compiling echo_core...

成功    ✓ cargo test · 12.4s · exit 0

失败    ✗ cargo test · 8.1s · exit 101
        error[E0308]: mismatched types
```

- [ ] 工具 renderer registry 按 kind 分派,未知工具走 Generic fallback。
- [ ] shell 标题直接展示 command,不显示 `shell {"command":...}`。
- [ ] spinner、elapsed time 在 running 时更新。
- [ ] 默认显示最近 3-6 行输出,成功且无重要输出时可完全折叠。
- [ ] 失败默认展示 stderr/错误 tail,但不强制展开全部日志。
- [ ] stdout/stderr 使用不同但克制的文本颜色。
- [ ] 支持复制 command、stdout、stderr 和全部日志。
- [ ] 自动滚动仅在用户位于底部时继续；用户向上滚动后不抢焦点。
- [ ] 不嵌套卡片,不使用大面积边框和状态 badge。
- [ ] 小屏宽度下 command 可换行或截断,状态/耗时不得覆盖正文。
- [ ] 复用现有字体/主题变量,不引入一套孤立配色。

**退出条件:** 运行 10 秒命令时 GUI 持续变化；1440x900 与 390x844 无重叠、横向溢出。

### Phase 8: TUI 结构化模型与渲染

**目标:** TUI 与 GUI 使用同一生命周期,以 ratatui 原地刷新执行项。

**文件:**

- `echo-agent-cli/src/tui/mod.rs`
- `echo-agent-cli/src/tui/events.rs`
- `echo-agent-cli/src/tui/widgets/chat.rs`
- `echo-agent-cli/src/tui/ui.rs`
- TUI reducer/widget tests

- [ ] 新增结构化 `ToolExecutionMessage`,禁止再把工具开始/结果写成 `MessageRole::System` 字符串。
- [ ] message/group 以 call_id 引用工具执行状态。
- [ ] running spinner 随既有 render tick 更新,不创建额外无界 timer task。
- [ ] 实时显示 output tail 与 elapsed time。
- [ ] 完成态压缩成一行,失败态附加关键 stderr。
- [ ] Ctrl+O 沿用 transcript 展开/折叠；必要时补 Enter 针对当前执行项展开。
- [ ] 并行工具各占一行并独立刷新。
- [ ] 终端宽度不足时 UTF-8 安全裁剪,不得字节切片。
- [ ] 去掉“所有 shell 都是 irreversible DANGER”的渲染；只有真实审批/风险事件显示风险提示。
- [ ] `--no-alt-screen` 下完成态仍保留可读 scrollback,运行中临时帧不得刷屏。

**退出条件:** TUI 中长命令可实时更新且完成后收敛成紧凑记录；同名并行 shell 不覆盖。

### Phase 9: 历史持久化与恢复

**目标:** 文件会话保存足够的最终信息,不把逐 chunk 日志当事件日志永久保存。

**文件:**

- `echo-agent-cli/echo-agent-app-core/src/conversation_file.rs`
- `echo-agent-cli/echo-agent-app-core/src/persistence.rs`
- `echo-agent-cli/echo-agent-app-core/src/conversation_restore.rs`
- `echo-agent-cli/web-frontend/src/stores/conversationStore.ts`

- [ ] SavedToolCall 增加稳定 call_id、status、metadata 与 capped stdout/stderr。
- [ ] 只保存最终投影,不保存每个 ToolStream chunk。
- [ ] 输出按字符/字节双上限安全截断并记录 truncated。
- [ ] 超长完整日志若需要保留,写独立 file artifact 并保存路径；默认不强制落盘。
- [ ] 恢复后工具为终态,不得重新出现 spinner。
- [ ] 删除过时的 name-only 恢复逻辑。

**退出条件:** 重启后 shell command、成功/失败、exit code、duration 和有限输出可恢复。

### Phase 10: 扩展工具专属 renderer

**目标:** 把 shell 建立的视觉语言扩展到高频工具,避免所有工具都显示 JSON。

建议顺序:

1. ✅ `read_file`:路径、行范围、读取行数。
2. ✅ `edit_file/write_file/create_file`:路径、变更规模、diff。
3. ✅ `grep/glob/code_search/search_text`:查询词、范围、过滤条件、命中摘要。
4. browser/web:域名、页面标题、动作摘要。
5. MCP:server/tool 名、结果类型。
6. subagent/task:复用现有 execution panel,不重复造卡片。

- [x] 每类 renderer 只解析已知字段,解析失败回退 Generic。
- [x] 文件写工具完成后优先显示 diff/统计,不展示整份 JSON args。
- [x] 读/搜索工具成功态默认单行,避免占据聊天主视觉。
- [ ] browser screenshot/chart/image 等富媒体继续走已有专属组件。

## 8. 输出、背压与内存策略

建议首版边界:

- 单个 transport chunk:最大 16 KiB；读取更大块时拆分。
- GUI/TUI 活跃 tail:每 channel 64 KiB 或最近 1000 行,取较小边界。
- 最终会话投影:stdout/stderr 各最多 128 KiB。
- 框架 ToolResult 总截断继续服从现有 snapshot/tool output 限制。
- channel 必须 bounded。满载时执行 future 等待消费者形成背压,不无限缓存。
- 若 UI 断开,核心工具执行是否继续由现有 chat cancellation 语义决定,不由 renderer 自行取消。

具体数字在 Phase 0 基准测试后可调整,但必须集中成常量并有测试,不得散落硬编码。

## 9. 失败与取消语义

| 场景 | 终态 | 展示 |
| --- | --- | --- |
| exit code 0 | succeeded | command + duration + exit 0,输出按需 |
| exit code 非 0 | failed | command + exit code + stderr tail |
| Tool infrastructure error | failed | 简短错误,可展开技术详情 |
| timeout | failed | 显示 timeout 时长,进程必须已终止 |
| 用户取消 | cancelled | 显示 cancelled + elapsed,不伪装失败 |
| UI transport 丢失 | 不自行定义 | 沿用 turn/run cancellation 权威 |
| 输出截断 | 原终态不变 | 显示 truncated 标记与日志文件入口(若有) |

## 10. 测试矩阵

### 10.1 框架

- identity:同名、不同名、并行、乱序完成。
- streaming:start/output/progress/complete 顺序。
- lifecycle:每个 call 恰好一个终态。
- process:stdout、stderr、交错输出、无换行输出。
- cancellation:用户取消、batch timeout、tool timeout。
- resources:child 回收、permit 释放、receiver drop。
- Unicode:中文/emoji 跨读取块,不 panic、不产生非法切片。
- volume:大输出、慢消费者、截断与 bounded channel。
- sandbox:local 真流式,其它 executor 明确退化。
- features:default/shell/subagent/human-loop/sqlite 等矩阵不破坏。

### 10.2 GUI

- reducer 状态机与 call_id 关联。
- running 不显示成功。
- 同名并行更新。
- stdout/stderr 追加和 truncation。
- 完成/失败/取消渲染。
- 自动滚动暂停/恢复。
- 历史恢复。
- desktop/mobile 边界截图。

### 10.3 TUI

- sink 不丢 ToolStream。
- reducer 按 call_id 更新。
- spinner/elapsed deterministic 测试。
- 并行工具布局。
- 展开/折叠。
- 窄宽度和 Unicode。
- `--no-alt-screen` 完成态不刷重复行。

## 11. 验证与提交闸门

### 11.1 `echo-agent`

```bash
cargo fmt --all
cargo fmt --all -- --check
./scripts/verify-all-crates.sh
cargo clean
```

脚本必须覆盖 8 crate 测试、clippy 与独立 feature 矩阵。框架先提交、先合并。

### 11.2 `echo-agent-cli`

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo check --no-default-features --features gui --bin echo-agent-tauri
cargo test --no-default-features --features gui
cargo clippy --all-targets -- -D warnings

cd web-frontend
npm test
npx prettier --check .
npx tsc -b
npm run build

cd ..
cargo clean
```

另跑项目要求的 `channels`、`tui+telemetry`、`tui+eval`、`tui+improve`、`gui+devtools` 等关键组合。

### 11.3 提交顺序

1. `echo-agent`:call_id + streaming API。
2. `echo-agent`:ReAct multiplex + ShellTool/sandbox。
3. `echo-agent-cli`:transport + reducer。
4. `echo-agent-cli`:GUI renderer。
5. `echo-agent-cli`:TUI renderer。
6. `echo-agent-cli`:persistence +其它工具 renderer。

所有 commit 显式使用 `git -c commit.gpgsign=false commit`。跨仓库依赖必须先合并框架。

## 12. 分阶段交付与验收

| Milestone | 范围 | 用户可见结果 |
| --- | --- | --- |
| M1 | Phase 0-3 | 工具事件有稳定 ID,框架可以实时流式输出 |
| M2 | Phase 4-7 | GUI shell 实时、紧凑、可展开 |
| M3 | Phase 8 | TUI shell 与 GUI 生命周期对等 |
| M4 | Phase 9 | 历史恢复保留最终执行摘要 |
| M5 | Phase 10 | 文件/搜索/Web/MCP 使用专属展示 |

每个 milestone 完成、验证、提交后立即更新 `/docs/MASTER-PLAN.md`。M1 与 M2 强耦合,建议同一新鲜窗口连续完成；M3 可独立窗口；M4/M5 属于弱耦合后续阶段。

## 13. 风险与控制

### 风险 1:并发流改变 ReAct 主路径

`run_tools()` 是聊天命脉。控制方式:先 mock 工具和 identity 测试,再接 ShellTool；保留 ToolResult 写上下文的单一终点。

### 风险 2:stream API 丢 ToolContext

不能直接启用现有 `execute_stream()`。必须先完成 `execute_stream_with_context`,否则 worktree cwd、run identity、cancel 和 trace 会退化。

### 风险 3:输出重复

增量 chunk 与 Complete 可能都含完整输出。UI reducer 以增量 buffer 做运行展示,完成时以 ToolResult 为最终权威替换/校验,不得盲目再次 append。

### 风险 4:慢 UI 阻塞命令

bounded channel 提供背压,但 UI 渲染需要节流。GUI/TUI 可每 50-100ms 合并 chunk 更新,协议仍保留完整顺序。

### 风险 5:沙箱能力不一致

不能因非沙箱路径已流式就宣称全路径完成。每个 SandboxExecutor 必须标注真流式或明确 buffered fallback,验收报告逐项列出。

### 风险 6:现有 Browser Runtime 未提交改动

`echo-agent-cli` 当前存在 Browser Runtime Phase 5 用户改动。实施时必须在独立 worktree/分支进行,不得覆盖或混入现有工作区改动；合并前先 merge main 并按 AGENTS.md 检查 Cargo 相对路径。

## 14. 首个执行窗口建议

首个实现窗口只做 M1:

1. 重读本计划与 MASTER-PLAN。
2. 新建 `echo-agent` feature worktree。
3. 完成 Phase 0 identity/stream contract 红测。
4. 完成 Phase 1 call_id 全链路迁移。
5. 完成 Phase 2 context-aware streaming API。
6. 完成 Phase 3 mock 并行流 multiplex。
7. 跑 `verify-all-crates.sh`、cargo clean、提交框架。
8. 更新 MASTER-PLAN,再决定同窗口继续 ShellTool 还是换新窗口。

不要在首个提交同时改 GUI/TUI。先让框架事件合同稳定,避免两端围绕临时协议重复返工。
