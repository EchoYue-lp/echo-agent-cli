# EKO 全链路任务生命周期追踪报告

> Historical trace. The task-plan mutation path was replaced on 2026-07-21 by
> atomic `task_create`, revisioned `task_update`, and separate `plan.json` /
> `run-state.json` projections. See `docs/2026-07-21-dynamic-plan-runtime.md`.

**审查日期**：2026-07-03
**追踪范围**：从 GUI 用户输入 → Agent ReAct 循环 → 任务计划/派发 → 任务执行 → HITL 审批，完整端到端链路。

---

## 一、全链路调用图（简化）

```
用户按 Enter
  │
  ├─ ChatInput.handleSend()                         [ChatInput.tsx:495]
  │   └─ useTauriChat.sendMessage()                 [useTauriChat.ts:142]
  │       ├─ chatStore.addUserMessage()             [chatStore.ts:112]
  │       ├─ chatStore.startAssistantMessage()      [chatStore.ts:121]
  │       └─ apiInvoke('send_chat_message', {...})  [tauri-bridge.ts:153]
  │
  ├─ [Tauri IPC 边界] ─────────────────────────────────────────────
  │
  ├─ send_chat_message()                            [chat.rs:381]
  │   ├─ 附件持久化                                   [chat.rs:394-426]
  │   ├─ Agent 路由 (pool.acquire)                   [chat.rs:437-441]
  │   ├─ HITL handler 注入                           [chat.rs:499-511]
  │   ├─ TauriChatSink 构建                          [chat.rs:541-554]
  │   └─ tokio::spawn {                             [chat.rs:607]
  │       drive_chat(&agent, &msg, res)              [chat_driver.rs:74]
  │         └─ drive_chat_inner()                    [chat_driver.rs:142]
  │             └─ agent.execute_stream_message_with_cancel()  [mod.rs:2427]
  │                 └─ run_stream_channel()          [stream_channel.rs:33]
  │                     └─ ReAct 核心循环 ───────────────┐
  │                                                      │
  ├─ [Agent ReAct Loop] ─────────────────────────────────┘
  │   │
  │   ├─ Agent 调用 task_create / task_update
  │   │   └─ PlanRevisionCommitted → plan.json + run-state.json
  │   │
  │   ├─ Agent 调用 execute_plan 工具                   [task_execute_tool.rs:147]
  │   │   ├─ [Unattended] → CP A 预检 (拒绝写操作)
  │   │   ├─ [ComplexRuntime + Attended] → Paused → 等待审批 → Running
  │   │   ├─ [ParallelReadonlyDelegation] → 直接执行
  │   │   └─ execute_run(store, ...)                  [executor.rs:202]
  │   │       └─ run_dag()                            [executor.rs:559]
  │   │           ├─ 构建 DAG (depends_on)
  │   │           ├─ Wave 调度器
  │   │           ├─ 并行派发只读任务 → subagent instances
  │   │           ├─ 串行执行变更任务 → primary agent
  │   │           └─ 返回 RunOutcome
  │   │
  │   ├─ [HITL 审批] ──────────────────────────────────
  │   │   check_tool_approval()                       [approval.rs:65]
  │   │     ├─ Hook 检查 → Allow/Deny/Block
  │   │     ├─ PermissionService 统一管线
  │   │     ├─ request_human_approval()               [approval.rs:289]
  │   │     │   └─ HitlDispatcher::request()          [dispatcher.rs:64]
  │   │     │       └─ TuiHumanLoopProvider / WS Provider
  │   │     │           └─ 前端 ApprovalCard           [ApprovalCard.tsx]
  │   │     └─ 用户决定 → oneshot channel → 返回 Agent
  │   │
  │   └─ [事件流回前端] ────────────────────────────────
  │       AgentEvent → TauriChatSink → agent_event_to_chat_event()
  │         → app.emit("chat://event", payload)
  │           → 前端 handleChatEvent() → chatStore 更新
```

---

## 二、阶段 1：用户输入 → Agent 接收

### 2.1 前端链路（TypeScript/React）

| 步骤 | 文件:行 | 操作 | 数据格式 |
|------|---------|------|----------|
| 1 | `ChatInput.tsx:495` | `handleSend()` — trim、slash 命令拦截、附件 base64 编码 | `text: string` + `Attachment[]` |
| 2 | `ChatInput.tsx:522` | `onSend(trimmed, attachmentData)` → `ChatPanel.handleSend()` | — |
| 3 | `useTauriChat.ts:142` | `sendMessage()` — 添加用户消息 + 创建 assistant 占位 | `ChatMessage` |
| 4 | `useTauriChat.ts:177` | `apiInvoke('send_chat_message', {message, attachments, conversationId, messageKey})` | JSON via Tauri IPC |

**发现的问题：**

| # | 严重度 | 位置 | 问题 |
|---|--------|------|------|
| F1-1 | 🟡 | `ChatInput.tsx:508` | `Promise.all(fileToBase64)` 无 `.catch()`。文件编码失败时 promise 未处理即 reject，而 `setText('')` / `setPendingFiles([])` 已先执行——用户输入丢失且无错误提示 |

### 2.2 Tauri IPC 层（Rust）

| 步骤 | 文件:行 | 操作 |
|------|---------|------|
| 5 | `chat.rs:381` | `send_chat_message` Tauri command — 附件持久化到磁盘 |
| 6 | `chat.rs:437` | Agent 路由：`state.app_state.connection.agent_for(conv_id)` → `AgentPool::acquire()` |
| 7 | `chat.rs:465` | 中断检测：若已有 in-progress run → emit `InterruptPrompt` + 拒绝新消息 |
| 8 | `chat.rs:485` | Cancel token 注册到 session map |
| 9 | `chat.rs:499` | HITL handler 注入：`TauriHumanLoopHandler` |
| 10 | `chat.rs:541` | `TauriChatSink` 构建 — `AgentEvent` → `ChatEvent` 转换桥 |
| 11 | `chat.rs:607` | `tokio::spawn { drive_chat(...) }` — **异步派发，立即返回 `{success: true}`** |
| 12 | `chat.rs:649` | 返回给前端 — 此时后台任务刚启动 |

**发现的问题：**

| # | 严重度 | 位置 | 问题 |
|---|--------|------|------|
| F1-2 | 🟡 | `chat.rs:418-421` | `build_message` 失败时附件静默丢弃——文件已存盘但从 LLM 上下文中移除，仅 `tracing::warn!` |
| F1-3 | 🟡 | `chat.rs:649` | 命令在 spawn 后立即返回 `{success: true}`，若 spawn 的任务立即 panic，前端已进入 "running" 状态且无后续事件扭转——依赖 spawn 内的 catch 块（`chat.rs:626-628`）补救 |

### 2.3 Chat Driver → Agent 框架

| 步骤 | 文件:行 | 操作 |
|------|---------|------|
| 13 | `chat_driver.rs:74` | `drive_chat()` — TaskRuntime run 创建（幂等） |
| 14 | `chat_driver.rs:132` | `with_run_context { drive_chat_inner() }` — 注入 run_id 到 task-local |
| 15 | `chat_driver.rs:172` | `agent.inner().read().await` — **获取 agent RwLock 读锁，持有整个 stream 生命周期** |
| 16 | `chat_driver.rs:191` | `guard.execute_stream_message_with_cancel(msg, cancel)` → 进入框架 |
| 17 | `chat_driver.rs:200` | Stream 消费循环：`while let Some(event) = stream.next()` → `sink.on_agent_event(event)` |

**发现的问题：**

| # | 严重度 | 位置 | 问题 |
|---|--------|------|------|
| F1-4 | 🟡 | `chat_driver.rs:207-209` | Stream 错误只 `tracing::warn!` + break——无 `Error` ChatEvent 发送给前端，用户看到的 assistant 消息卡在 streaming 状态 |
| F1-5 | 🟡 | `chat_driver.rs:194-198` | `execute_stream_message_with_cancel` 本身返回 Err 时，仅清理 context + 返回错误字符串——同样无 Error 事件发给前端 |
| F1-6 | 🟢 | `chat_driver.rs:172` | RwLock 读锁持有整个 stream——阻止 agent 写入（配置变更、工具注册等），但符合设计（序列化执行） |

---

## 三、阶段 2：Agent ReAct 循环 → 任务创建

### 3.1 ReAct 循环核心

**入口**：`ReactAgent::run_stream_channel()` (`stream_channel.rs:33`)
- 获取 `execution_mutex`（序列化所有 agent 执行）
- Guard 安全检查
- IntentRouter 分类（DirectAnswer / SkillRequired）

**核心循环** (`run_core_loop`, `stream_channel.rs:359`)：
```
for iteration in 0..max_iterations:
  run_compact → run_think → 分支:
    ├─ 有 tool_calls → run_tools → Continue | Finish | Abandoned
    ├─ 有文本内容 → verify_final_text → Continue | FinalText → Break
    └─ 无内容/无工具 → NoResponse → 终端错误
超出 max_iterations → MaxIterationsExceeded 终端错误
```

**8 个终止条件：**

| # | 条件 | 类型 |
|---|------|------|
| 1 | `final_answer` 工具通过 verifier | ✅ 成功 |
| 2 | LLM 文本回答通过 verifier | ✅ 成功 |
| 3 | LLM 无工具调用也无文本 | ❌ NoResponse 错误 |
| 4 | 超出 max_iterations | ❌ MaxIterationsExceeded 错误 |
| 5 | Channel 关闭（receiver dropped） | ⏹ 取消 |
| 6 | CancelToken 触发 | ⏹ 取消 |
| 7 | Intervention callback 返回 cancel | ⏹ 取消 |
| 8 | Stop hook 返回 continue_reason（一次性） | 🔄 继续一轮 |

**工具错误 vs LLM 错误的处理差异：**
- **工具错误**：软化为 `[Error] {error}` 注入上下文，循环**继续**（可自我修正）
- **LLM 错误**：通常**终端**——传播为 `Err` 结束 run

### 3.2 任务创建（历史路径，已替换）

**文件**：`task_tools.rs:419-484`

当前路径一次提交完整 DAG：
1. `ensure_run_exists()` → bootstrap `TaskRun` → transition `Running`
2. `task_create(tasks=[...])` 校验完整 DAG 并提交 revision 1
3. 后续修改通过 `task_update(base_revision, operations, reason)` 原子提交

**发现的问题：**

| # | 严重度 | 位置 | 问题 |
|---|--------|------|------|
| F2-1 | 🟡 | `task_tools.rs` vs `task_execute_tool.rs` | `task_create` 不经过 `RUN_EXECUTION_LOCKS` 保护。若 LLM 并行发出多个 `task_create` + `execute_plan`，plan 可能被并发覆写 |
| F2-2 | 🟢 | `planner.rs` | `planner.rs` 不负责创建 plan——仅含 `validate_plan_deps`（循环依赖检测 + DAG 完整性）和 `analyze_file_ownership`（文件重叠分析） |

---

## 四、阶段 3：任务计划与派发

### 4.1 execute_plan 工具

**文件**：`task_execute_tool.rs:147-530`

```
execute_plan 被调用
  ├─ 读取 run_id (task-local)
  ├─ [有 params.task] → 创建 ad-hoc run + task (内联路径)
  ├─ [无 plan] → 自动生成单任务 plan
  ├─ [Unattended] → CP A 预检 (拒绝写任务)
  ├─ [ComplexRuntime + Attended] → transition Paused → await approval → transition Running
  ├─ 获取 RUN_EXECUTION_LOCKS 锁 (防并发)
  ├─ [已完成检查] → 直接返回 summary
  └─ execute_run(store, ...)
```

**Route 类型（`router.rs`）**：
- `ComplexRuntime` — 需要审批门控
- `ParallelReadonlyDelegation` — 自动执行（默认）

### 4.2 execute_run → run_dag (DAG 调度器)

**文件**：`executor.rs:202-949`

DAG 调度核心循环：
```
1. 检查 parent_cancel → 是则返回 Cancelled
2. 若有失败任务 → 标记下游 Blocked → 全部阻塞则 Failed，否则 Paused
3. 若全部完成 → Completed
4. 刷新 in_flight（从 store 读取兄弟 run_dag 完成的终端状态）
5. 计算 frontier（依赖全部满足 + 未完成 + 不在 in_flight）
6. 若 frontier 空 + in_flight 非空 → sleep 250ms 重试
7. 若 frontier 空 + 无 in_flight + 未全部完成 → DAG stall → Failed
8. 派发 wave：每个就绪任务 spawn 到 subagent
9. 等待 wave 完成（支持中途中止）
10. 处理结果 → review gate → Completed/Failed/Paused
```

**并发控制（4 个信号量）**：
- `subagent_sem`: 最大并发只读 subagent（默认 4）
- `write_sem`: 最大并发写操作（默认 4）
- `shell_sem`: 最大并发 shell（默认 1）
- `llm_sem`: 最大并行 LLM 调用（默认 4）

**发现的问题：**

| # | 严重度 | 位置 | 问题 |
|---|--------|------|------|
| F3-1 | 🔴 | `task_execute_tool.rs:387` | **审批等待无超时**：`approval_signal.notified().await` 阻塞无限。用户永不审批时 run 永久 `Paused`，无法恢复 |
| F3-2 | 🔴 | `store.rs:854-856` | **Paused 不被启动恢复**：`recover_incomplete` 只扫描 `Running` 状态。若进程在 `Paused` 时死亡，run 永久僵尸（除非被重新 drive） |
| F3-3 | 🟡 | `store.rs:207-244` | **非原子 transition_run**：读→验证→写，无 CAS。两个并发合法 transition 可能都通过验证，最后一个写覆盖前一个 |
| F3-4 | 🟡 | `store.rs:339, 601` | **并发 plan 修改**：`insert_task` 和 `attach_plan` 读→改→写，无锁保护。并行的 `task_create` 调用互相覆盖 |
| F3-5 | 🟡 | `task_execute_tool.rs:342` | CP A 预检仅对 Unattended 执行。Attended 模式下的写任务无预检——审批门控是唯一安全网（且只是"用户点了同意"） |
| F3-6 | 🟢 | `types.rs` | `TodoStatus::Blocked` 无自动解封逻辑——仅手动/GUI 操作可解除 |
| F3-7 | 🟢 | `types.rs` | `Cancelling` 中间状态缺失——cancel 直接从 `Running` → `Cancelled`，运行中任务可能未收到取消信号 |

---

## 五、阶段 4：任务执行（Subagent Agent）

### 5.1 Subagent 派发

**`RealTaskSubagent::dispatch()`** (`executor.rs:504-553`)：
```
dispatch(task)
  → with_run_context { execute_task() }
    → [Unattended 预检] 拒绝 write/shell
    → 子 cancel token (child_token)
    → TokenGuard RAII (自动注销)
    → 标记 Running + emit started
    → 获取并发许可 (subagent_sem / write_sem / shell_sem)
    → 文件级写锁 (按文件名排序防死锁)
    → LLM 速率限制
    → Hitrisk 安全检查
    → 构建 prompt (workspace + task_context + 依赖摘要)
    → 按 kind 派发:
        ├─ ReadOnly → run_readonly_subagent() → agent.delegate_to_agent_with_parent_and_cancel()
        ├─ Implementation/Debugging → run_writer_subagent() → 同上
        └─ Verification → run_main_agent_task() → agent.execute_stream_with_cancel()
    → 持久化 TaskExecutionSummary
    → emit completed/failed 事件
```

### 5.2 Agent Pool 管理

**文件**：`agent_pool.rs`

三种 Agent 类别：

| 类别 | Key 模式 | 生命周期 | 用途 |
|------|----------|----------|------|
| 对话 Agent | `"conv-{id}"` | 长生命周期，30min 空闲超时淘汰 | 用户交互 |
| 后台 Agent | `"__background__"` | 永不淘汰 | 后台任务 |
| Subagent Agent | `"__task__:{key}"` | 短生命周期，任务完成即释放 | 任务执行 |

**`acquire()` 流程** (`agent_pool.rs:264-334`)：
1. 获取 `agents.write()` 锁（**持有整个操作，包括 async create_agent**）
2. 快速路径：已存在 → 更新 `last_used` → 返回
3. 池满 → 淘汰最老的非执行中 agent
4. 创建新 agent → 注入共享资源 → 注册工具

**Subagent Agent 关键设计**：**不注册 `ExecutePlanTool`**（`agent_pool.rs:740-742`）——防止 subagent 递归派生子任务导致死锁（§10.2）。

**清理**：每 300s 扫描一次，淘汰空闲 >30min 的非执行中 agent。

### 5.3 事件流回前端

**事件类型** (`executor.rs:52-60`)：
```rust
ExecEvent { run_id, task_id?, event, agent?, payload }
```

**事件发射路径**：
```
emit_exec(trace_sink, ev)
  → TauriChatSink 闭包
    → app.emit("execution://event", payload)
      → 前端 execution://event listener
        → 按 run_id/task_id 过滤 → 更新对应 UI 组件
```

**事件类型表**：

| 事件 | 作用域 | 触发时机 |
|------|--------|----------|
| `run_started` | Run | execute_run 开始 |
| `run_completed/failed/cancelled` | Run | Run 结束 |
| `started` | Task | 任务派发开始 |
| `thinking_started/delta/ended` | Task | Agent 思考流 |
| `token_delta` | Task | 输出 token 流 |
| `tool_started/completed` | Task | 工具调用 |
| `usage` | Task | LLM 用量 |
| `completed/failed` | Task | 任务结束 |

**发现的问题：**

| # | 严重度 | 位置 | 问题 |
|---|--------|------|------|
| F4-1 | 🟡 | `agent_pool.rs:265` | `acquire()` 持有 `agents.write()` 跨 async `create_agent()`——高负载下可能造成显著锁竞争 |
| F4-2 | 🟡 | `executor.rs:811-816` | Subagent panic 后 task 状态停留 `Running`——`JoinError` 被捕获但 task 不自动失败，阻塞 DAG |
| F4-3 | 🟡 | `agent_pool.rs:528-533` | 前台预留仅 1 slot（`max_agents - 1`）。多个 GUI 会话时后台任务可能饿死 |
| F4-4 | 🟢 | `executor.rs:673-713` | 兄弟 `run_dag` 通过 store 协调——依赖存储层的事务隔离性保证正确性 |
| F4-5 | 🟢 | `executor.rs:1240-1241` | 文件锁排序防死锁——正确实现 ✅ |

---

## 六、阶段 5：HITL 人机协同

### 6.1 审批触发链路

**框架层** (`approval.rs:65-449`)：
```
check_tool_approval(tool_name, input)
  ├─ Phase -1: 生命周期 Hook 检查 → Allow/Deny/Block（短路）
  ├─ Phase 0: PermissionService 统一管线
  │   ├─ Allow → 立即返回
  │   ├─ Deny → 返回错误
  │   ├─ RequireApproval → request_human_approval()
  │   └─ Ask → handle_ask_decision()
  │
  └─ request_human_approval()
      ├─ Notification hook (permission_prompt)
      ├─ 动态风险等级计算
      ├─ 审计日志 ApprovalRequested
      ├─ HitlDispatcher::request() ──────────┐
      ├─ 审计日志 ApprovalCompleted           │
      └─ 处理用户决定                          │
```

**应用层** (`dispatcher.rs:22-111`)：
```
HitlDispatcher::request()
  ├─ 无 provider → 自动拒绝
  ├─ 遍历 registered providers
  │   └─ provider.request() ← 5min 超时
  └─ 全部超时/失败 → 自动拒绝
```

**前端** (`ApprovalCard.tsx`)：
- 展示：工具名、prompt、可折叠 JSON args
- 4 个操作：同意 / 拒绝(附理由) / 修改(附反馈) / 本会话同意
- `isSubmitting` 防重复提交

### 6.2 run 级审批门控

**`task_execute_tool.rs:360-394`**：
```
若 route == ComplexRuntime 且 Attended：
  → transition_run(Paused)
  → register_approval_signal
  → await approval_signal.notified()   ← 阻塞！
  → remove_approval_signal
  → transition_run(Running)
```

### 6.3 审批状态机

```
check_tool_approval()
├─ Hook blocks → Err ❌
├─ Hook allows → Ok(None) ✅
├─ PermissionService::Allow → Ok(modified_args?) ✅
├─ PermissionService::Deny → Err ❌
├─ RequireApproval → request_human_approval()
│   ├─ Notification hook 自动通过 → Ok
│   ├─ Notification hook 自动拒绝 → Err
│   ├─ 用户 Approved → Ok(None)
│   ├─ 用户 ApprovedWithScope → Ok(None)
│   ├─ 用户 ModifiedArgs → Ok(Some(args))
│   ├─ 用户 Rejected → Err
│   ├─ Timeout → Err
│   └─ Deferred → Err
└─ Ask → handle_ask_decision()
    ├─ 用户文本含 "reject"/"deny" → Err
    └─ 否则 → Ok(None) (确认)
```

**发现的问题：**

| # | 严重度 | 位置 | 问题 |
|---|--------|------|------|
| F5-1 | 🔴 | `task_execute_tool.rs:387` | **审批等待无超时**（同 F3-1）——最严重的 HITL 缺陷 |
| F5-2 | 🟡 | `dispatcher.rs:80` | 多 provider 场景下超时叠加——2+ provider 全部超时时总等待 `N×5min` |
| F5-3 | 🟡 | `executor.rs:1287-1330` | Hitrisk 安全暂停无去重——用户不改 plan 直接 resume 会再次命中同一检查 |
| F5-4 | 🟢 | `ApprovalCard.tsx` | `isSubmitting` 仅客户端防重——无请求级去重 ID。若快速连续收到同一工具的审批请求，可能展示过期状态 |
| F5-5 | 🟢 | `chat.rs:491-495` | Cancel token 注册在 HITL handler 注入之前——若 cancel 在这之间到达，pending requests 为空 |

---

## 七、跨阶段全局问题

### 7.1 状态一致性问题

| # | 严重度 | 涉及阶段 | 问题 |
|---|--------|----------|------|
| G1 | 🔴 | 3, 5 | **Paused 状态恢复不完整**：`recover_incomplete` 不扫 `Paused` → 进程崩溃后 Paused run 永久僵尸。`execute_run` 虽有 zombie recovery（Paused → Failed），但只在被重新 drive 时触发 |
| G2 | 🟡 | 2, 3 | **并发 plan 修改无保护**：`task_create` 和 `execute_plan` 可能被 LLM 并行调用，plan 读写无锁 |
| G3 | 🟡 | 3, 4 | **非原子状态转换**：`transition_run` 读→验证→写无 CAS，并发场景可能丢事件 |

### 7.2 资源泄漏（跨阶段累积）

| # | 严重度 | 涉及阶段 | 问题 |
|---|--------|----------|------|
| G4 | 🟡 | 3 | `RUN_EXECUTION_LOCKS` 永不清理（之前报告 P1-1） |
| G5 | 🟡 | 4 | `file_write_locks` 永不清理（之前报告 P1-2） |
| G6 | 🟡 | 5 | `APPROVAL_NOTIFIES` 未消费时泄漏（之前报告 P1-3） |

### 7.3 Subagent 生命周期完整性

| # | 严重度 | 涉及阶段 | 问题 |
|---|--------|----------|------|
| G7 | 🟡 | 4 | Subagent panic 后 task 状态不自动失败——`JoinError` 捕获但未触发状态转换 |
| G8 | 🟢 | 4 | `TaskAgentLease::Drop` 安全网在 runtime shutdown 时可能不执行——进程退出则可接受 |
| G9 | 🟢 | 4 | Subagent 不注册 `ExecutePlanTool`（防死锁设计 ✅）——但依赖隐式契约，未来维护者可能误加 |

---

## 八、优先修复建议

### 立即修复

| # | 问题 | 改动 | 文件 |
|---|------|------|------|
| F3-1/F5-1 | 审批等待加超时 | `tokio::time::timeout(Duration::from_secs(300), signal.notified()).await` | `task_execute_tool.rs:387` |
| F3-2/G1 | Paused 启动恢复 | `recover_incomplete` 加 `&[Running, Paused]` | `store.rs:854` |
| F2-1/G2 | Plan 并发保护 | `task_create` 也获取 `RUN_EXECUTION_LOCKS` 或改 store 为 CAS | `task_tools.rs` / `store.rs` |

### 近期修复

| # | 问题 | 改动 |
|---|------|------|
| F1-4/F1-5 | Stream 错误传播到前端 | 在 `catch` 块 emit `Error` ChatEvent |
| F3-3/G3 | 原子状态转换 | store 层加版本号或 CAS |
| F4-2/G7 | Subagent panic → task 自动失败 | `JoinError` 处理中调用 `mark_task_failed` |
| F5-2 | 多 provider 超时并行化 | 改为 `select!` 而非串行迭代 |
| G4-6 | 内存泄漏清理 | 各静态 map 加 TTL 或完成时清理 |

---

## 九、值得肯定的设计

1. **Agent Pool 设计**：Subagent agent 和对话 agent 共用池但生命周期独立，`TaskAgentLease` 的 `Drop` 安全网防止泄漏
2. **DAG 调度器**：4 信号量并发控制 + 文件锁排序防死锁 + 兄弟 `run_dag` 协调——成熟的并行调度
3. **Subagent 递归防护**：`ExecutePlanTool` 不注册到 subagent（§10.2）——显式防死锁设计
4. **HITL 多层防护**：Hook → PermissionService → HitlDispatcher → Provider → 前端 ApprovalCard，每层都有 fallback
5. **事件流完整性**：`AgentEvent` → `ChatEvent` → Tauri event → 前端 store，类型转换链完整
6. **审批回退安全**：全部 provider 超时/失败 → 自动拒绝（非自动通过）——fail-closed 安全策略
