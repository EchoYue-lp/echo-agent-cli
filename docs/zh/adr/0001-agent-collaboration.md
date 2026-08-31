# ADR-0001：Agent 协同——Codex 机制与 EKO 功能设计

> 状态：Proposed
>
> 日期：2026-08-24；实现证据复核：2026-08-25
>
> 范围：Codex Desktop/App 的可观察协同机制，以及 EKO 未来的跨会话 Agent 协同功能设计。
>
> 证据标记：
>
> - **已验证**：来自当前 Codex App 暴露给模型的工具 schema，或 OpenAI Codex 开源
>   `app-server` / `exec` 协议源码。
> - **合理推断**：由可观察工具调用、事件顺序和公开协议推导出的运行方式，不代表私有
>   后台实现细节。
> - **EKO 设计**：目标行为，不表示当前代码已经具备该能力。
>
> 复核基线：当前 Codex Desktop 内置 `codex-cli 0.149.0-alpha.4`、当前会话实际暴露的
> App/Collaboration 工具 schema，以及 OpenAI Codex 源码快照
> `fde2156057c38c0227ce94c8514d04c7498df60d`（2026-08-19）。官方 OpenAI Docs 在本次
> 复核中返回 HTTP 403，因此本文不把源码内部结构写成稳定的公开产品承诺。

## 1. 背景

用户可以把一个大型工作拆成多个 Codex 任务，再建立一个总协调任务。任务之间默认拥有
独立会话上下文，但协调任务能够发现其他任务、读取进度、发送指令、等待结果，并根据依赖
关系继续推进。这个体验不是“所有会话共享一个上下文窗口”，而是三个机制叠加：

1. **独立 Thread 生命周期**：每个任务有自己的历史、turn、工具状态和执行环境。
2. **宿主维护的任务目录**：App 维护任务注册表，提供跨任务发现和状态查询。
3. **模型可调用的协同工具**：模型按需要调用发现、读取、等待、发送和生命周期工具。

本 ADR 的决策是：EKO 采用同样的“独立会话 + 共享目录索引 + 精确地址消息 + 事件等待 +
协调策略”产品模型，但把 EKO 专属策略留在应用层；通用 Agent、Subagent、DAG、取消和
事件原语仍由通用框架提供。

### 1.1 本文术语

“主 Agent”在日常讨论中有两种含义，必须先拆开，否则会把两套通信机制混成一套：

| 本文术语   | 精确定义                                                       | Codex 身份                                                |
| ---------- | -------------------------------------------------------------- | --------------------------------------------------------- |
| App 主任务 | 用户在 Codex App 中创建的独立任务                              | 一个没有 Subagent parent 的 Root Thread                   |
| 树根 Agent | 某个任务内 Subagent 树的根节点                                 | `AgentPath=/root`，同时也是该树的 Root Thread             |
| Subagent   | 由树内 Agent 通过 `spawn_agent` 创建的后代                     | 独立 Thread，带 `parentThreadId` 和 canonical `AgentPath` |
| Agent 实例 | 可跨多个 turn 复用的 Thread 身份及其历史                       | 稳定 `thread_id`，不是一次模型请求                        |
| Turn       | 一次输入到终态的运行回合                                       | 同一 Agent 可以顺序执行多个 turn                          |
| Task       | 发给 Agent 的工作说明，或 EKO 的 revisioned TaskRun graph 节点 | 不是通信地址，也不等于 Thread                             |
| Goal       | Thread 可选的持久目标                                          | 独立于当前 turn、Todo 和消息队列                          |
| Todo       | 计划/任务状态的展示投影                                        | 不是 Agent，不拥有 mailbox 或执行器                       |

本文说“主 Agent 与主 Agent 通信”时，默认指两个 App 主任务之间通信；说“树根 Agent 与
Subagent 通信”时，指同一 `AgentControl` 树内的通信。

## 2. 非目标

本 ADR 不承诺以下内容：

- 复刻 Codex 的私有后台调度器、模型提示词或隐藏策略；
- 让所有会话共享完整提示词、完整上下文或隐藏推理；
- 让一个 Agent 任意修改另一个会话的文件、权限或模型配置；
- 把跨会话协同重新建模成第二套 Task/Plan/Subagent 执行器；
- 用高频轮询替代事件、游标和持久消息；
- 把“同一个文件夹”当作唯一可见性或权限边界。

## 3. Codex 的运行时模型

### 3.1 公开的 Thread → Turn → Item

OpenAI Codex `app-server` 的公开协议把交互建模为：

```text
Thread
  └── Turn
       └── Item
            ├── user message
            ├── agent message / reasoning summary
            ├── command execution
            ├── file change
            ├── MCP / collaboration tool call
            └── error / progress item
```

- **Thread**：一段可恢复、可 fork 的会话。
- **Turn**：一次用户输入到 Agent 终态的执行回合。
- **Item**：turn 内的消息、工具调用、命令、文件变更和终态事件。

公开 app-server 支持 `thread/start`、`thread/resume`、`thread/fork`、`thread/list`、
`thread/read`、`thread/turns/list`、`thread/items/list`，以及 `turn/start`、
`turn/steer`、`turn/interrupt`。执行进度通过 `thread/started`、`turn/started`、
`item/started`、`item/updated`、`item/completed` 和 `turn/completed` 等通知表达。

公开协议还提供 `thread/goal/set`、`thread/goal/get` 和 `thread/goal/clear`，用于维护一个
持久化 goal；但当前 App 的 `list_threads` 工具 schema 没有把完整 goal 列为目录字段。
因此“Thread 内部可以有持久 goal”和“另一个 Agent 能通过目录直接读取 goal”是两件事。

这说明 Codex 的协同基础不是把多个 Agent 的文本拼到同一个 prompt，而是给每个 Thread
独立生命周期，再通过外层控制面管理 Thread。

### 3.2 App 协同工具与公开 app-server 的关系

当前 Codex Desktop 给模型暴露的 `codex_app__*` 工具是 App 层协同接口；公开
`codex app-server` 的 JSON-RPC 方法是客户端/宿主控制接口。两者有关联，但不是同一份
稳定公共 wire contract：

```text
模型
  └── codex_app__list_threads / read_thread / wait_threads / send_message_to_thread ...
        └── Codex Desktop App control plane
              └── thread/list / thread/read / turn/start / event stream
                    └── Thread runtime + local execution environment
```

当前 App 工具的名称和字段来自本地工具 schema 快照；它们可能随 App 版本变化。公开协议
的稳定参考是：

- [Codex app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
- [Codex exec JSONL events](https://github.com/openai/codex/blob/main/codex-rs/exec/src/exec_events.rs)

### 3.3 两层协同：任务内 Subagent 与 App 级 Thread

Codex 还存在一层“同一主任务内部”的 Subagent 树，不能与 App 级独立 Thread 混淆：

| 维度     | 任务内 Subagent                         | App 级独立 Thread                            |
| -------- | --------------------------------------- | -------------------------------------------- |
| 创建关系 | 有明确 parent/child 调度关系            | 所有任务在目录中是 peer，可独立由用户创建    |
| 上下文   | 从父任务按策略 fork，或只接收子任务说明 | 自己拥有完整会话历史、goal 和 turn 生命周期  |
| 发现     | `list_agents` 查看当前 Subagent 树      | `list_threads` 查看 App 任务目录             |
| 通信     | `send_message` / `followup_task`        | `send_message_to_thread`                     |
| 等待     | `wait` / 成员等待工具                   | `wait_threads`                               |
| 中断     | `interrupt_agent` 精确中断成员          | Thread 的 turn interrupt 或 App 生命周期工具 |
| 文件环境 | 默认共享父环境，也可由上层提供隔离      | 可直接项目运行，也可有独立 worktree          |

OpenAI Codex 开源仓库的 `multi_agents_v2` handlers 包含 `spawn`、`list_agents`、
`send_message`、`followup_task`、`wait` 和 `interrupt_agent`。当前任务内协同工具 schema
投影出的主要参数为：

| 工具              | 参数                                                                   |
| ----------------- | ---------------------------------------------------------------------- |
| `spawn_agent`     | `task_name`、`message`；可选 `fork_turns`、`model`、`reasoning_effort` |
| `list_agents`     | 可选 `path_prefix`                                                     |
| `send_message`    | `target`、`message`；只投递消息，不主动启动新 turn                     |
| `followup_task`   | `target`、`message`；投递消息并在目标 idle 时触发新 turn               |
| `interrupt_agent` | `target`                                                               |
| `wait_agent`      | 可选 `timeout_ms`，等待成员消息、完成或用户输入                        |

任务内 Subagent 适合当前请求的有界并行子任务；App 级 Thread 适合用户可见、可长期恢复、
彼此独立的任务。EKO 的设计必须保留这两层语义，不能用跨会话目录替代 TaskRun 内的
Subagent DAG，也不能把用户创建的独立会话降格为某个临时 Subagent。

### 3.4 四个控制面，不是一张全局消息总线

Codex 当前可观察实现至少有四个彼此独立的控制面：

```text
App 任务目录控制面
  Root Thread A <---- send_message_to_thread / wait_threads ----> Root Thread B

任务内协作控制面
  /root <---- send_message / followup_task / wait_agent ----> /root/reviewer
                                      |
                                      +----> /root/reviewer/checker

单 Thread 输入控制面
  用户输入 ---- start_or_steer_turn ----> 当前 Root Thread 的 active turn / 新 turn

事件投影控制面
  Core Event ---- app-server Item notification ----> Desktop/TUI 渲染
```

四者不能互相替代：

- App 任务目录连接的是相互独立的 peer Root Thread，不共享 `AgentControl`；
- 任务内协作只在一个 root session tree 内寻址和投递；
- 用户 steer 是目标 Thread 自己的 turn admission，不是 Agent-to-Agent 消息；
- “智能体已更新”是运行时事件的 UI 投影，不是某个 Agent 额外调用了通知工具；
- 共享 cwd、Git 仓库或 worktree 只构成文件环境关系，不自动建立消息或上下文关系。

### 3.5 Collaboration runtime 维护在哪里

任务内协作运行时在 `codex app-server` 后端进程中维护，而不是在 Desktop 渲染层或模型
上下文里维护：

```text
Codex Desktop / TUI
  |
  | app-server protocol
  v
ThreadManagerState                         进程级已加载 Thread 目录
  |- threads: thread_id -> CodexThread
  |- thread_store                         Thread 历史和元数据
  `- agent_graph_store                    持久 parent/child 拓扑
       |
       `- AgentControl                    每棵 root tree 一个，所有后代共享
            |- session_id                 树内共享 session identity
            |- AgentRegistry              AgentPath <-> thread_id
            |- V2Residency                已加载/可重载状态
            |- AgentExecutionLimiter      并发执行许可
            `- RolloutBudget              树级 rollout 预算

每个 CodexThread / Session
  |- active_turn                          当前 turn 与 TaskKind
  |- InputQueue.mailbox_pending_mails     Agent 间消息队列
  |- TurnState.pending_input              当前 turn 的 steer/pending input
  |- status watch channel                 Running/Completed/... 状态订阅
  `- rollout/event stream                 历史与 UI 事件来源
```

`AgentControl` 的源码约束是“每个 root thread/session tree 最多创建一个，spawn 出来的每个
Subagent 共享它”。因此：

- `AgentRegistry` 的作用域是当前根树，不是整个 App；
- `ThreadManagerState` 可以知道进程中所有已加载 Thread，但树内工具只能通过自己的
  `AgentControl` 解析目标；
- 两个独立 App 主任务不会因为处于同一 workspace 就进入同一个 `AgentRegistry`；
- 跨主任务通信必须经过 App 目录工具，不能拿树内相对路径越界访问。

### 3.6 子智能体树、canonical path 与“找到同族”

每个 V2 Subagent 都有两个身份：

1. `thread_id`：全局稳定的 Thread 身份，用于存储、恢复和精确调用；
2. `AgentPath`：当前根树内稳定、可读的协作地址。

典型树如下：

```text
/root
|- /root/architecture
|  `- /root/architecture/reviewer
`- /root/ci
```

`spawn_agent(task_name="architecture")` 会把调用方路径和 `task_name` 拼成子路径。路径规则是：

- 根路径固定为 `/root`；
- 名称只接受 ASCII 小写字母、数字和下划线；
- 相对引用从当前 Agent 向下解析；
- 绝对引用从 `/root` 开始，可指向同树中的父、兄弟或其它分支；
- `..` 被禁止，避免路径归一化歧义；
- 工具也接受精确 `thread_id`，但可读路径更适合模型协作。

因此 `/root/architecture` 发送给 `reviewer`，解析结果是自己的子节点
`/root/architecture/reviewer`；它若要找兄弟 `/root/ci`，必须使用 canonical
`/root/ci`，不能写 `../ci`。

寻址链路是：

```text
target 字符串
  -> 若是 ThreadId，直接采用
  -> 否则 current AgentPath.resolve(target)
  -> AgentRegistry.agent_id_for_path(...)
  -> ThreadManagerState.get_thread(thread_id)
  -> 必要时 ensure_v2_agent_loaded(...)
  -> 向目标 Session 提交 Op
```

`list_agents` 列的是当前 root tree 中仍可用的 Agent，并可用路径前缀过滤；它不是扫描所有
Codex 任务，也不是从自然语言标题猜目标。

### 3.7 三种输入队列与安全边界

“给正在运行的 Agent 追加指令”至少有三种不同入口：

| 输入来源                    | Core 入口                     | 队列/信封                             | 是否可自动启动新 turn                         |
| --------------------------- | ----------------------------- | ------------------------------------- | --------------------------------------------- |
| 用户对当前 Root Thread 输入 | `start_or_steer_turn`         | turn-local user input                 | idle 时直接启动；active regular turn 时 steer |
| Agent `send_message`        | `Op::InterAgentCommunication` | session mailbox，`trigger_turn=false` | 否；等待当前或以后某个 turn 消费              |
| Agent `followup_task`       | `Op::InterAgentCommunication` | session mailbox，`trigger_turn=true`  | 是；目标 idle 时启动 regular turn             |

所谓“安全边界”不是发送方判断“现在看起来安全”，而是目标 Session 的状态机决定：

1. 消息先进入目标 `InputQueue`，不会修改已经发给模型的请求；
2. regular turn 在下一次模型采样前，从 pending input/mailbox drain 新输入；
3. 当前模型仍在采样时，消息只能等待下一次采样；
4. 当前工具调用仍在执行时，消息等待工具调用完成后的模型边界；
5. turn 已记录用户可见 final answer 后，queue-only 子消息切到 `NextTurn`，不会偷偷延长
   用户已经看到的答案；
6. 显式 user steer、模型发起的新工具调用或明确的 follow-up 可以重新打开
   `CurrentTurn` 消费窗口；
7. Review 和 Compact turn 明确拒绝 user steer；regular turn 才可 steer；
8. turn 已经结束时，原 turn 不再可 steer；`start_or_steer_turn` 在同一次原子判定中改为
   启动新 turn，而不是先失败再由 UI 猜测重试。

所以“当前 turn 仍可接纳 steer”具体要求是：目标仍有 active task、TaskKind 是 Regular、
输入非空、指定的 expected turn ID 没有发生 ABA 式变化，并且 final-output schema 等 turn
约束兼容。判定者是目标 `Session::steer_input`；发送方 Agent、App UI 和 LLM 都无权单独
宣布成功。

### 3.8 内存状态、持久状态与恢复

当前 Codex 实现不是把整个 collaboration runtime 放进一个数据库：

| 数据                                          | 当前权威位置                         | 重启后的性质                                     |
| --------------------------------------------- | ------------------------------------ | ------------------------------------------------ |
| 已加载 Thread 句柄、active turn、status watch | `app-server` 进程内存                | 消失，必须重建                                   |
| `AgentPath <-> thread_id` 活动索引            | `AgentRegistry` 内存 HashMap         | 从持久 Thread 元数据重建                         |
| 尚未 drain 的 Agent mailbox                   | `InputQueue` 内存 `VecDeque`         | 源码未证明具备 exactly-once 持久恢复             |
| 父子边                                        | `AgentGraphStore`                    | 本地实现写入 `state_5.sqlite/thread_spawn_edges` |
| Thread 元数据和历史                           | `ThreadStore`、rollout/history store | 可恢复、分页读取或重建模型上下文                 |
| Thread goal                                   | 独立 GoalStore                       | 本地实现写入 `goals_1.sqlite`                    |

恢复根 Thread 时，runtime 先读取仍为 Open 的后代边，只恢复 path、role、nickname 和
thread identity，不立即打开所有后代运行时。后续消息命中某个未加载 Subagent 时，
`ensure_v2_agent_loaded` 才读取存储历史、恢复配置和模型上下文，再把它装回
`ThreadManagerState`。这是“身份和历史可恢复”与“当前执行仍在运行”的明确分离。

因此不能声称：Codex 当前的内存 mailbox 已经提供 durable exactly-once。EKO 在 7.7 和
9 节提出的 durable mailbox、幂等 message ID 和 delivery receipt 是更强的产品目标，不是
对 Codex 当前实现的照抄。

### 3.9 Agent 状态、turn 终态与长时间复用

当前树内 `AgentStatus` 包含：`PendingInit`、`Running`、`Interrupted`、
`Completed(final_message?)`、`Errored(error)`、`Shutdown`、`NotFound`。

这里最容易误解的是 `Completed`：它主要表达当前 turn 已结束，不等于这个 Thread 身份被
删除。只要父子边和 Agent metadata 仍存在，协调者就可以用 `followup_task` 给同一个
Subagent 新任务；目标继续使用原 `thread_id`、`AgentPath`、历史和角色，再启动下一个 turn。

所以 UI 中一个 Subagent 显示运行或存在十几个小时，不能直接解释为“一次 LLM sampling
持续了十几个小时”。它可能经历：

```text
spawn
  -> turn 1 完成
  -> idle / 等待
  -> followup_task
  -> turn 2 完成
  -> 等待依赖、工具或用户
  -> turn 3 ...
```

是否继续使用旧 Agent，不由经过时长决定，而由上下文连续性决定。新工作依赖原历史、角色、
文件所有权或未完成推理时，复用同一个 Subagent 更合理；工作目标完全独立、需要隔离历史或
并行执行时，才 spawn 新 Agent。

### 3.10 Goal、Task、Todo 与 Subagent 的关系

Codex 当前协议为每个 Thread 提供可选的持久 `ThreadGoal`，字段包括 objective、status、
token budget、tokens/time used 和时间戳；状态包括 Active、Paused、Blocked、UsageLimited、
BudgetLimited、Complete。它通过 `thread/goal/set|get|clear` 独立维护。

但这不意味着每次 `spawn_agent` 都自动创建一个 Goal，也不意味着后续消息会自动重写 Goal：

- `spawn_agent.message` 是新 Subagent 的首个任务输入；
- `followup_task.message` 是同一 Agent 的后续任务输入；
- `ThreadGoal` 只有显式 set/update 才是持久目标；
- turn status 只描述一次运行回合；
- Todo/Plan 描述“接下来做什么”，不拥有 Agent 身份；
- TaskRun/PlanTask 描述 EKO 的工作图，不替代消息路由。

合理模型是：Goal 提供长期方向，Task/PlanTask 提供可执行工作单元，Todo 提供当前投影，
Subagent 提供执行身份，Turn 提供一次执行尝试，message/receipt 提供因果连接。不要把这六个
概念压成一个状态机。

## 4. Codex 协同工具

### 4.1 工具总览

| 工具                                | 作用                                                | 典型触发时机                                |
| ----------------------------------- | --------------------------------------------------- | ------------------------------------------- |
| `codex_app__list_threads`           | 发现任务目录中的 Thread/Chat                        | 协调任务启动、用户新增任务、目录可能变化时  |
| `codex_app__read_thread`            | 读取一个精确 Thread 的近期状态和摘要                | 发现候选任务后确认目标、进度、依赖          |
| `codex_app__wait_threads`           | 对最多 8 个 Thread 做带游标的事件等待               | 已知任务 ID 后等待完成或需要处理            |
| `codex_app__send_message_to_thread` | 给指定 Thread 发送用户可见的 follow-up prompt       | 下发任务、纠偏、询问状态、主动汇报          |
| `codex_app__create_thread`          | 创建用户拥有的新任务                                | 只有用户明确要求创建新任务时                |
| `codex_app__fork_thread`            | 从已完成历史 fork 新 Thread                         | 需要复制历史并建立独立分支时                |
| `codex_app__handoff_thread`         | 移动 Thread 及其 Git 状态到另一个 checkout/worktree | 需要切换执行位置或宿主时                    |
| `codex_app__read_thread_terminal`   | 读取当前 App terminal 输出                          | 需要确认终端提示、后台命令或当前 shell 状态 |
| `codex_app__set_thread_*`           | 标题、置顶、归档等 UI 生命周期操作                  | 用户或宿主管理任务时                        |

`list_threads`、`read_thread`、`wait_threads` 和 `send_message_to_thread` 是跨会话协调的
核心四件套；其余工具负责创建、分叉、迁移和 UI 生命周期。

### 4.2 `list_threads`

**参数**：

```json
{
  "limit": 30
}
```

`limit` 可选，限制非置顶 Thread 摘要数量；置顶任务会单独返回。工具描述明确说它列出
App 范围内的 Thread/Chat，所有任务是平级 peer，不因为是否由当前任务委派而改变目录身份。

**返回的单任务元数据**通常包括：

| 字段        | 含义                               |
| ----------- | ---------------------------------- |
| `id`        | 精确 Thread ID                     |
| `kind`      | Codex 或其他会话来源类型           |
| `hostId`    | 运行该 Thread 的宿主               |
| `projectId` | App 项目归属，可为空               |
| `cwd`       | 工作目录                           |
| `status`    | 例如 `active`、`idle`、`notLoaded` |
| `updatedAt` | 最近更新时间                       |
| `title`     | 来源系统提供的标题                 |
| `summary`   | 用于检索和选择的简短摘要           |

它**不直接返回**：

- 完整系统提示词、developer 指令或隐藏规则；
- 一个可被其他任务直接修改的 `goal` 对象；
- 完整上下文窗口；
- 全部文件内容、命令输出或隐藏推理；
- 其他 Thread 的模型内存。

因此 `list_threads` 只能完成“发现和粗筛”，不能单独完成目标理解。标题和摘要是选择
线索，不是可信指令，必须当作不可信数据处理。

### 4.3 `read_thread`

**参数**：

```json
{
  "threadId": "01...",
  "hostId": "local",
  "cursor": "older-page-cursor",
  "turnLimit": 4,
  "includeOutputs": false,
  "maxOutputCharsPerItem": 3000
}
```

只有 `threadId` 必填。它读取一个精确任务的近期状态和 turn 摘要，可选返回截断的工具或
命令输出。协调任务通常通过它确认：

- 最近的用户目标和 follow-up；
- 任务当前执行到哪一步；
- 已完成的提交、测试和结果；
- 等待哪些依赖；
- 是否需要人工输入或纠偏。

这仍然不是“读取另一个 Agent 的完整 prompt”。能看到的内容取决于宿主允许投影的会话
记录；隐藏系统消息、未投影的上下文和原始隐藏推理不因 `read_thread` 自动泄露。

### 4.4 `wait_threads`

**参数**：

```json
{
  "targets": [
    {
      "threadId": "01...",
      "hostId": "local",
      "afterCursor": "cursor-from-previous-wait"
    }
  ],
  "timeoutMs": 120000
}
```

约束和语义：

- 一次最多等待 8 个目标；
- `afterCursor` 防止重复交付已经消费的结果；
- 第一个 Thread 完成或需要处理时唤醒；
- `timeoutMs: 0` 用于即时快照；
- 新的用户输入会结束等待；
- 普通 commentary 不会反复唤醒等待；
- 超时会返回所有目标的紧凑进度，而不是让模型重新扫描全部历史。

因此协调任务的常见循环是：

```text
wait_threads
  -> 某个任务完成/需要处理
  -> 读取增量结果
  -> 分析依赖和下一动作
  -> send_message_to_thread（必要时）
  -> wait_threads（继续等待）
```

这不是模型每秒轮询 `list_threads`；它是“稳定 Thread ID + 游标 + 事件等待”。

### 4.5 `send_message_to_thread`

**参数**：

```json
{
  "threadId": "target-thread-id",
  "hostId": "local",
  "prompt": "请基于最新基线完成定向复验并汇报提交和测试结果。",
  "model": "optional-explicit-override",
  "thinking": "optional-reasoning-override"
}
```

`prompt` 会作为目标任务中用户可见的 follow-up 消息出现，不是隐形共享内存。通常用于：

- 下发子任务或明确验收标准；
- 通知新的提交、依赖门或合并顺序；
- 要求停止写入、只读审计或回报现状；
- 目标任务主动向协调任务发送完成报告。

目标任务可以主动发消息给协调任务，也可以只正常完成，由协调任务的 `wait_threads` 观察
完成事件。两种回传通道可以同时存在。

### 4.6 其他 Thread 管理工具参数

这些工具不是完成协同所必需的，但会影响任务是否可被继续、定位和展示：

```jsonc
// set_thread_title
{ "threadId": "01...", "title": "新的任务标题" }

// set_thread_archived
{ "threadId": "01...", "hostId": "local", "archived": true }

// set_thread_pinned
{ "threadId": "01...", "pinned": true }

// list_archived_threads
{ "hostId": "local", "cursor": "next-page", "limit": 10 }

// share_thread
{ "threadId": "01...", "hostId": "local" }
```

`set_thread_title`、置顶和归档只改变宿主目录中的管理投影，不改变目标 Agent 的 goal 或
执行权限。归档任务不应被默认的自动协同扫描选中，但在用户明确恢复或指定 ID 后仍可继续。
`share_thread` 生成不可变分享链接，属于用户主动分享能力，不应被协调器自动调用。

### 4.7 创建、分叉和迁移

`create_thread` 的关键参数是：

```json
{
  "prompt": "初始任务",
  "title": "任务标题",
  "target": {
    "type": "project",
    "projectId": "project-id",
    "environment": { "type": "worktree" }
  },
  "model": "optional-explicit-model",
  "thinking": "optional-effort"
}
```

`target` 还可以是 `projectless` 或明确的 `chatgptWorkCloud`。项目 Git 仓库默认使用
worktree，除非用户明确要求直接使用保存的项目。创建操作是异步的，可能先返回
`clientThreadId`，待 worktree 准备完成后才得到正式 `threadId`。

`fork_thread` 支持 `same-directory` 或 `worktree`；fork 只复制已完成历史，不复制正在运行
的 turn 和未完成响应。`handoff_thread` 在迁移前会中断运行中的 Thread，并返回后台操作
标识；迁移状态需要单独查询。

```jsonc
// fork_thread；threadId 可省略，表示 fork 当前任务
{
  "threadId": "01...",
  "environment": { "type": "worktree" }
}

// handoff_thread；destinationHostId 省略时在当前宿主的 checkout/worktree 之间切换
{
  "threadId": "01...",
  "destinationHostId": "local",
  "followUpPrompt": "迁移完成后继续执行集成复验。"
}

// read_thread_terminal 无参数
{}
```

### 4.8 任务内 Collaboration 工具的真实职责

当前 MultiAgent V2 的六个任务内工具形成一个完整但刻意很小的控制面：

| 工具              | 改变的事实                                                        | 不做什么                                          |
| ----------------- | ----------------------------------------------------------------- | ------------------------------------------------- |
| `spawn_agent`     | 创建子 Thread、分配 canonical path、登记父子边、提交首个 NEW_TASK | 不创建 App peer 任务，不共享可变上下文窗口        |
| `list_agents`     | 从当前 root tree 的 registry 返回 Agent path/status               | 不扫描 App 全部任务，不读取完整历史               |
| `send_message`    | 投递 `trigger_turn=false` 的 MESSAGE                              | 不为普通 idle Agent 启动新工作，不中断当前工具    |
| `followup_task`   | 投递 `trigger_turn=true` 的 NEW_TASK                              | 不创建新 Agent，不允许把 root 当 follow-up target |
| `interrupt_agent` | 给精确的非 root、非自身目标发送 `Op::Interrupt`                   | 不删除 Thread，不清空历史，不改变长期 goal        |
| `wait_agent`      | 订阅当前 Session 的 mailbox/steer activity，等待任意更新          | 不返回所有消息正文，不持续占用模型采样            |

`send_message` 和 `followup_task` 的 handler 只接受 `target`、`message`，拒绝未知字段；二者
共用同一条 `handle_message_string_tool` 提交路径。差异不是两套队列，而只是：

```text
send_message  -> MessageDeliveryMode::QueueOnly  -> trigger_turn=false
followup_task -> MessageDeliveryMode::TriggerTurn -> trigger_turn=true
```

两者都会先解析目标、确认目标属于当前树、必要时从持久历史加载目标，再构造：

```text
InterAgentCommunication {
  author: AgentPath,
  recipient: AgentPath,
  other_recipients: [],
  content,
  trigger_turn,
  ...
}
```

然后通过 `AgentControl -> ThreadManagerState.send_op -> target Session::InputQueue` 投递。工具
调用成功表示目标 Session 已接受这次提交，不等于目标已经理解、执行或完成消息中的任务。

### 4.9 全方向通信矩阵

下面这张表覆盖本文讨论的主 Agent、Subagent、用户和运行时之间的全部主要方向：

| 来源             | 目标                         | 正确通道                                           | 目标 turn 行为                                         | 自动回传                                                    |
| ---------------- | ---------------------------- | -------------------------------------------------- | ------------------------------------------------------ | ----------------------------------------------------------- |
| App 主任务 A     | App 主任务 B                 | `send_message_to_thread`                           | B 收到用户可见 follow-up；由 B 的宿主做 turn admission | 无 parent completion；A 应 `wait_threads` 或等待 B 主动回复 |
| 树根 `/root`     | 直接/间接 Subagent           | `send_message`                                     | 正在运行则在消息边界处理；idle 时仅排队                | mailbox activity 可唤醒 `wait_agent`                        |
| 树根 `/root`     | 直接/间接 Subagent           | `followup_task`                                    | 正在运行则在边界处理；idle 时启动新 turn               | 新 turn 终态会通知直接 parent                               |
| Subagent         | 直接 parent                  | `send_message`                                     | parent 当前 turn 边界处理；idle 时仅排队               | parent 的 `wait_agent` 被 mailbox activity 唤醒             |
| Subagent         | `/root`                      | `send_message("/root", ...)`                       | 同上                                                   | 不自动启动 idle root turn                                   |
| Subagent         | `/root`                      | `followup_task`                                    | **拒绝**；root 不是 spawned agent                      | 无                                                          |
| Subagent         | 非 root parent/兄弟/其它分支 | `send_message` 或 `followup_task` + canonical path | 按 queue-only/trigger-turn 语义处理                    | 只对 direct parent 有自动终态通知                           |
| Subagent         | 自己的新子节点               | `spawn_agent`                                      | 建立下一层 Thread 和 path                              | 子节点每个终态 turn 通知它的直接 parent                     |
| Subagent runtime | 直接 parent                  | 自动 completion MESSAGE                            | `trigger_turn=false`，不会擅自启动 parent 新 turn      | 这是回传本身                                                |
| 用户/App         | App Root Thread              | `turn/start` / start-or-steer                      | active regular 时 steer；idle 时新 turn                | 正常 turn event stream                                      |
| 用户/App         | V2 spawned Subagent          | 直接 app-server turn input                         | **拒绝**，`canAcceptDirectInput=false`                 | 应由树内 Agent 使用 collaboration 工具                      |
| Core runtime     | UI                           | Item/Event notification                            | 不影响模型运行                                         | UI 渲染“智能体已更新”等状态                                 |

“主 Agent 与子 Agent 可以双向通信”并不表示双方拥有完全对称的控制权。Subagent 可以给
root 发 MESSAGE，但不能用 `followup_task` 强制 root 启动新 turn，也不能 interrupt root。
这让根协调者保持调度权，同时允许后代及时汇报。

### 4.10 `send_message` 与 `followup_task` 的状态真值表

| 目标状态                                        | `send_message`                            | `followup_task`                                  |
| ----------------------------------------------- | ----------------------------------------- | ------------------------------------------------ |
| regular turn 正在采样，尚未 final               | 入 mailbox；下一次模型边界 drain          | 同一 mailbox；下一次模型边界 drain               |
| 工具调用正在执行                                | 等工具完成后的边界，不抢占工具            | 等工具完成后的边界，不抢占工具                   |
| active turn 已输出可见 final，但尚未完全 settle | queue-only 留给以后 turn                  | 保持 trigger work；当前 turn 结束后可拉起新 turn |
| idle                                            | 只排队，不产生模型调用                    | 运行时保留启动槽并创建 regular turn              |
| 持久 Thread 未加载                              | 先 `ensure_v2_agent_loaded`，再按上述规则 | 先加载，再按上述规则                             |
| target 是 `/root`                               | 允许                                      | 拒绝                                             |
| target 不在当前 root tree                       | 解析失败                                  | 解析失败                                         |

一个窄例外是目标带有 outstanding durable sleep：queue-only 消息可以唤醒这段已经登记的延续
工作。它不是把普通 `send_message` 升格成 NEW_TASK，而是让原有 durable continuation 响应
mailbox activity。

所以二者不是“steer 成功/steer 失败的自动降级关系”。它们表达发送方意图：

- `send_message`：这是一条信息；不要因为它单独唤醒一个 idle Agent；
- `followup_task`：这是一项新工作；如果目标没有运行，就为同一个 Agent 启动新 turn。

最终消息进入当前 turn 还是下一个 turn，仍由接收方 Session 在原子状态和 mailbox phase 上
决定。发送方只选择是否允许触发 turn，不能指定“必须插入第 N 次模型请求”。

### 4.11 Subagent 到 parent 的结果回传

V2 Subagent 的每一个 terminal turn 都会由目标 Session 自动构造 completion envelope 发给
它的**直接 parent**。触发点是 `TurnComplete` 或 `TurnAborted`，流程为：

```text
child turn terminal
  -> 归约 AgentStatus
  -> 生成标准 completion message
  -> author=child AgentPath
  -> recipient=direct parent AgentPath
  -> trigger_turn=false
  -> parent mailbox
```

边界细节：

- `Completed(Some(message))` 携带 child 最终回复；
- error 会被截断并附带“如仍需要此 Agent，请再分配任务”的下一步；
- completion envelope 的预算有界，当前实现上限为约 1,000 tokens；
- Running、PendingInit、Interrupted 不产生完成消息；
- 每次 follow-up turn 完成都再次通知，不是一个 Thread 一生只通知一次；
- 只通知直接 parent，不广播给 root 和所有祖先；
- 消息使用 `trigger_turn=false`，因此不会在 parent idle 时制造一次意外模型调用；
- parent 若正在 `wait_agent`，mailbox watch 会结束等待，随后 parent 在自己的 turn 中读取消息。

这种设计把“结果一定可被协调者观察”与“是否立即花一次模型调用处理结果”拆开了。

### 4.12 Subagent 与 Subagent 通信

Subagent 之间不需要经过 root 转发。它们共享 root-scoped `AgentControl`，因此任何已知路径
都可以直接成为消息目标：

```text
/root/implementation
  --send_message("/root/review", "commit 已准备好，请开始复审")-->
/root/review
```

但“能发消息”不等于“应该共享写权限”。协调 prompt、PlanTask ownership 或应用层 policy
仍应约束：

- 谁拥有某组文件的写入权；
- 谁只做只读 review；
- 谁可以宣布集成门通过；
- 消息是依赖已满足的证据，还是仅仅一个未经核验的声明。

直接点对点消息减少 root 的转发负担；root 仍通过 `list_agents`、终态通知和必要的显式
消息保持全局协调。

### 4.13 App 主任务与 App 主任务通信

两个 App 主任务都是 peer，不存在自动 parent/child 边。协调者需要显式完成四步：

```text
list_threads              发现稳定 threadId/hostId
  -> read_thread           核对目标、基线和状态
  -> send_message_to_thread 下发用户可见 follow-up
  -> wait_threads          用 cursor 等待完成或 needs-attention
```

`send_message_to_thread` 的 prompt 在目标任务中显示为用户消息；它不是隐藏的 system prompt，
也不会把 A 的完整上下文复制到 B。A 若希望 B 知道某个提交、文件或验收标准，必须把必要信息
明确写进消息，或提供可解析 artifact/commit 引用。

由于没有树内 parent 边：

- B 完成时不会自动向 A 发送 direct-parent completion；
- A 应使用 `wait_threads` 观察 B，或要求 B 用同一 App 工具主动回报 A；
- App 目录中的 title/summary 只是发现信息，不是可信指令；
- 两个任务即使共享 cwd，也仍有独立 transcript、turn、goal、权限与可能冲突的文件写入。

### 4.14 用户为什么不能直接给 V2 Subagent 输入

当前 app-server Thread 投影包含 `canAcceptDirectInput`。对 MultiAgent V2 的
`ThreadSpawn` Subagent，该字段是 `false`，直接 `turn/start` 会返回：

```text
direct app-server input is not allowed for multi-agent v2 sub-agents
```

因此用户点开 Subagent 详情看到的是可观察 Thread，而不是另一个完全独立的 App 输入口。
新的工作仍由它所属树中的 Agent 通过 `followup_task` 投递，所以 UI 会继续显示在原来的
Subagent 身份和历史下，而不是自动新开一个 Agent。

这个约束同时保留了两点：

- Subagent 是完整可恢复 Thread，可以拥有多轮历史并被复用；
- Subagent 的调度归属仍在原 root tree，不会因用户从 App 任意注入而绕过父任务协调。

### 4.15 “智能体已更新”是怎样产生的

它不是第七个 collaboration tool，也不是 Subagent 主动调用一个“通知 UI”工具。V2 工具
handler 在完成控制操作后会发结构化 `SubAgentActivity`：

| 操作                                      | `SubAgentActivityKind` |
| ----------------------------------------- | ---------------------- |
| spawn 成功                                | `Started`              |
| `send_message` / `followup_task` 投递成功 | `Interacted`           |
| interrupt 完成                            | `Interrupted`          |

事件包含 event/call ID、目标 `agent_thread_id`、canonical `agent_path` 和 kind。app-server 把
它映射为 `ThreadItem::SubAgentActivity`，再通过 `item/completed` 通知客户端；Desktop 根据
kind 和本地化文案渲染成“智能体已更新”等条目。

旧/兼容事件中还有 `CollabAgentToolCallItem`，可携带 tool、sender thread、receiver threads、
prompt、status 和 receiver states。无论哪种投影，UI 事件都是 runtime tool handler 的副作用：

```text
Agent 调 collaboration tool
  -> Core 修改 Thread/registry/mailbox
  -> Core emit structured activity item
  -> app-server 转换并广播 item notification
  -> UI reducer/render
```

因此连续出现三条“智能体已更新”，通常表示发生了三次可观察协作活动，不表示创建了三个新
Agent，也不表示每条都已经产生最终业务结果。应展开对应 Subagent 或结合终态消息判断具体
发生了什么。

## 5. 工具触发与协同调度

### 5.1 发现不是持续轮询

`list_threads` 是发现/刷新目录，不是监听器。合理的触发条件：

1. 协调任务刚启动，需要建立任务地图；
2. 用户声称又创建了任务，或当前目录可能发生变化；
3. 现有 Thread ID 不可用，需要重新解析宿主；
4. 一个协调阶段完成，需要重新评估整个任务图。

拿到稳定 Thread ID 后，应优先使用 `read_thread` 和 `wait_threads`，避免频繁全量扫描。

### 5.2 目标识别不是单字段匹配

协调 Agent 通常按照以下顺序确认目标：

```text
projectId / cwd / hostId
  + title / summary
  + read_thread 的近期用户目标和回报
  + Git 分支、提交、测试与依赖关系
  + 发送一条明确的确认或委派消息
```

“同一个文件夹”只能作为相关性线索，不能作为任务归属、权限或写入授权的唯一依据。

### 5.3 并行度会自然收缩

大型任务通常采用：

```text
fan-out：独立调查和实现并行
   ↓
barrier：公共 API、基线提交和测试门禁收敛
   ↓
fan-in：一个集成/协调任务合并、复验和收口
```

后期通常只有一个活跃任务，不主要是因为各 Agent 的 goal 自动改变，而是因为：

- 依赖链变长，ready frontier 变窄；
- 多个任务需要同一个公共 API 或 main 基线；
- 合并、锁文件、生成文件和最终门禁需要单一写入权威；
- 继续并行会基于旧基线产生冲突或重复实现。

goal 应保持稳定；改变的是代码基线、依赖状态和下一步可执行条件。

### 5.4 `active`、`idle`、`notLoaded`

- `active`：当前有执行中的 turn 或工具操作；
- `idle`：当前没有执行 turn，可能已完成、正在等待 follow-up，或暂时没有工作；
- `notLoaded`：持久化 Thread 存在，但当前未加载到内存；需要 resume/唤醒后才能继续。

“idle”不等于“退出协作”；协调者仍可通过精确 Thread ID 发送 follow-up。

### 5.5 一次完整的树内协作时序

以下时序把 spawn、点对点通信、等待、自动结果回传和 Agent 复用连起来：

```text
User
  |  “实现功能并独立复审”
  v
/root turn-1
  |-- spawn_agent("implementation", initial task) --> /root/implementation turn-1
  |-- spawn_agent("review", review contract) ------> /root/review turn-1
  |
  |-- wait_agent ----------------------------------- waiting
  |
/root/implementation
  |-- tools / code / tests
  |-- send_message("/root/review", commit + evidence)
  `-- terminal --> automatic completion MESSAGE --> /root

/root/review
  |-- mailbox receives commit + evidence
  |-- review and report findings
  `-- terminal --> automatic completion MESSAGE --> /root

/root wait_agent wakes
  |-- consumes both mailbox messages
  |-- decides implementation needs one correction
  |-- followup_task("/root/implementation", correction + acceptance gate)
  `-- wait_agent

/root/implementation turn-2
  |-- reuses original Thread/history/path/role
  `-- terminal --> automatic completion MESSAGE --> /root

/root
  `-- verifies evidence and completes its own turn
```

这个过程中没有任何全局共享 prompt：共享的是精确地址、短消息、状态事件、代码/commit 和
可核验 artifact。每个 Agent 只在自己的 turn 中决定如何处理收到的信息。

### 5.6 复用旧 Agent、发消息、追加任务还是新建 Agent

| 意图                                              | 正确动作                      | 原因                               |
| ------------------------------------------------- | ----------------------------- | ---------------------------------- |
| 告知一个正在工作的 Agent 新事实，不要求它单独醒来 | `send_message`                | queue-only，最少打扰               |
| 要求已有 Subagent 继续做下一项相关工作            | `followup_task`               | 保留历史和身份，idle 时自动新 turn |
| 目标与原工作独立，需要并行或上下文隔离            | `spawn_agent`                 | 新 Thread、新 path、新执行身份     |
| 需要用户长期拥有、侧边栏可见的独立主任务          | `create_thread`               | App peer，不属于临时 Subagent 树   |
| 给另一个 App 主任务纠偏或追加验收条件             | `send_message_to_thread`      | 用户可见的跨任务 follow-up         |
| 只是等待结果                                      | `wait_agent` / `wait_threads` | 事件驱动，不制造无意义轮询和 turn  |

“初始任务肯定会变化”不是必须新建 Agent 的理由。变化如果是同一责任域内的下一步，应该
通过 follow-up 演进；变化如果改变了所有权、隔离需求、上下文前提或需要并行，才建立新
Agent。稳定的是执行身份和因果历史，不是第一条 task 文本永远不变。

## 6. 隔离、可见性与权限

### 6.1 三种隔离必须分开

| 隔离层   | 隔离内容                                             | 共享内容                        |
| -------- | ---------------------------------------------------- | ------------------------------- |
| 会话隔离 | transcript、turn、模型上下文、工具状态、goal、内存   | 受控任务元数据和消息摘要        |
| 执行隔离 | cwd、sandbox、permission profile、环境变量、审批状态 | 显式授予的能力和宿主资源        |
| 文件隔离 | checkout/worktree、文件所有权、并发写入范围          | 只读公共基线或显式共享 artifact |

同一 `cwd` 不等于共享会话上下文；同一项目也不等于允许互相写文件。真正的文件隔离
依赖 worktree、文件所有权或显式锁。

### 6.2 可见性维度

Codex App 的 `list_threads` 描述为跨 App 列举任务，而不是只列当前文件夹。任务条目会
携带 `hostId`、`projectId` 和 `cwd`，协调 Agent 可以据此筛选项目相关任务。可见性应理解为：

```text
账户/App 可发现
  -> host/project/cwd 元数据筛选
  -> Thread 读取能力检查
  -> 具体消息或执行权限检查
```

可发现不等于可读取完整历史；可读取摘要不等于可执行命令；可发送消息不等于可写入目标
worktree。当前 App 的内部授权细节没有公开完整文档，不能把 `list_threads` 的返回结果
解释成“对所有任务拥有全部权限”。

### 6.3 执行权限与协同权限分离

公开 `app-server` 的 `turn/start`、`thread/start`、`thread/resume` 和 `thread/fork` 支持
sandbox、permissions profile、approval policy 等执行配置。它们控制目标 Thread 自己能做
什么；协同工具只控制发现、阅读、发送和生命周期操作。

EKO 也必须分离：

- **Discovery**：能否看到任务元数据；
- **Inspect**：能否看到有界目标/进度/结果摘要；
- **Message**：能否向目标发送 follow-up；
- **Control**：能否 steer、interrupt、resume 或 handoff；
- **Execute**：能否在某个 workspace/worktree 使用工具和文件。

默认不得由“同项目”自动推导全部五种权限。

### 6.4 Codex 可观察的权限门

当前可观察的权限不是一个布尔开关，而是多层门共同决定：

| 权限门   | 可观察行为                                                                             |
| -------- | -------------------------------------------------------------------------------------- |
| 工具暴露 | 只有宿主和当前指令提供的工具，模型才可能调用；不存在的工具不能靠 prompt 创造           |
| 用户意图 | `create_thread` 只应在用户明确要求新任务时调用；分享、归档、handoff 等也应遵循明确授权 |
| 对象访问 | `threadId`、`hostId`、project/source 可访问性决定目标是否能被列出、读取或继续          |
| 执行配置 | sandbox、permission profile、approval policy 决定目标 Thread 内部的工具/文件能力       |
| 环境边界 | worktree、checkout、host 和 workspace roots 决定命令和文件实际作用范围                 |
| 数据信任 | title/summary 等目录字段按不可信数据处理，不能直接当成要执行的指令                     |

协同 Agent 可以发 follow-up，但消息仍只是进入目标 Thread 的用户可见输入；目标会在自己的
指令、权限、sandbox 和执行环境中重新判断并执行。发送者不能借消息绕过目标的权限边界。

## 7. EKO 功能设计

以下是 EKO 的产品功能设计，不描述当前实现状态。

### 7.1 候选方案与决策

| 方案                                     | 优点                         | 主要问题                                           | 决策               |
| ---------------------------------------- | ---------------------------- | -------------------------------------------------- | ------------------ |
| 所有会话共享完整上下文                   | 看似无需发现和消息           | 上下文爆炸、隐私泄露、并发写入和恢复边界不清       | 拒绝               |
| 只使用任务内 Subagent                    | 实现简单、父任务容易汇总     | 不支持用户独立创建、长期可见、可恢复的平级会话     | 拒绝作为唯一方案   |
| 纯中心化固定调度器                       | 顺序可预测                   | 开放任务适应性差，模型无法动态发现新依赖和请求帮助 | 仅用于运行时硬约束 |
| 独立会话 + 目录 + 消息 + wait + 混合调度 | 隔离清晰、可恢复、可动态并行 | 需要摘要、receipt、游标和权限设计                  | **采用**           |

采用方案的核心取舍是：运行时拥有身份、权限、消息、事件和终态事实；模型拥有拆分、选择、
协商和动态调度决策。两者缺一不可。

### 7.2 产品目标

EKO 应支持用户把大型任务拆成多个独立会话 Agent，再创建一个总协调任务，让它们：

- 自动发现属于同一协作组的会话；
- 查看安全的目标、进度和完成证据；
- 通过持久消息相互请求、汇报和纠偏；
- 在依赖满足时并行，在集成阶段自动收缩并行度；
- 重启后继续等待或恢复，而不是丢失协作关系；
- 在 GUI、TUI、CLI/JSONL 和 channel 中保持功能对等。

### 7.3 核心产品对象

| 对象                   | 作用                                                                                  |
| ---------------------- | ------------------------------------------------------------------------------------- |
| `AgentAddress`         | 精确定位一个会话 Agent：scope + conversation ID                                       |
| `AgentEndpoint`        | 可发现的安全元数据：标题、摘要、状态、最近更新时间、能力标签                          |
| `CoordinationGroup`    | 协调者、成员、角色、可见性和策略的持久关系                                            |
| `CoordinationTaskView` | 对既有 TaskRun graph 的协调投影，不拥有第二套 task store 或状态机                     |
| `AgentMessage`         | 有 `message_id`、目标、来源、correlation/causation、正文和附件引用的消息              |
| `DeliveryReceipt`      | persisted/claimed/drained/turn-settled/failed 等消息交付事实                          |
| `GoalSnapshot`         | 目标的有界、可投影版本，不暴露完整隐藏提示词                                          |
| `ProgressSnapshot`     | 当前阶段、完成项、阻塞项、下一步和证据引用                                            |
| `CoordinationEvent`    | discovered、message_persisted、agent_started、agent_completed、needs_attention 等事件 |

EKO 的会话 Agent、任务运行和 Subagent 仍然使用既有产品术语，不新增第二种执行角色。
`CoordinationTaskView` 只能引用 `run_id/task_id/plan_revision` 并投影状态；它不得再次拥有依赖
DAG、ready frontier、重试、取消或完成判定。

### 7.4 模型可调用工具

EKO 可以提供以下模型工具；工具名是功能设计，最终 wire name 由实现阶段确定：

| 工具              | 必填参数                                                       | 触发策略                         |
| ----------------- | -------------------------------------------------------------- | -------------------------------- |
| `agent_list`      | 可选 `scope`、`group_id`、`status`、`limit`                    | 初始发现或目录刷新，不做高频轮询 |
| `agent_inspect`   | `agent_address`、可选 `cursor`、`detail_level`                 | 对候选任务做目标/进度确认        |
| `agent_message`   | `to`、`text`、可选 `correlation_id`、`reply_to`、`attachments` | 委派、纠偏、询问、主动汇报       |
| `agent_wait`      | `targets`、可选 `after_cursor`、`timeout_ms`                   | 等待完成、失败或需要人工处理     |
| `agent_spawn`     | `goal`、`scope`、`execution_policy`、可选 `worktree`           | 明确需要新增执行会话时           |
| `agent_interrupt` | `target`、`attempt_id`、`reason`                               | 只中断精确执行尝试               |
| `agent_resume`    | `target`、`resume_policy`                                      | 依赖变化或用户授权后恢复         |
| `agent_handoff`   | `target`、`destination`、可选 `follow_up`                      | 迁移宿主或 worktree              |
| `agent_group`     | `create/list/update/delete` 参数                               | 管理长期协作组和成员关系         |

这些工具应返回短摘要和稳定 receipt；大结果通过 artifact/reference/cursor 读取，不直接
塞回协调 Agent 的上下文。

### 7.5 协调策略

EKO 默认采用“模型决策 + 运行时约束”的混合方式：

1. 模型根据目标和摘要决定是否拆分、询问、等待或合并；
2. 运行时强制目标地址、依赖、并发上限、取消、消息幂等和权限边界；
3. 任务图、消息 journal 和终态 receipt 是事实；模型最终 prose 只是摘要；
4. 协调器可以动态唤醒成员，但不能绕过依赖门或安全点直接覆盖结果。

推荐默认调度：

```text
规划
  -> 独立任务 fan-out
  -> 结果/提交/证据收集
  -> 依赖 barrier
  -> 单一集成 owner
  -> 定向 reviewer 唤醒
  -> 完成证据与最终报告
```

如果仍存在互不依赖的任务，协调器应继续并行；如果所有任务都触及同一权威边界，应主动
收缩到一个集成 owner，而不是维持表面上的多 Agent 活跃。

### 7.6 可见性设计

EKO 建议提供三档可见性：

| 档位        | 可见范围                                    | 默认用途             |
| ----------- | ------------------------------------------- | -------------------- |
| `app`       | 当前用户账户/App 内的安全元数据             | 用户总览和跨项目协调 |
| `workspace` | 同一 workspace/project 的元数据、摘要和消息 | 默认自动协同范围     |
| `group`     | 明确加入 CoordinationGroup 的成员           | 高信任、长期协作     |

可见性是读权限，不自动授予执行权限。每个返回对象必须携带 `scope`、`owner`、
`visibility` 和 `redaction` 信息，使 UI 和模型知道哪些字段被省略。

推荐默认：**workspace 可见 + app 级用户手动发现**。不建议默认让任意项目的 Agent 互相
读取完整目标和历史；跨 workspace 协同应通过显式 group、用户确认或一次性邀请打开。

### 7.7 消息与回执

EKO 必须区分“消息已可靠保存”“输入已进入目标上下文”和“目标 turn 已完成”。建议投递事实为：

```text
Persisted
  -> Claimed(receiver generation / attempt)
  -> AcceptedByMailbox
  -> DrainedIntoContext
  -> TurnSettled(completed | failed | cancelled | dropped)

任一 pre-drain 阶段
  -> Failed(retryable | terminal)
```

各边界含义：

| 边界                 | 能证明什么                                    | 不能证明什么         |
| -------------------- | --------------------------------------------- | -------------------- |
| `Persisted`          | App journal 已拥有消息，重启后不会遗失        | 目标已加载或看到消息 |
| `Claimed`            | 某个精确 receiver generation/attempt 正在处理 | 已进入模型上下文     |
| `AcceptedByMailbox`  | 目标 runtime 的真实 mailbox 已接受            | 已发生下一次模型采样 |
| `DrainedIntoContext` | 消息已被插入目标 ContextManager/turn input    | turn 成功、任务完成  |
| `TurnSettled`        | 该 owning turn 已有 typed terminal            | 业务验收已经通过     |

框架现有 tracked steering 已提供 `Accepted -> Drained -> TurnSettled` 的真实 turn 边界；应用
层 durable journal 负责 `Persisted/Claimed`、重试和 boot reconciliation。应用不得从 UI
渲染、最后一条文本或“Agent 当前 idle”反推 drain。

要求：

- `message_id` 幂等；相同消息重复提交返回原 receipt；
- `correlation_id` 关联一次协作请求，`causation_id` 指向触发消息；
- 目标使用精确 `AgentAddress`，不能只用 title 或 cwd；
- 先产生 `Persisted`，再尝试 claim 或唤醒目标；
- 目标不可用时进入有界重试和 backoff；
- restart 后从 journal 恢复 persisted/claimed/mailbox-accepted 等未结算状态；
- `DrainedIntoContext` 必须由目标 turn safe point 或 tracked receipt 确认；
- `TurnSettled` 与业务完成分离；TaskRun completion gate 仍验证 artifact、测试和 postcondition；
- receipt 绑定 receiver generation/turn incarnation，旧 attempt 不能结算新 attempt；
- 结果报告和请求消息都可以是幂等的。

### 7.8 会话、执行和文件隔离

每个会话 Agent 至少隔离：

- 对话 transcript 和模型上下文；
- 当前 turn、goal、计划和任务结果；
- provider/model/thinking profile；
- tool permissions、approval policy 和 sandbox；
- worktree、文件所有权和写入锁；
- 事件游标和消息收件箱。

共享的只有明确投影的数据：AgentEndpoint、GoalSnapshot、ProgressSnapshot、artifact 引用、
DeliveryReceipt 和 CoordinationEvent。完整 prompt、长工具输出和隐藏推理默认不共享。

### 7.9 生命周期和恢复

EKO 应为协同组提供：

1. `start`：创建或绑定协调任务和成员；
2. `discover`：建立成员目录快照；
3. `dispatch`：发送带 correlation 的消息或启动成员；
4. `wait`：使用游标等待事件；
5. `reconcile`：比较 journal、成员状态和实际提交/产物；
6. `pause` / `resume`：在安全点暂停和恢复；
7. `interrupt`：精确中断某个 attempt；
8. `complete`：验证所有依赖、结果和完成证据后结算；
9. `recover`：启动时重建目录、收件箱、等待项和未结算 receipt。

不能用当前 UI focus、某个 Agent 是否还在内存或模型最后一句话推断协同状态。

### 7.10 Surface 对等

GUI、TUI、CLI/JSONL 和 channel 都应提供同一组能力：

- 列出协作组和成员；
- 查看目标/进度/阻塞原因；
- 发送消息、follow-up、interrupt、resume；
- 显示 persisted/claimed/mailbox-accepted/drained/turn-settled/failed receipt；
- 显示依赖图和当前 ready frontier；
- 查看完成证据和 artifact；
- 恢复或关闭协作组。

差异只在输入和渲染方式，不得因为某个 surface 当前没有 UI 就删除核心能力。

### 7.11 删除 mode 后如何做路由

通信和 turn admission 不需要 Chat/Task/Auto 这类 mode 状态机。删除 mode 后，决策只依赖
显式意图和可观察运行时事实：

```text
用户输入
  -> 目标是谁？Root Thread / App peer / Subagent
  -> 当前是否有 active regular turn？
  -> 是追加信息，还是要求 idle Agent 开始新工作？
  -> 是否需要 TaskRun graph、依赖、持久恢复和 completion gate？
  -> 调用 steer / message / follow-up / task_execute 中唯一匹配的入口
```

对应规则：

- active regular turn 的用户追加走 tracked steer；
- idle Root Thread 的用户输入启动新 turn；
- queue-only Agent 消息走 message；
- 要求已有 Subagent 开始下一项工作走 follow-up；
- 需要依赖 DAG、并发、恢复和验收的工作进入既有 TaskRun graph；
- 普通对话不伪造 TaskRun；
- 不用一个 `mode` enum 同时决定工具可见性、路由、UI、持久化和权限。

这与 Codex 的安全边界一致：判定发生在目标 active-turn/mailbox 状态上，不发生在一个全局
“当前处于什么模式”的产品标签上。

### 7.12 Todo、TaskRun 与 Subagent 的单一权威

EKO 的任务关系继续只有一条权威链：

```text
TaskRun(revision)
  -> PlanTask(DAG node)
  -> SubagentRun(execution attempt)

TaskPlan = 可编辑、可审阅的版本化 artifact
TodoItem = TaskRun graph 的 UI/TUI/CLI 投影
AgentMessage = 引用 run/task/attempt 的通信事实
```

通信层不得新增 `CoordinationTaskStore`、第二个 Todo store、第二个 Plan executor 或第二套
ready-frontier 算法。一次 follow-up 若改变了正式任务关系，协调者必须通过既有
`task_update` 提交新 revision；仅发送一条文本不能静默改写 TaskRun 权威图。

反过来，TaskRun 状态变化也不能假装消息已经交付。任务图负责“应该做什么及依赖是否满足”，
mailbox/receipt 负责“指令是否被目标接收和消费”，SubagentRun terminal 负责“一次执行如何
结束”，completion gate 负责“结果是否被接受”。这些事实可以关联，但不能互相替代。

## 8. 权限模型

### 8.1 操作权限

```text
discover < inspect < message < control < execute
```

权限应按 `actor -> target -> operation -> scope` 判定，并记录授权来源：用户、协调 Agent、
组策略或系统恢复。每次控制操作返回带 target/attempt/revision 的 receipt。

这是一组能力分类，不要求 EKO 为本地单用户场景增加多租户权限系统。默认仍是同一用户可信；
只有会造成错投、旧 attempt 干扰新执行、未提交文件覆盖或密钥泄露的本地风险需要硬边界。

### 8.2 本地个人助理取舍

EKO 是本地个人助理，不需要照搬多租户网络服务的权限体系。仍然必须防止：

- 用户无意覆盖其他会话的未提交改动；
- 旧 attempt 覆盖新 attempt；
- 跨 workspace 错投消息或结果；
- 隐藏密钥和完整 prompt 进入协同摘要；
- 消息重复造成重复执行。

默认只加这些本地场景成立的保护，不把用户主动使用的 terminal、MCP 或文件选择器套上
不必要的 Agent 自动权限门控。

## 9. 数据与事件设计

建议采用 append-only journal + 有界 projection：

```text
coordination-events.jsonl   事实事件
coordination-index.json     可丢弃目录索引
messages/<address>.jsonl    目标收件箱和 delivery facts
artifacts/                  大结果和证据
```

索引可重建，journal 和 artifact receipt 才是事实。任何 projection 都必须带：

- `schema_version`；
- `event_id` / `message_id` / `attempt_id`；
- `agent_address`；
- `coordination_id` / `correlation_id`；
- `created_at` / `sequence`；
- `visibility` / `redaction`；
- `terminal_status` 和错误分类。

不把完整上下文复制进全局目录；摘要和结果应有字符/字节上限，长文本转 artifact 引用。

存储归属按通信范围拆分，但不能重复记录同一事实：

- TaskRun 内的 PlanTask/SubagentRun 消息与终态写入既有 TaskRuntime journal；
- Conversation/App peer 协调写入应用层 conversation coordination journal；
- framework tracked steer receipt 只提供实时生命周期信号，由应用 journal 选择需要持久化的
  边界；
- `coordination-index.json`、Todo 和 UI activity 都是可重建 projection；
- EKO 使用文件或内存，不为这套协同引入 SQLite。

## 10. 实施边界

### 10.1 应用层

EKO 应拥有：

- app/workspace/group 可见性策略；
- conversation Agent 目录和产品标题/摘要；
- 协调组、协作请求和用户确认；
- 文件/worktree 所有权、review/merge policy；
- GUI/TUI/CLI/channel 投影；
- durable mailbox、delivery receipt 和 boot reconciliation；
- 协调器的 fan-out/barrier/fan-in 策略。

### 10.2 通用框架层

只有在多个复用方都需要时，才下沉：

- 稳定 Agent/Subagent identity 原语；
- 通用消息 envelope、取消、超时和 typed terminal；
- 通用 DAG、claim、retry、checkpoint 和事件流；
- 通用 execution isolation/provider 与结果合同。

框架不应知道 EKO 的 workspace UI、review、worktree 合并策略、产品角色名称或可见性默认值。

### 10.3 实现前分层结论与适配边界

| 层              | 权威职责                                                                                                           | 禁止拥有                                                       |
| --------------- | ------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------- |
| 通用 framework  | active-turn mailbox、tracked steer、通用 Agent/Subagent identity、消息 envelope、typed terminal、DAG/取消/超时原语 | EKO workspace/group、review policy、UI 字段、worktree 合并策略 |
| EKO application | App/workspace 目录、durable message journal、boot reconciliation、TaskRuntime 关联、文件 ownership、surface 投影   | 第二套 ReAct loop、第二个 Task DAG、伪造 mailbox drain         |
| 薄 adapter      | `AgentMessage -> framework input` 转换，注入 run/task/attempt metadata，把 receipt 写回 journal                    | ready frontier、重试主循环、完成 gate、独立状态机              |

新增实现前必须全仓库搜索既有 `Agent::steer_input_tracked`、TaskRuntime journal、
SubagentRegistry/Executor、ChatEventLog 和 conversation store。能扩展现有权威路径就不新增平行
store/tool/executor。若 adapter 开始自行判断 ready frontier、重试或 turn terminal，说明边界
已经放错，应停止实现并重新分层。

## 11. 验收标准

### 11.1 功能

- 协调器能发现同一协作组中的所有成员，并排除无权限任务；
- 目标/进度摘要不泄露完整 prompt、密钥或隐藏推理；
- 消息具有幂等 ID、精确地址、关联关系和可查询 receipt；
- `wait` 支持游标、超时、完成和 needs-attention；
- 目标完成后协调器能收到主动报告或被动完成事件；
- 依赖收敛后并行度从 fan-out 自动缩到集成关键路径；
- GUI/TUI/CLI/channel 行为对等。

### 11.2 故障

- 目标在 persisted、claimed、mailbox-accepted、drained 任一阶段崩溃后可恢复或明确结算；
- 重复消息不产生重复执行；
- 旧 attempt 的消息、结果和 interrupt 不能影响新 attempt；
- 目标被删除或 workspace 切换时不会错投；
- 协调器重启后保留 group、目标快照、等待游标和未结算消息；
- 文件冲突被明确阻塞，不静默覆盖。

### 11.3 性能和边界

- `agent_list` 返回有界摘要，不扫描全部历史；
- `agent_inspect` 支持 cursor/detail level；
- `agent_wait` 不需要高频轮询；
- 单个协调器的关注目标数、消息长度、并发执行数和 artifact 读取量均有上限；
- 10k/100k 级历史查询不退化为从头重放全部事件；
- 长时间协同测试放在完整产品集成阶段，不阻塞前期功能开发。

## 12. 取舍与结论

最终选择不是“所有 Agent 共享上下文”，也不是“所有任务永远并行”，而是：

```text
App peer plane
  = 独立 Root Thread
  + App 目录/精确 threadId
  + bounded inspect/follow-up/cursor wait

Root tree plane
  = 共享 AgentControl
  + canonical AgentPath
  + queue-only message / trigger-turn follow-up
  + direct-parent completion / mailbox wait

每个目标 Thread 内部
  = receiver-owned start-or-steer admission
  + safe-boundary drain
  + typed turn terminal

EKO 产品可靠性
  = durable exact-address journal/receipt
  + 唯一 TaskRun graph/Todo projection
  + worktree/file ownership
  + fan-out -> barrier -> fan-in
```

Codex 的关键能力来自控制面与模型工具的组合，而不是某一个 `list_threads` 调用。EKO 应
复用这个产品交互模型，但以自己的地址、消息、任务、权限和文件安全合同实现；不要复制
Codex 的私有实现，也不要把 EKO 产品策略污染进通用框架。

这套设计值得保留的核心不是工具名称，而是六条分离原则：

1. **身份与执行分离**：Thread/Agent 可长期存在，turn 是一次执行；
2. **地址与上下文分离**：精确寻址不等于共享 prompt；
3. **接受与消费分离**：mailbox accepted 不等于 context drained；
4. **消费与完成分离**：drained 不等于 turn 或业务成功；
5. **消息与调度分离**：queue-only message 不擅自唤醒，follow-up 才表达新工作；
6. **运行事实与 UI 分离**：事件/journal 是事实，“智能体已更新”和 Todo 都只是投影。

## 13. 参考资料与证据边界

完整的当前会话工具入口、参数、触发门和 EKO 工具层参考见
[Codex 工具能力目录](./0002-codex-tool-capability-catalog.md)。

- [OpenAI Codex app-server](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/app-server/README.md)：Thread/Turn/Item、JSON-RPC、生命周期和事件协议。
- [App-server Thread protocol](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs)：`sessionId`、`parentThreadId`、status、source 和 `canAcceptDirectInput`。
- [AgentControl](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/core/src/agent/control.rs) 与 [AgentRegistry](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/core/src/agent/registry.rs)：root-scoped 协作控制器、path/thread 索引、状态订阅和消息提交。
- [MultiAgent V2 message tool](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/core/src/tools/handlers/multi_agents_v2/message_tool.rs)：`send_message`/`followup_task` 共用路径、root target 限制和 `trigger_turn` 差异。
- [AgentPath](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/protocol/src/agent_path.rs)：canonical/relative path 解析和命名规则。
- [InputQueue](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/core/src/session/input_queue.rs)、[turn input](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/core/src/session/turn_input.rs) 与 [TurnState](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/core/src/state/turn.rs)：mailbox、steer admission 和 CurrentTurn/NextTurn 边界。
- [Subagent terminal forwarding](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/core/src/session/mod.rs)：每个 V2 child terminal turn 向 direct parent 回传有界结果。
- [Direct-input policy](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/app-server/src/request_processors/turn_processor.rs)：V2 spawned Subagent 禁止 App 直接 turn input。
- [Agent graph store](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/agent-graph-store/src/local.rs) 与 [spawn-edge migration](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/state/migrations/0021_thread_spawn_edges.sql)：持久父子拓扑。
- [Codex exec events](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/exec/src/exec_events.rs)：JSONL 事件与终态表达。
- [OpenAI Agents SDK multi-agent orchestration](https://github.com/openai/openai-agents-python/blob/main/docs/multi_agent.md)：manager/tool、handoff、代码编排和并行编排的公开对比。

`codex_app__list_threads`、`codex_app__read_thread`、`codex_app__wait_threads`、
`codex_app__send_message_to_thread` 等字段和触发语义来自 2026-08-25 当前 Codex Desktop
工具 schema 与可见调用记录。它们是 App 行为观察，不应被当成公开、长期稳定的 API；如果
未来 App schema 改变，应重新导出工具定义并更新本 ADR。

OpenAI Docs 的 Codex 页面在本次复核环境中返回 HTTP 403；本文因此只引用可核验的当前
工具 schema 和 OpenAI 官方开源仓库，不声称掌握 Desktop 私有 renderer、隐藏 prompt 或
云端调度实现。
