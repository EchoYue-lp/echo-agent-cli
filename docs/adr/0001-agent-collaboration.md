# ADR-0001：Agent 协同——Codex 机制与 EKO 功能设计

> 状态：Proposed
>
> 日期：2026-08-24
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

| 对象                | 作用                                                                                 |
| ------------------- | ------------------------------------------------------------------------------------ |
| `AgentAddress`      | 精确定位一个会话 Agent：scope + conversation ID                                      |
| `AgentEndpoint`     | 可发现的安全元数据：标题、摘要、状态、最近更新时间、能力标签                         |
| `CoordinationGroup` | 协调者、成员、角色、可见性和策略的持久关系                                           |
| `CoordinationTask`  | 协调目标、阶段、依赖和验收标准                                                       |
| `AgentMessage`      | 有 `message_id`、目标、来源、correlation/causation、正文和附件引用的消息             |
| `DeliveryReceipt`   | queued/claimed/injected/delivered/failed 等消息交付事实                              |
| `GoalSnapshot`      | 目标的有界、可投影版本，不暴露完整隐藏提示词                                         |
| `ProgressSnapshot`  | 当前阶段、完成项、阻塞项、下一步和证据引用                                           |
| `CoordinationEvent` | discovered、message_accepted、agent_started、agent_completed、needs_attention 等事件 |

EKO 的会话 Agent、任务运行和 Subagent 仍然使用既有产品术语，不新增第二种执行角色。

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

消息投递采用以下状态：

```text
accepted -> claimed -> injected -> delivered
                         └──────> failed/retryable
```

要求：

- `message_id` 幂等；相同消息重复提交返回原 receipt；
- `correlation_id` 关联一次协作请求，`causation_id` 指向触发消息；
- 目标使用精确 `AgentAddress`，不能只用 title 或 cwd；
- 先持久化 accepted，再尝试唤醒目标；
- 目标不可用时进入有界重试和 backoff；
- restart 后从 journal 恢复 queued/claimed/injected 状态；
- delivered 必须由目标 turn safe point 或可靠注入事实确认；
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
- 显示 queued/claimed/injected/delivered/failed receipt；
- 显示依赖图和当前 ready frontier；
- 查看完成证据和 artifact；
- 恢复或关闭协作组。

差异只在输入和渲染方式，不得因为某个 surface 当前没有 UI 就删除核心能力。

## 8. 权限模型

### 8.1 操作权限

```text
discover < inspect < message < control < execute
```

权限应按 `actor -> target -> operation -> scope` 判定，并记录授权来源：用户、协调 Agent、
组策略或系统恢复。每次控制操作返回带 target/attempt/revision 的 receipt。

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

- 目标在 accepted、claimed、injected 任一阶段崩溃后可恢复；
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
独立 Thread
  + App/账户级发现目录
  + project/workspace/group 过滤
  + bounded metadata inspection
  + durable exact-address mailbox
  + cursor-based event wait
  + explicit message/control permissions
  + worktree/file ownership isolation
  + fan-out -> barrier -> fan-in scheduling
```

Codex 的关键能力来自控制面与模型工具的组合，而不是某一个 `list_threads` 调用。EKO 应
复用这个产品交互模型，但以自己的地址、消息、任务、权限和文件安全合同实现；不要复制
Codex 的私有实现，也不要把 EKO 产品策略污染进通用框架。

## 13. 参考资料与证据边界

完整的当前会话工具入口、参数、触发门和 EKO 工具层参考见
[Codex 工具能力目录](./0002-codex-tool-capability-catalog.md)。

- [OpenAI Codex app-server](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)：公开的 Thread/Turn/Item、JSON-RPC、生命周期和事件协议。
- [OpenAI Codex exec events](https://github.com/openai/codex/blob/main/codex-rs/exec/src/exec_events.rs)：JSONL 事件类型和 `item/started` / `item/completed` / `turn/completed` 终态表达。
- [OpenAI Codex multi-agents v2 handlers](https://github.com/openai/codex/tree/main/codex-rs/core/src/tools/handlers/multi_agents_v2)：任务内 Subagent 的 spawn、发现、消息、follow-up、等待和中断工具边界。
- [OpenAI Agents SDK multi-agent orchestration](https://github.com/openai/openai-agents-python/blob/main/docs/multi_agent.md)：manager/tool、handoff、代码编排和并行编排的公开对比。

`codex_app__list_threads`、`codex_app__read_thread`、`codex_app__wait_threads`、
`codex_app__send_message_to_thread` 等字段和触发语义来自 2026-08-24 当前 Codex Desktop
工具 schema 与可见调用记录。它们是 App 行为观察，不应被当成公开、长期稳定的 API；如果
未来 App schema 改变，应重新导出工具定义并更新本 ADR。
