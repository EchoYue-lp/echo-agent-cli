# GUI 主窗口与右侧栏重构设计

- **日期**: 2026-06-22
- **范围**: `echo-agent-cli/web-frontend`(前端 GUI 重构)+ `echo-agent` / `echo-agent-cli` 框架层(agent_tool 解耦,见 §8)
- **目标**: 主窗口回归主流 agent IDE(Cursor / Codex / Claude Code 桌面版 / ZCode)的「一条流」范式,删除割裂的 6 张卡片;右侧栏精简为三块;修复 markdown 不渲染;框架解耦让 EKO 的 LLM 不再调 `agent_tool`(避免与产品并行路径 TaskRuntime `run_dag` 混淆)。

---

## 1. 背景与问题诊断

### 1.1 主窗口割裂感来源

`ChatPanel.tsx` 在 `messages.map(...)` 之后无脑追加两个全局组件:

```tsx
<ConversationTimeline />      // 路由决策 / worker 卡片(轻量版)
<TaskRuntimeMainPanel />      // 6+ 张卡片全在这
```

`TaskRuntimeMainPanel` 一次渲染最多 9 张 `RuntimeStoryCard`:任务执行、路由决策、计划确认、并行执行、最终任务结果、产出、文件变更、审查结果、测试/验证。这些卡片用 `story-dot` 时间线点串联,但和「用户提问 / agent 回答」的对话流平铺,读起来像两套系统硬塞在一起。

路由决策、并发委派数量、permission mode、route signals、approval policy 等是**框架内部可观测性数据**,不该在主窗口和对话平起平坐。

### 1.2 信息双份

右侧栏 `RightRail` 已有 `TaskRuntimePanel`(精简版)+ 进度 + 文件改动。主窗口又来一份完整版,信息重复。

### 1.3 Markdown 几乎不渲染的根因

`utils/markdown.ts` 产出的 HTML 正确,但所有 CSS 选择器都以 `.md-content` 为前缀(`.md-content h1`、`.md-content p`、`.md-content table` …),见 `index.css:415-546`。

`MarkdownContent` 组件**不自带 `md-content` 类**,各调用点传的 `className` 也没有 `md-content`:

| 调用点 | 传的 className | 能渲染? |
|---|---|---|
| `MessageBubble`(主回答) | 外层 div 凑巧带 `md-content` | ✅ 唯一能渲染 |
| `ConversationTimeline`×4 | `text-[11px]` / 无 | ❌ 裸 HTML |
| `TaskRuntimePanel`×4 | `text-[11px]` / `text-[12px]` | ❌ 裸 HTML |
| `ResultFullView` | 无 | ❌ 裸 HTML |
| `MarkdownCell` | 无 | ❌ 裸 HTML |

---

## 2. 设计原则

1. **主窗口 = 纯对话流**(主流 agent IDE 范式):用户提问 + agent 回答,agent 回答是「一条连续的 markdown 流」,思考段 / 工具调用 / 最终正文按时间顺序内联穿插,不分块、无中间细线。
2. **按需出现,不常驻**:计划确认 / 工具审批 / 输入 / 选择 / 失败提示等卡片只在触发时出现,结束即卸载。
3. **右侧栏只留三块**:任务执行进度 / 输出产物 / Token & Cache。
4. **前端启发式关联 sub-agent**,不动后端。
5. **删除优先于保留**(AGENTS.md):过时组件、双系统、死代码直接删,不留兼容。

---

## 3. 主窗口(ChatPanel)设计

### 3.1 整体结构

```
┌─ 顶栏:workspace 名 + 根路径 + run 状态点(保留不动)──────────────────┐
├─ 消息滚动区(max-w-[920px] 居中)────────────────────────────────────────┤
│                                                                         │
│  for each message in messages:                                          │
│    [时间分隔线(若与上条间隔>5min,保留)]                                  │
│    <MessageBubble message={msg} />   ← 重写,见 §3.2                     │
│                                                                         │
│  (删除) <ConversationTimeline />                                        │
│  (删除) <TaskRuntimeMainPanel />                                        │
│                                                                         │
│  [流式占位 spinner + 状态文案 + 三点 bounce](保留)                       │
│  [isCancelled 横线提示](保留)                                            │
│                                                                         │
│  [按需卡片,触发才出现,结束即卸载,见 §3.4]                                │
│    · 计划确认卡(awaitingApproval)                                       │
│    · 工具审批卡(approvalRequest,保留)                                   │
│    · 输入卡(inputRequest,保留)                                         │
│    · 选择卡(selectionRequest,保留)                                      │
│    · 执行失败 toast(todos 有 failed 项,新增)                            │
│                                                                         │
├─ 连接状态条(disconnected 时,保留)──────────────────────────────────────┤
├─ 停止生成按钮(isStreaming 时,保留)─────────────────────────────────────┤
├─ <ChatInput />(保留)────────────────────────────────────────────────────┤
└─────────────────────────────────────────────────────────────────────────┘
```

### 3.2 单条 assistant 消息的新结构(MessageBubble 重写)

「一条流」范式:思考段、工具调用行、最终正文,全部按 `executionRounds` 的时间顺序内联在同一个内容区里,无块分隔、无中间细线。

```
┌──────────────────────────────────────────────────────────┐
│ [头像]                                                    │
│ ┌──────────────────────────────────────────────────────┐ │
│ │ <思考 1,可折叠>                                      │ │
│ │   <markdown,LLM 推理>                                 │ │
│ │ 🔧 read_file  src/main.rs         [✓]  ← 一行摘要     │ │
│ │   └ 点开:参数 / 结果                                  │ │
│ │ <思考 2,可折叠>                                      │ │
│ │ ┌─ 并行执行 ─────────────────── 2 个 worker ─────┐    │ │
│ │ │ ┌─ sub-agent: reviewer ────────────── [运行中] ┐│    │ │
│ │ │ │ ▸ 提示词   ▸ 执行过程(嵌套循环)   ▸ 结果    ││    │ │
│ │ │ └──────────────────────────────────────────────┘│    │ │
│ │ │ ┌─ sub-agent: explorer ───────────── [已完成] ─┐│    │ │
│ │ │ │ ▸ 提示词   ▸ 执行过程(嵌套循环)   ▸ 结果    ││    │ │
│ │ │ └──────────────────────────────────────────────┘│    │ │
│ │ └────────────────────────────────────────────────┘    │ │
│ │ <最终回答正文,markdown 渲染,流式追加>                │ │
│ │ ...continues...                                       │ │
│ └──────────────────────────────────────────────────────┘ │
│ [hover: 复制 / 重新生成]                                  │
└──────────────────────────────────────────────────────────┘
```

**渲染顺序**:
1. 按 `executionRounds` 顺序,每轮先渲染「思考」段,再渲染该轮 `tools[]` 的「行动」段(工具调用行)。
2. 所有 rounds 渲染完后,若本次 run 有并行 worker(§3.3),渲染「并行执行」段。
3. 最后接 `message.content` 的最终正文(markdown)。
4. 符合实际:主 agent ReAct(若有)→ 框架并行派发的 worker → 最终汇总正文。

**关键规则**:
- **删除** `ExecutionProcessBlock` 的"思考与执行 (N 思考, M 工具)"整块外壳。数据来源不变,渲染方式重写为内联流。
- **思考段**:可折叠,默认 streaming 时展开、结束后折叠。一段 markdown,左边紫色细竖线 + "思考 N" 小标签 + Brain 图标。多个思考段独立编号。
- **工具调用行**:轻量,一行摘要(图标 + 工具名 + 关键参数预览 + 状态 ✓/✗),点击展开参数/结果。**不再是独立卡片**。复用 `ToolCallCard` 但改成 inline 折叠行样式(左侧细竖线,非独立 bordered card)。
- **并行 sub-agent 行动**:当本次 run 派发了并行 worker(见 §3.3),在主 agent 回答流里渲染一个「并行执行」段,内含 N 个 worker 的嵌套块。**不挂在某个工具调用下**——并行 worker 是 run 级别框架派发的,和主 agent 的 ReAct 步骤平行。关联规则见 §3.3。
- **最终正文**:`message.content` 的 markdown,直接接在最后一个思考/工具之后,自然成为流的一部分。流式光标在末尾。
- **用户消息**:不变,保留现有 `whitespace-pre-wrap` 气泡。
- **hover 操作按钮**(复制/编辑/重新生成):保留,挂在最终正文上。

### 3.3 并行 sub-agent 的渲染(run 级别关联)

#### 3.3.1 事实基础(已核查后端代码)

并行 worker 的真正触发机制是 **TaskRuntime 的 `run_dag`**,不是 `agent_tool` 工具调用。

- `agent_tool`(`echo-agent/src/tools/builtin/agent_dispatch.rs:117`)是库层的 LLM 工具,靠提示词驱动 LLM 在一个 turn 里返回多个 tool call 实现并发。它**不创建 TaskRun、不发 PlanReady 事件、和前端 run 事件流无关联**。不是产品路径。
- 产品路径(框架驱动确定性派发):
  1. 路由决策(`router.rs:704`):信号命中 → `ParallelReadonlyDelegation`,这是唯一 `should_auto_execute` 的路由。
  2. 确定性 plan 生成(`planner.rs:113`):`generate_parallel_readonly_plan` 把 N 个 `WorkerSpec` 转成 N 个无依赖 `PlanTask`(只读)+ 1 个 `summary_writer`(依赖前面全部)。**不调 LLM**。
  3. DAG 并发 spawn(`executor.rs:528-559`):`run_dag` 的 wave 循环对每个 ready task `tokio::spawn(worker.dispatch(...))`,push 进 `handles: Vec<JoinHandle>` 并发执行。并发上限 `max_concurrent_workers=4`。
  4. Fork 模式隔离(`subagent/executor.rs:662`):每个 worker 在隔离 agent 实例上跑,持有自己的 semaphore permit。

#### 3.3.2 关联策略(run 级别,确定版)

**主 agent 的"行动单位"是 `run_id`(一个 TaskRun),不是某个工具调用。** worker 的归属标识是 `WorkerTraceEvent.run_id`。

1. **判定本次 run 是否有并行 worker**:从 `useTaskRuntimeStore().activeRun` 取 `run_id`;从 `useWorkerTraceStore().workers` 筛 `runId === activeRunId` 的 worker。有则渲染「并行执行」段。
2. **渲染位置**:在主 agent 回答流里,作为独立的一段(不挂在某个工具调用下)。位置规则——若主 agent 有 `executionRounds`,「并行执行」段接在最后一个 round 之后、最终正文之前;若无 rounds,直接在最终正文之前。
3. **多个并行 worker**:同一「并行执行」段下并排 N 个嵌套块(每个 worker 一块),按 `startedAt` 排序。
4. **关联不到时**:无 worker(`runId` 不匹配或 store 为空)则不渲染「并行执行」段,不报错。

> 设计理由:并行 worker 是框架在 run 级别派发的,不是主 agent 某个 ReAct 步骤里调出来的。它们和主 agent 的 `executionRounds` 是平行的两套数据。因此 sub-agent 不挂在主 agent 的某个工具调用下,而是作为 run 级别的并行行动段呈现。

#### 3.3.3 嵌套块结构(每个 worker)

**折叠态**(默认):header 一行,显示标题 + 状态 + **进度摘要**,让用户不展开也能看到 sub-agent 在干什么(吸收 ZCode 主界面的做法)。

```
┌─ 并行执行 ─────────────────────────── N 个 worker ──┐
│ ▸ sub-agent: reviewer ── [运行中]  3 工具 · 已读 2 · 思考 4 轮
│ ▸ sub-agent: explorer ── [已完成] 5 工具 · 已读 8
└────────────────────────────────────────────────────────┘
```

**展开态**:标题下三段折叠(提示词 / 执行过程 / 结果)。

```
┌─ 并行执行 ─────────────────────────── N 个 worker ──┐
│ ▾ sub-agent: reviewer ── [运行中]  3 工具 · 已读 2 · 思考 4 轮
│ │ ▸ 提示词                                  (默认折叠)│
│ │   <MarkdownContent content={worker.task} />        │
│ │ ▸ 执行过程                                (默认展开)│
│ │   ┌──────────────────────────────────────────────┐│
│ │   │ 思考 1  <worker_thinking_delta 拼接,markdown> ││
│ │   │ ─── 行动 1 ───                                ││
│ │   │ 🔧 <worker_tool_start/result 配对>            ││
│ │   │ 思考 2 ...                                    ││
│ │   └──────────────────────────────────────────────┘│
│ │ ▸ 结果                                    (默认展开)│
│ │   <MarkdownContent content={worker_result(worker)} />│
│ ▸ sub-agent: explorer ── [已完成] 5 工具 · 已读 8
└────────────────────────────────────────────────────────┘
```

**嵌套递归**:若 sub-agent 自己又启动了 sub-agent(`parentWorkerId` 非空的 worker),在其"执行过程"内再嵌套一层同样的块(含折叠态进度摘要)。递归渲染,用 `parentWorkerId` 链追溯。

#### 3.3.4 进度摘要的构成(折叠态 header)

折叠态 header 的进度摘要从 `worker.events` 实时统计,格式:

```
[状态]  N 工具 · 已读 M · 思考 K 轮
```

- **状态**:`worker.status` 映射:`running`→"运行中"(spinner),`completed`→"已完成"(✓),`failed`→"失败"(✗),`cancelled`→"已取消"。
- **N 工具**:`worker_tool_start` 事件计数(含未配对 result 的,表示进行中)。
- **已读 M**:`worker_tool_start` 事件中 `payload.name` 为读类工具(`read_file`/`read`/`glob`/`grep`/`list` 等,前端维护一个读类工具名集合)的数量。读类工具是 sub-agent 最常见的探索动作,单独拎出来让用户感知"它在查资料"。
- **思考 K 轮**:`worker_thinking_end` 事件计数(每段思考结束算一轮);streaming 中未结束的思考不计入,但 header 状态已是"运行中"可推断。
- **失败/取消态**:只显示状态,不显示工具数(无意义)。如 `[失败]  subagent dispatch failed: ...`。
- **实时更新**:`worker.events` 增长时(header 是 `worker.status` 和 `events.length` 的派生),header 文案随之刷新。

> 设计理由:ZCode 主界面的 sub-agent 折叠块在折叠时显示 `0/10  已搜索  已读取` 这类进度,用户不展开就能感知 sub-agent 在干什么、进度如何。EKO 的 sub-agent 同样是长时操作,折叠态进度摘要是必要的可感知性,避免用户面对一排"运行中"折叠块却不知道内部进展。

#### 3.3.5 worker 内部循环的数据还原

worker 的 `events: WorkerTraceEvent[]` 是原始事件流,需还原成"思考+行动"循环:

- **思考段**:`worker_thinking_delta` 事件序列拼接 `payload.content`,遇到 `worker_thinking_end` 结束一段。多段思考按时间顺序。
- **行动段**:`worker_tool_start` + `worker_tool_result` 配对为一个工具调用行(用 `tool_name` + args/result)。未配对的 `worker_tool_start`(streaming 中)显示为"进行中"。
- **结果**:`worker_completed` 事件的 `payload.summary`,或 `worker_token_delta` 拼接(兜底,复用现有 `workerResult()` 函数)。

#### 3.3.6 `agent_tool` 路径的处理

框架解耦后(见 §8),EKO 主 agent 的 LLM 不再能调 `agent_tool`,这条路径在产品层消失。前端无需为 `agent_tool` 做任何特殊处理——主 agent `executionRounds[].tools[]` 里不会出现 `agent_tool` 工具调用。

> 框架层 `AgentDispatchTool` 实现保留(框架通用性),其他框架用户仍可显式启用。EKO 只是不开 `register_agent_dispatch_tool`。

### 3.4 按需卡片(主窗口,触发才出现,结束即卸载)

位置在消息流末尾、流式占位之后。不是常驻 UI,状态消失立即从 DOM 移除。

| 卡片 | 触发 | 消失 |
|---|---|---|
| 计划确认 | `awaitingApproval && plan` | 用户操作后 `awaitingApproval=false` |
| 工具审批 | `approvalRequest` 存在 | 用户操作后置空(已有 `ApprovalCard`,保留) |
| 输入卡 | `inputRequest` 存在 | 提交后置空(已有 `InputCard`,保留) |
| 选择卡 | `selectionRequest` 存在 | 选择后置空(已有 `SelectionCard`,保留) |
| 执行失败 toast | `todos` 有 `failed` 项,或 `activeRun.status==='failed'` | 5s 自动 / 点查看后 / run 离开 failed |

- **计划确认卡**:从 `TaskRuntimePanel.tsx` 的 `TaskRuntimeMainPanel` 里抽成独立组件 `<PlanApprovalCard />`,主窗口和右侧栏共用同一份 store action(`approve/reject/execute`)。内容:`plan.assumptions` / `plan.risks` / 任务数 + 三按钮"执行全部 / 编辑计划 / 取消"。主窗口卡片消失后,右侧栏「任务执行进度」区仍可操作。
- **执行失败 toast**(新增):toast 横条,不占消息流位置。内容 `"有 N 项执行失败"` + "查看"按钮(点击聚焦右侧栏失败项)。**不显示失败详情**,详情归右侧栏。

### 3.5 streaming 实时填充

- `executionRounds` 在 streaming 中逐步增长,「一条流」随之追加新的思考/行动段。
- sub-agent 的 `workerTraceStore.workers[...].events` 实时增长,嵌套块内"执行过程"实时追加。
- 自动滚动:维持现有 `isNearBottomRef` 逻辑——用户在底部时跟随滚动,上滑查看时不打断。

### 3.6 明确不在主窗口出现的东西(防回退清单)

实现时若发现以下内容出现在主窗口,就是做错了:

- ❌ 路由决策卡(路由标签、route reason、permission mode、route signals、approval policy、worker 名单、"下次走 Chat/只读并行"按钮)
- ❌ 独立的「并行执行」卡片(旧 `RuntimeStoryCard` 那种带 story-dot 的整块)——并行 worker 现以内联「并行执行」段形式融入主 agent 回答流(§3.3),不再是独立卡片
- ❌ 最终任务结果卡(汇总 markdown)——仅在右侧栏可点开 `ResultFullView` 全屏
- ❌ 文件变更卡(文件列表)——归右侧栏「输出产物」
- ❌ 产出 artifact 卡——归右侧栏「输出产物」
- ❌ 审查结果卡、测试/验证卡——归右侧栏「任务执行进度」
- ❌ `ConversationTimeline` 的任何事件卡(route_decision / worker_started / llm_usage / final_answer / initial_thinking / worker_tool_call / worker_result / approval_request / error)
- ❌ Token/Cache 卡——归右侧栏
- ❌ 底部 `Clock3/Activity/ShieldAlert` 全局状态三行——删除

---

## 4. 右侧栏(RightRail)设计

推倒重来,只留三块,从上到下:

### 4.1 任务执行进度

```
┌─ 任务执行进度 ────────── [状态点 已完成/执行中] ─┐
│ <goal,截断一行,title 显示全文>                   │
│ ─────────────────────────────────────────────── │
│ sub-agent 列表:                                  │
│ [worker 1] 标题 · 状态 · 当前事件标签    [展开]  │
│ [worker 2] 标题 · 状态 · 当前事件标签    [展开]  │
│   └ 展开后:思考/工具/结果摘要(精简,非完整嵌套) │
│ ─────────────────────────────────────────────── │
│ [awaitingApproval 时] 计划确认操作区:            │
│   执行全部 / 编辑计划 / 取消                      │
│ ─────────────────────────────────────────────── │
│ [底部] 刷新按钮                                   │
└──────────────────────────────────────────────────┘
```

- 数据源:`useWorkerTraceStore` + `useTaskRuntimeStore`。
- worker 列表用精简版(一行 + 可展开摘要),**不复用**主窗口的完整嵌套渲染——右侧栏是概览,详情在主窗口。
- 计划确认操作区:和主窗口的「计划确认卡」共用同一份 store action。主窗口卡片消失后,这里仍可操作。
- **删除**现有的「进度」区(`BackgroundTask` 列表,`tasksApi` 另一套东西,用户不要)。
- **删除**底部 `Clock3/Activity/ShieldAlert` 三行全局状态。

### 4.2 输出产物

```
┌─ 输出产物 ──────────── N 改动 ──┐
│ [A] 文件名1   · 路径   ×2       │  ← 复用现有 displayedChanges
│ [M] 文件名2   · 路径            │
│ [D] 文件名3   · 路径            │
│ ────────────────────────────── │
│ artifacts(若有):               │
│  📄 artifact标题  · 路径         │
└──────────────────────────────────┘
```

- 文件改动:复用现有 `useChangesStore` + `deriveChangedFiles`,点击进 `ChangesDrawer`(保留)。
- artifacts:从 `useTaskRuntimeStore().artifacts` 取,有才显示。
- **删除**现有「输出」区上方的标题装饰,合并成一个区。

### 4.3 Token / Cache

```
┌─ Token / Cache ──── N LLM calls ──┐
│ Input: 12,345   Output: 678       │
│ Cached: 9,000   Cache write: 1,200│
│ Read rate: 73.0%   Missing: 0     │
│ model: gpt-4o                     │
│ ───────────────────────────────── │
│ [▸ 缓存诊断]  (可选展开)          │
│   ⚠ cache read 偏低 ...           │
└────────────────────────────────────┘
```

- 复用现有 `CacheUsageCard` + `cacheDiagnostics`,精简布局。
- 数据源:`cacheUsageForWorkers(visibleTraceWorkers)`。

---

## 5. Markdown 渲染修复

### 5.1 修法

`MarkdownContent.tsx` 内部 div 默认带 `md-content` 类,和传入的 `className` 合并:

```tsx
<div
  ref={ref}
  className={`md-content ${className ?? ''}`.trim()}
  style={containerStyle}
  dangerouslySetInnerHTML={{ __html: renderMarkdown(content) }}
/>
```

`MessageBubble` 外层 div 的 `md-content` 类**去掉**(避免重复,统一由 `MarkdownContent` 自带)。

### 5.2 影响范围

修复后所有调用点生效:
- 主回答(最终正文)✅
- sub-agent 提示词/结果/思考 ✅
- `ResultFullView`(右侧栏点开全屏)✅
- `MarkdownCell`(notebook)✅

---

## 6. 删除清单(AGENTS.md:无需兼容,直接删)

| 删除项 | 原因 |
|---|---|
| `ConversationTimeline.tsx` 整文件 | 主窗口不再追加,数据重复 |
| `conversationRuntimeStore.ts` | 若 grep 确认无其他引用则一并删 |
| `TaskRuntimeMainPanel` 导出 | 主窗口不再追加;计划确认逻辑抽成独立组件 |
| `RuntimeStoryCard.tsx` | 若重写后不再用则删 |
| `RightRail` 的「进度」区(BackgroundTask) | 用户不要 |
| `RightRail` 底部三行全局状态 | 用户不要 |
| `MessageBubble` 的 `ExecutionProcessBlock` 旧外壳 | 被「一条流」内联渲染取代 |
| `ChatPanel` 里 `<ConversationTimeline />` 和 `<TaskRuntimeMainPanel />` 两行挂载 | 主窗口纯对话流 |

删除时连带清理:调用点、import、测试(若有)。删完必须重新编译 + 全 feature 验证(AGENTS.md)。

---

## 7. 数据来源汇总(实现参考)

| 渲染内容 | 数据源 | 字段 |
|---|---|---|
| 主 agent 思考段 | `ChatMessage` | `executionRounds[].thinking.content` / `thinkingSegments` / `thinkingContent`(legacy) |
| 主 agent 工具调用行 | `ChatMessage` | `executionRounds[].tools[]` / `toolCalls[]` / `executionSteps`(legacy) |
| 主 agent 最终正文 | `ChatMessage` | `content` |
| 「并行执行」段(worker 列表) | `useWorkerTraceStore` + `useTaskRuntimeStore` | `activeRun.run_id` 过滤 `workers`,`runId === activeRunId` |
| sub-agent 嵌套块 | `useWorkerTraceStore` | `workers[key]`(`agentName` / `task` / `events` / `parentWorkerId` / `status`) |
| sub-agent 内部循环 | `WorkerTraceState.events` | `worker_thinking_delta` / `worker_tool_start` / `worker_tool_result` / `worker_completed` |
| 计划确认 | `useTaskRuntimeStore` | `awaitingApproval` / `plan` / `approve` / `reject` / `execute` |
| 右侧栏 worker 列表 | `useWorkerTraceStore` + `useTaskRuntimeStore` | `activeRun.run_id` 过滤 workers |
| 右侧栏文件改动 | `useChangesStore` | `files`(由 `deriveChangedFiles(messages)` 派生) |
| 右侧栏 artifacts | `useTaskRuntimeStore` | `artifacts` |
| 右侧栏 Token/Cache | `useWorkerTraceStore` | `cacheUsageForWorkers(workers)` |
| 执行失败 toast | `useTaskRuntimeStore` | `todos` 有 `failed` 项,或 `activeRun.status==='failed'` |

---

## 8. 框架解耦:agent_tool 不再注册给 LLM

### 8.1 背景

系统存在两条 sub-agent 触发路径(详见 §3.3.1):
- 路径 A(TaskRuntime `run_dag`):产品并行主路径,框架确定性派发。
- 路径 B(`agent_tool` 工具):库层 LLM 提示词驱动路径,EKO 不需要(系统提示词不引导,所有并行只读工作由路径 A 替代)。

EKO 在 `infra.rs:172` 用 `.enable_subagent()` 同时启用了两条路径。问题是 `enable_subagent` flag 在框架层耦合了两件事:① 注册 `AgentDispatchTool` 给 LLM(路径 B);② 允许 worker 注册进 registry(路径 A 依赖)。直接去掉 `.enable_subagent()` 会让 worker 注册被跳过(`capabilities.rs:291-298` early return),路径 A 失败,EKO 并行能力丧失。

### 8.2 解耦方案(跨两个子仓库)

**`echo-agent`(框架层,先改先合并)**:

1. `react/capabilities.rs:291-298`:`register_subagent_with_definition` 的 `enable_subagent` early return 去掉,或改由新 flag 控制。worker 注册不再受 `enable_subagent` 守卫。
2. `react/mod.rs:423-443`:`AgentDispatchTool` 的注册改由新独立 flag 控制(如 `register_agent_dispatch_tool: bool`,默认 false)。`enable_subagent` 不再触发工具注册。
3. `react/builder.rs` + `agent/config.rs`:新增 `register_agent_dispatch_tool()` builder 方法 + `AgentConfig.register_agent_dispatch_tool` 字段(默认 false)。保留 `enable_subagent()` 语义不变(仍控制 worker 注册基础设施),但不再连带注册 LLM 工具。
4. **不删** `AgentDispatchTool` 文件、不删 Sync/Teammate 实现、不改 examples/tests。框架通用性完整。框架用户仍可显式调 `register_agent_dispatch_tool()` 启用 LLM 工具。

**`echo-agent-cli`(产品层,后改后合并)**:

5. `echo-agent-app-core/src/infra.rs:172`:保留 `.enable_subagent()`(worker 注册照常,路径 A 正常),**不调** `register_agent_dispatch_tool()`(LLM 不再能调 `agent_tool`)。

### 8.3 跨仓库合并顺序(AGENTS.md)

- 先在 `echo-agent` 子仓库完成解耦改动 → `cargo check`/`test`/`fmt`/`clippy` 全 feature 矩阵验证 → `cargo clean` → 提交 → 合并到 echo-agent main。
- 再在 `echo-agent-cli` 子仓库完成 `infra.rs` 改动 + 前端 GUI 重构 → 验证 → `cargo clean` → 提交 → 合并到 echo-agent-cli main。
- worktree 开发期间 `Cargo.toml` 可临时指向 worktree 绝对路径,合并前必须改回相对路径(AGENTS.md worktree 规范)。

### 8.4 不在本次范围

- 不删除 `AgentDispatchTool` 实现、不删 Sync/Teammate 模式、不改框架 examples/tests。
- 不动 `WorkerTraceEvent` 协议。
- notebook / settings / 其他面板:不动(除 `MarkdownCell` 因 markdown 修复被动受益)。

---

## 9. 验收标准

1. 主窗口一条 assistant 消息 = 一条连续 markdown 流,思考段(可折叠)/ 工具调用行(可折叠)/ 最终正文按时间顺序内联,无独立卡片块、无中间细线。
2. 主窗口不再出现路由决策、并行执行、最终任务结果、文件变更、产出、审查/测试任何卡片。
3. 本次 run 派发的并行 worker 在主 agent 回答流里以「并行执行」段呈现,每个 worker 展开后显示 提示词 / 执行过程(嵌套思考+工具循环) / 结果。worker 按 `run_id === activeRunId` 关联,不依赖 `agent_tool` 工具名。
4. 右侧栏只有三块:任务执行进度 / 输出产物 / Token & Cache,无 BackgroundTask 列表、无底部全局状态三行。
5. 所有 `MarkdownContent` 调用点正确渲染 markdown(标题/列表/代码块/表格/链接/引用)。
6. 按需卡片(计划确认/工具审批/输入/选择/失败 toast)只在触发时出现,结束即消失。
7. streaming 时思考段展开实时填充,结束后折叠;用户在底部时自动滚动跟随。
8. sub-agent 折叠块在折叠态 header 显示进度摘要(`[状态] N 工具 · 已读 M · 思考 K 轮`),`worker.events` 增长时实时刷新;展开后显示 提示词 / 执行过程 / 结果 三段。
9. **框架解耦**:EKO 主 agent 的 LLM 不再能调 `agent_tool`(工具不在 tool 定义列表中);路径 A(TaskRuntime `run_dag` 并行派发)正常工作,`parallel_readonly_delegation` 路由的 worker 全部成功执行。
10. `echo-agent` 全 feature 矩阵(`--no-default-features --features subagent` 等)编译 + 测试通过;`echo-agent-cli` `cargo check --no-default-features --features gui --bin echo-agent-tauri` 通过;前端 `npx tsc -b` / `npm run build` 通过;`cargo fmt` / `cargo clippy --all-targets -- -D warnings` 全绿(AGENTS.md 验证规则)。
