# 主 Agent ReAct 主导架构重构设计

- **日期**: 2026-06-23
- **范围**: `echo-agent`(框架层 ReAct 循环 + 工具)+ `echo-agent-cli`(应用层路由/执行/事件桥接)+ 前端(渲染适配)
- **目标**: 把 `parallel_readonly_delegation` 从"TaskRuntime 批处理"重构为"主 agent ReAct 循环主导"——主 agent 作为长程 CoWork 任务的持久执行者,全程跑 ReAct,需要并行只读探查时自主派 worker(并发),收 summary 后继续推理/改代码/验证。主 agent 真正"主导",不再是路由分发器。

---

## 1. 背景与问题

### 1.1 现状:主 agent 在并行只读路由下不参与执行

当前 `parallel_readonly_delegation` 路由的执行模型是**一次性批处理**:

```
用户消息 → 路由命中 → 生成 plan(N 个 fanout worker + 1 个 summary_writer)
  → tokio::spawn(execute_run) → run_dag 并发派 N 个 worker → summary_writer 汇总
  → run 结束
```

主 agent(`echo-assistant`)在这个流程里只做一件事:路由分发。`launch_task_run_execution`(`chat.rs:1384`)在独立线程跑,主 agent 调用方早就返回了。主 agent 从不执行 ReAct,从不调 LLM,从不改代码。

前端那句"已进入 TaskRuntime..."是 `chatEventHandler.ts:192` **硬编码**的假象——主 agent 的 `message.content` 根本不是 LLM 生成的。

### 1.2 问题:无法支撑长程 CoWork 任务

用户的核心场景是长程任务协作(像一起改 GUI 改了一周)。这类任务需要:

- **主 agent 持续存活**:从任务开始到结束,持有目标、状态、历史决策
- **多轮迭代**:改代码 → 验证 → 再改 → 再验证,循环推进
- **动态派发**:遇到编译错误临时派 debugger,遇到未知代码派 explorer,不是预先 plan 能覆盖的

现有批处理模型无法支撑:run 结束就结束,没有"主 agent 接管继续"的机制;worker 角色在 plan 生成时固定,无法动态调整;主 agent 没有状态,每次都从零开始。

### 1.3 根因:执行模型错位

`parallel_readonly_delegation` 被设计成"自动化批处理"——框架全权决策,主 agent 旁观。但 CoWork 任务需要的是"主 agent 主导,框架提供能力"——主 agent 是执行主体,worker 是它的探查工具。

---

## 2. 设计原则

1. **主 agent 是唯一持久执行者**:全程跑 ReAct,持有状态,推进迭代。跨多轮用户对话存活(AgentPool 已支持,见 §4.1)。
2. **LLM 自主决策派 worker**(方案 1):主 agent 通过工具调用决定何时派、派几个、派什么角色。框架提供能力,不强制流程。
3. **顶层约束提供确定性**(方案 2 的效果):系统提示词固化 SOP、动态工具注入收敛决策范围、工具参数 schema 校验。确定性由上层编排实现,不是底层硬编码。
4. **Worker 结果隔离**:worker 的完整 trace(思考/工具)不进主 agent context;只把 worker 的最终 summary 回注主 agent。
5. **Worker 过程对用户可见**:worker 的执行过程仍通过 `worker://trace` 事件流发到前端,内联显示在主 agent 回答流里。
6. **复用现有基础设施**:AgentPool(conversation 绑定)、delegate API(Fork 并发)、worker trace 事件、prompt cache、memory。不推翻重建。

---

## 3. 架构形态

### 3.1 新执行模型

```
用户消息
  ↓
主 agent(已绑定 conversation,跨轮次持久)开始/继续 ReAct 循环:
  ┌─ 思考(基于目标 + 当前状态 + 上轮结果)
  ├─ 决策:
  │   ├─ 需要并行只读探查 → 调 delegate_readonly 工具(可多个并发)
  │   │   └─ 框架派 worker(Fork),worker 跑完回 summary
  │   │   └─ worker trace 经 SubagentEvent → worker://trace → 前端内联显示
  │   ├─ 需要改代码 → 调 write_file / edit_file / shell
  │   ├─ 需要验证 → 调 shell 跑测试/编译
  │   └─ 完成 → 调 final_answer
  ├─ 观察(worker summary / 工具结果进入 context)
  └─ 再思考(循环)
  ↓
主 agent 输出最终回答(message.content,流式 emit)
```

### 3.2 与现状的关键差异

| 维度 | 现状(批处理) | 新模型(ReAct 主导) |
|---|---|---|
| 执行主体 | TaskRuntime `run_dag` | 主 agent ReAct 循环 |
| 派发决策 | 路由 + 确定性 plan | 主 agent LLM 自主(tool call) |
| worker 角色 | plan 生成时固定 | 主 agent 动态选择 |
| 汇总 | summary_writer worker | 主 agent 自己(基于 worker summary 推理) |
| 状态 | run 结束丢失 | 主 agent 跨轮次持久(AgentPool + memory) |
| 迭代 | 单次批处理 | 多轮 ReAct 循环 |

---

## 4. 关键设计

### 4.1 主 agent 持久化(复用 AgentPool)

**事实**: `AgentPool.acquire(conversation_id)`(`agent_pool.rs:244`)按 conversation 绑定 agent 实例,复用(`agents.get_mut(conversation_id)`)。同一个会话跨用户消息用同一个 agent 实例。

**设计**: 主 agent 的持久化**已具备**,无需新建。每个 conversation 的 agent 实例跨轮次保留 memory 和 context。重构后主 agent 全程跑 ReAct,天然利用这个机制。

**状态显式化**(增强): 除 memory 外,主 agent 通过 `todo_write` 工具维护任务进度(已有 `TodoWriteTool`),跨轮次可见"当前在第几步、已完成什么、待办什么"。这是长程任务的状态锚点。

### 4.2 delegate_readonly 工具(LLM 自主派 worker)

**新增工具**: `delegate_readonly`,注册给主 agent 的 LLM。

```
工具名: delegate_readonly
参数:
  role: string        # worker 角色,如 "project_explorer" / "code_reviewer"(枚举自注册的只读 worker)
  task: string        # 派给 worker 的任务描述
  context: string?    # 可选,补充上下文
返回:
  worker 的最终 summary(string)——不是完整 trace
```

**并发**: 主 agent 在一个 ReAct turn 里调多个 `delegate_readonly`(不同 role/task),ReAct 循环现有的 `join_all`(`react_loop.rs:499`)自动并发执行。无需新建并发机制。

**实现**: 工具内部调 `delegate_to_agent_with_parent_and_cancel(role, task, run_context, cancel)`(现有 API,Fork 模式),返回 `SubagentResult.output`。

**角色枚举**: 工具的 `role` 参数 schema 从 `subagent_registry` 动态生成(注册了哪些只读 worker,枚举就列哪些)。LLM 看到可选角色,自主选择。

**与解耦的 agent_tool 的关系**: `agent_tool`(LLM 提示词驱动的通用派发)是框架层能力,其他项目可用。`delegate_readonly` 是 EKO 产品层的专用工具,只派只读 worker,有产品语义。两者可并存,但 EKO 只注册 `delegate_readonly`。

### 4.3 顶层约束(确定性 by 上层编排)

按领域场景注入不同约束:

**Coding 场景**:
- 系统提示词:`AI_CODING.prompt_suffix` 已有"实现/调试任务主 agent 直接做,不分派 worker"。保留。
- 工具注入:主 agent 始终有 `delegate_readonly`(探查用)+ write_file/edit_file/shell(改代码用)。LLM 自主选。

**学术/医学场景**(未来):
- 系统提示词固化 SOP:"必须先派 Search_Worker,拿到结果后才能派 Review_Worker"。
- 动态工具注入:命中医学场景时,只注入 `delegate_medical_reviewer` 等特定角色,收敛决策范围。

**机制**: 路由决策(`route_message`)不消失,但角色从"决定执行路径"变为"注入约束"(系统提示词变体 + 工具集裁剪)。路由结果作为建议传给主 agent,不强制。

### 4.4 Worker 结果隔离(三层上下文管理)

**第 1 层:Worker trace 不进主 agent context(核心)**

`delegate_readonly` 工具返回的是 `SubagentResult.output`(worker 最终 summary,几百字),不是 worker 的思考/工具 trace。主 agent context 只增加 summary,不增加 trace。

5 个 worker 各跑 50 步 → 主 agent context 只增加 5 段 summary(~2KB),而不是 250 步 trace(~50KB+)。

**第 2 层:历史轮次裁剪**

跨多轮用户对话时,旧轮次的详细 tool call 结果折叠成摘要。复用现有 memory 机制 + 扩展:超过 N 轮的 tool 结果由 memory store 压缩成"轮次摘要"。

**第 3 层:稳定前缀 + 缓存**

现有 prompt cache(`stable_prefix_hash`)保留:系统提示词 + 工具定义 + 早期消息作为稳定前缀命中缓存,只有最近活动进活跃区。

### 4.5 Worker 过程对用户可见(前端不变)

worker 执行时,`SubagentEvent::DispatchStarted/ThinkingDelta/ToolStarted/.../Completed` 经 `tauri/mod.rs` 桥接成 `worker://trace` 事件,前端 `workerTraceStore` 接收,`WorkerStreamBlock` 内联显示。

**这条路径完全独立于 TaskRuntime**——主 agent 用 `delegate_to_agent*` 派 worker 时,trace 照发。前端 worker 显示不需要改。

### 4.6 事件流:主 agent 输出怎么到前端

主 agent 跑 ReAct 时,它的思考/工具调用/最终正文通过现有 chat 事件流 emit:
- 思考段:`thinking_delta` 事件
- 工具调用:`tool_call` 事件
- 最终正文:`assistant_message_delta` 事件(流式)

这些是主 agent ReAct 循环的既有 emit 机制(`stream_channel.rs`),重构后照用。前端 `MessageBubble` 的一条流渲染(思考+工具+worker+正文)天然适配。

---

## 5. 现有能力归属

### 5.1 保留并复用

| 能力 | 现位置 | 新角色 |
|---|---|---|
| AgentPool(conversation 绑定) | `agent_pool.rs` | 主 agent 持久化的基础,不变 |
| delegate API(Fork 并发) | `react/mod.rs:1979` | `delegate_readonly` 工具内部调用,不变 |
| 只读 worker 注册(13 个角色) | `infra.rs:276 register_default_subagents` | 不变,worker 仍注册到主 agent 的 registry |
| SubagentEvent → worker://trace 桥接 | `tauri/mod.rs:386` | 不变,trace 照发 |
| Worker trace store(前端) | `workerTraceStore.ts` | 不变 |
| Memory + TodoWrite | `react` | 主 agent 状态锚点,增强使用 |
| Prompt cache | `llm/cache` | 不变 |

### 5.2 角色转变

| 能力 | 现角色 | 新角色 |
|---|---|---|
| 路由决策 `route_message` | 决定执行路径(强制) | 注入约束(系统提示词变体 + 工具集建议),建议性非强制 |
| `generate_parallel_readonly_plan` | 确定性生成 plan + worker 角色 | **退役**(主 agent 自主决策);planner 逻辑可保留作为"建议"传给主 agent(可选) |
| `execute_run` / `run_dag` | 执行主体 | **退役**(主 agent ReAct 取代);TaskRuntime 退为状态记录层(如保留任务进度持久化) |
| `summary_writer` worker | 汇总 | **退役**(主 agent 自己汇总) |

### 5.3 新增

| 能力 | 位置 |
|---|---|
| `delegate_readonly` 工具 | `echo-agent-cli/echo-agent-app-core`(产品层工具,调 delegate API) |
| 路由结果作为约束注入 | `infra.rs` / 系统提示词组装 |

---

## 6. 迁移路径(分阶段)

### Phase 1: 主 agent 能派 worker(最小可用)

- 新增 `delegate_readonly` 工具,注册给主 agent
- `parallel_readonly_delegation` 路由改为:不再 spawn TaskRuntime,而是让主 agent 跑 ReAct(系统提示词引导它用 `delegate_readonly` 派 worker)
- 去掉前端引导语(`chatEventHandler.ts:192`)
- 验证:主 agent 能自主派 worker、收 summary、输出最终回答

### Phase 2: 退役批处理路径

- `generate_parallel_readonly_plan` / summary_writer / `run_dag` 的 parallel_readonly 分支退役(或转为建议)
- 路由结果转为约束注入(系统提示词 + 工具集)
- 验证:Coding 场景主 agent 全程主导(派 worker 探查 + 自己改代码 + 验证)

### Phase 3: 上下文管理增强

- 历史轮次裁剪(第 2 层)
- 长程任务的 todo 状态锚点强化
- 验证:多轮迭代不爆 context

### Phase 4: 领域约束(未来)

- 学术/医学场景的 SOP 提示词 + 动态工具注入
- 路由结果精细化为领域约束

---

## 7. 风险与对策

| 风险 | 对策 |
|---|---|
| LLM 不派 worker(自主决策失败) | 系统提示词强引导 + few-shot;fallback:路由命中时框架注入"建议派 worker"上下文 |
| LLM 过度派 worker(token 爆炸) | 工具调用频率限制 + worker summary 隔离(第 1 层) |
| 主 agent context 跨轮次膨胀 | 第 2 层裁剪 + memory 摘要 |
| 迁移期间功能回退 | 分阶段,Phase 1 与现有批处理共存(路由可选走哪条),验证后再退役 |
| Worker trace 事件丢失 | delegate 路径已发 trace(§4.5),独立于 TaskRuntime,不受影响 |

---

## 8. 不在本次范围

- 多 agent 协作(teammate 模式)
- agent_tool 框架层能力的改动(保持现状,EKO 只是不注册它)
- 复杂的 plan 审批 UI(主 agent 自主决策后,plan 概念弱化)
- 后端 LLM 调用的双路径清理(fallback 路径,独立问题)

---

## 9. 验收标准

1. 主 agent 在并行只读场景下全程跑 ReAct,自主派 worker(`delegate_readonly` 工具调用),收 summary 后继续推理。
2. 主 agent 的最终回答是 LLM 生成的(`message.content`),不是前端硬编码。
3. Worker 过程经 `worker://trace` 内联显示在主 agent 回答流里(前端不变)。
4. 主 agent 跨多轮用户对话保持状态(同一 conversation 复用 agent 实例 + memory + todo)。
5. Worker trace 不进主 agent context(只回 summary)。
6. 路由决策转为约束注入(系统提示词/工具集),不强制执行路径。
7. `parallel_readonly_delegation` 的批处理路径(summary_writer / run_dag)退役。
8. Coding 场景:主 agent 能派 worker 探查 + 自己改代码 + 验证,多轮迭代。
9. `cargo check/test/fmt/clippy` 全 feature 矩阵通过;前端 `tsc/build` 通过。
